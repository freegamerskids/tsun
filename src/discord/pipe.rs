//! Discord's IPC transport on Windows: the named pipe `\\.\pipe\discord-ipc-N`.
//!
//! The pipe is opened as an ordinary synchronous file. A synchronous handle has no read
//! timeout, so [`PipeStream::read`] asks `PeekNamedPipe` whether bytes are waiting and only
//! then reads, which gives the connection loop the same "nothing yet" answer a Unix socket's
//! read timeout does.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use windows_sys::Win32::System::Pipes::PeekNamedPipe;

/// How often a waiting read looks at the pipe again.
const PEEK_INTERVAL: Duration = Duration::from_millis(5);

pub(super) struct PipeStream {
    file: File,
    read_timeout: Duration,
}

impl PipeStream {
    /// Opens the pipe at `path`; a read with nothing waiting gives up after `read_timeout`.
    pub(super) fn connect(path: &Path, read_timeout: Duration) -> io::Result<PipeStream> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        Ok(PipeStream { file, read_timeout })
    }

    /// The number of bytes that can be read without blocking.
    fn available(&self) -> io::Result<u32> {
        let mut available = 0u32;
        // SAFETY: the handle is this open file's, and every pointer is either null (no data is
        // copied out) or points at a live `u32`.
        let ok = unsafe {
            PeekNamedPipe(
                self.file.as_raw_handle() as _,
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                &mut available,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(available)
    }
}

impl Read for PipeStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let deadline = Instant::now() + self.read_timeout;
        loop {
            match self.available() {
                Ok(0) => {}
                Ok(_) => return self.file.read(buf),
                // Discord closed its end: end of stream, as a socket reports it.
                Err(error) if error.kind() == io::ErrorKind::BrokenPipe => return Ok(0),
                Err(error) => return Err(error),
            }
            if Instant::now() >= deadline {
                return Err(io::ErrorKind::TimedOut.into());
            }
            thread::sleep(PEEK_INTERVAL);
        }
    }
}

impl Write for PipeStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.file.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}
