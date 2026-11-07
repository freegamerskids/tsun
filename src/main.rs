//! TIDAL desktop client UI.
//!
//! The TIDAL backend is blocking (device auth polling and search), so it lives on one
//! worker thread that owns `Client` (see [`worker`]). The UI ([`app`]) sends commands over a
//! channel and drains worker events in `AppSpec::poll`, woken by the `Waker` Rune hands to
//! `AppSpec::setup`. Nothing network- or playback-related ever blocks the frame.
//!
//! Layout: [`theme`] holds the palette/styles/icons, [`widgets`] the shared controls, and
//! [`ui`] the view modules, one per area.

mod app;
mod art;
mod audio;
mod cover;
mod discord;
mod history;
mod history_log;
mod lastfm;
mod local;
mod media_events;
mod mpris;
mod notify;
mod playback;
mod recent;
mod settings;
mod theme;
mod tidal;
mod ui;
mod util;
mod video;
mod widgets;
mod worker;

use rune::AppSpec;

fn main() {
    rune::set_app_id("tsun");
    let spec = AppSpec::<app::Ui> {
        min_size: Some((860.0, 560.0)),
        setup: Some(app::setup),
        poll: Some(app::poll),
        on_key: Some(app::on_key),
        ..AppSpec::new("TIDAL", (1120.0, 720.0), app::init, ui::view)
    };
    // `keep_running` plus `on_close` give the window a "close to tray" mode: the close button
    // hides the window and the tray brings it back, unless the setting is off, in which case
    // `on_close` quits. `init` receives the event-loop handle so the tray can show or quit the
    // window from its own thread.
    // The system title bar is gone (`borderless`); the top bar moves the window and the app
    // draws its own window controls (`ui::topbar::window_controls`).
    let app = rune::App::new(spec)
        .window_options(rune::WindowOptions { borderless: true, ..Default::default() })
        .keep_running()
        .on_close(app::on_close);
    let handle = app.handle();
    app.init(move || app::init_with_handle(handle)).run();
}
