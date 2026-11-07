# tsun

A native Rust TIDAL client built with [Rune](https://github.com/anomalyco/rune). It requests hi-res lossless playback where available.

## Requirements

- Rust and Cargo
- A TIDAL account and subscription with streaming access
- On Linux, development packages for ALSA, Wayland and xkbcommon
- The FFmpeg development libraries for video: `libavcodec`, `libavformat`,
  `libavutil`, `libswresample` and `libswscale` (the `ffmpeg` package's `-dev`/`-devel`
  files; `pkg-config --exists libavcodec` must succeed). Tsun links them for H.264/AAC
  decoding; the same libraries are what the system `ffmpeg` package ships.
- A C toolchain and libclang for `ffmpeg-sys-next`'s bindgen step (`clang` on most
  distributions).

Run with `cargo run`. For hi-res, sign in using the app's authorization-code flow. After signing in in your browser, copy the final redirected URL into the app. TIDAL may show an error page at that URL; the URL itself contains the authorization code. The device flow is easier but may be limited to lossy quality.

The Music view shows personalized custom mixes, favorite tracks, and the album and playlist rows of TIDAL's home feed, with a New releases tab beside For you. Open a mix to browse its tracks. Search (the box in the top bar — which collapses to an icon button when the window is narrow, or opens as a field again on a press — or ⌘F) covers tracks, albums, artists, videos and playlists: Top results shows the strongest hit beside a short track list, the other tabs show the full lists, suggestions appear as you type, and recent searches are kept in `recent_searches.json` beside the session. Explore browses TIDAL's explore feed and new releases; Collection opens your library, and the sidebar lists your playlists. Collection's Albums, Artists, Tracks, Videos and Playlists tabs each carry a filter box and a sort dropdown (date added, name, artist, release date), and the Albums and Artists tabs show their counts. Select a track or its play button to listen, or double-click a row to start the list it sits in. The transport can pause, resume, stop, seek by clicking or dragging the progress bar, and move to the previous/next track in the current queue. Shuffle reshuffles what follows the current track and repeat cycles off → all → one; both persist, and a page's Shuffle button starts its list with shuffle mode already on. The queue button opens a panel with what is playing and what is next, where a row can be played, removed or the whole queue cleared. The speaker icon and its slider set the volume and mute; both persist. The window title shows the playing track as `Artist – Title`, or `TIDAL` when nothing plays. Artwork is fetched from TIDAL. The lyrics button shows the now-playing track's words. Open the profile menu in the top right for Settings (streaming quality, volume normalization, output device, desktop notifications) and to sign out and remove the saved session. Otherwise, tokens are stored in `$XDG_CONFIG_HOME/tsun/session.json`, or `~/.config/tsun/session.json` when `XDG_CONFIG_HOME` is unset; preferences are stored beside it in `settings.json`.

Audio is decoded and played in-process using Rust libraries. The quality indicator reports TIDAL's response, which may be lower than requested; its tooltip reports the format the device actually accepted ("Output: 96 kHz · f32 · …"). Settings' Output device picker lists the host's real output devices (including raw ALSA `hw:` ones, which give an exclusive path) as a dropdown, reading them with friendly names and the raw device name as a second line, and applies the choice immediately, at the position that was playing. A device opened at the track's own sample rate is not resampled at all; any rate conversion uses a Hann-windowed sinc interpolator, and at volume 100% with normalization off nothing is applied to the samples at all. Consecutive tracks play without a gap: the next queue track is opened and decoded before the current one ends and is swapped in on the same output stream. Auto-advance does not depend on the window being drawn: the UI hands the worker the ordered list of tracks that come next, so a natural end starts the next track — and preloads the one after it — on the worker's own thread, even while a compositor is not producing frames for a covered or hidden window (with no frames, `poll` does not run at all). Play/pause from a media key or the tray still reaches the player then; next/previous, seek and volume wait for the next frame, because they need the play queue. Start tsun with `TSUN_TRACE_POLL=1` and it says so on stderr — `UI poll stalled for Ns; window frames are not being produced (compositor throttling?)` — whenever the window stops being drawn while a track is playing. On Linux, check the default PipeWire/PulseAudio sink if it is silent; stream and device failures appear in the app.

Your own music files play beside TIDAL. Settings → Local files lists music folders — typed or picked with the desktop's folder dialog — and a background scanner walks them (hidden folders and symlinks are skipped), reading tags and artwork with the pure-Rust `lofty` crate. The index is cached in `$XDG_CONFIG_HOME/tsun/local_index.json` keyed by path, mtime and size, so a restart shows the library immediately while a refresh scan runs behind it. Local files enter the same queue as TIDAL tracks: seek, volume, mute, ReplayGain (when normalization is on), gapless playback between consecutive local files, MPRIS, desktop notifications and the player bar's quality pill (e.g. `FLAC 24-bit/96 kHz`) all behave the same. `tsun song.flac` and files dropped onto the window play as a one-off queue (file drops are delivered on X11, macOS and Windows; Wayland reports no file drags in winit 0.30), and the repo's `tsun.desktop` declares the audio MIME types for "Open With". Local rows deliberately have no TIDAL-only controls: no favourite heart (the favourites API refuses local ids), no radio, credits or TIDAL links, and no adding to TIDAL playlists. Local files are still scrobbled and shown on Last.fm through the same playback events as TIDAL tracks; a file with neither artist nor album-artist tag is skipped (with a notice) because Last.fm needs an artist. FLAC, MP3, M4A/AAC, Vorbis/OGG, WAV and AIFF are scanned and play; `.opus` is not listed because symphonia 0.5 has no pure-Rust Opus decoder.

Music videos are first-class: the Videos tab of search, a Videos carousel on artist pages and a Videos tab in Collection (favourite videos) list them with TIDAL's 16:9 thumbnails, duration, artist and quality, and the heart toggles a video favourite through the same favourites endpoints as the other kinds. Pressing a card stops the music queue and opens a window-filling player: TIDAL's playback endpoint hands back a plain `application/vnd.apple.mpegurl` master playlist (H.264 High + AAC in MPEG-TS, no `EXT-X-KEY`), the best variant up to 1080p is decoded in-process with FFmpeg's libavcodec, and each frame is streamed to Rune's pixel surface while the audio is resampled to the output device and played through cpal using the same volume/mute handle as music. A/V sync follows the audio device's clock; play/pause, the seek bar (HLS segment seek), volume/mute and the desktop media keys work while a video is open, and the MPRIS service reports the video's title, artist, length and position. Escape or the close button stops decoding and returns to the page underneath. Changing the chosen output device while a video is open re-opens its audio at the position that was playing, exactly as music does.

The player bar only appears once you have chosen a track; pressing it opens a full-screen Now Playing view — large cover art, artist/album links, a seek bar, the full transport and the synced lyrics alongside — closed with Escape or the minimize button, with its background gradient tinted by the cover's own average colour. There is no stop button; the controls inside the bar (play/pause, heart, queue, volume…) keep their own actions. A round knob marks the seek and volume values, showing a ring while its slider is hovered or dragged. Settings' "Adjust volume with the mouse wheel" is off by default, so a scroll near the volume control never changes the volume; turned on, the wheel over it (icon and slider) steps 5% at a time, or 1% with Shift. Settings can also post a desktop notification with the cover art on every track change (`org.freedesktop.Notifications`); it is off by default.

Every track row shows its number at rest and a round play button in its place while the row is hovered, selected or the track that is playing: pressing it plays the table from that row — next/previous then follow the list the row belongs to — and on the row that is playing it pauses (or resumes) instead. A double-click anywhere on a row plays it the same way.

Every track row and the album, artist, playlist and mix headers carry a "…" menu (a right-click on a row opens it too): play now, play next, add to queue, add to a playlist (or a new one), add to or remove from My Collection, go to the artist or album, track radio, copy the TIDAL link, and a track-info dialog with title, artists, album, track number, quality, duration and TIDAL's credits. On one of your own playlists the menu also removes a track from it and offers "Move up"/"Move down", which reorder it through TIDAL (optimistically, rolling back with a notice if TIDAL refuses); a queue row's menu reorders the play queue the same way. The sidebar's "Create playlist" button, that menu's "New playlist…" and a playlist's own "…" menu create, rename and delete playlists and edit their descriptions through TIDAL.

Playlists and personalized mixes are loaded one page at a time — the first 50 playlists and the first 100 tracks of a playlist or mix — while favourites (tracks, albums, artists, playlists) and album track lists are paged through in full, up to a safety cap. Legacy AES-encrypted streams are decrypted in-process. Widevine and other DRM-protected streams require a licensed CDM and are not supported.

While a track plays, tsun writes it to a local listening history (`$XDG_CONFIG_HOME/tsun/play_history.json`, 0600, the 200 most recent distinct tracks) once it has been listened to for more than 30 seconds or has reached its end; Collection's History tab and the Home page's "Recently played" row draw it, and both offer Clear history. TIDAL's own history endpoints 404 for this client, so the list never leaves the machine and never includes plays from other clients.

On Linux the app also publishes an MPRIS2 service (`org.mpris.MediaPlayer2.tsun`), so desktop media keys and panel widgets play, pause, stop, skip, seek and set the volume, and see the current track, artwork, position and length. It also installs a StatusNotifierItem tray icon with Play/Pause, Previous, Next, Show and Quit; Settings' "Close to tray" switch (off by default) makes the window's close button hide it instead of quitting, and the tray's Show brings it back. Settings are written to `$XDG_CONFIG_HOME/tsun/settings.json` (or `~/.config/tsun/settings.json` when `XDG_CONFIG_HOME` is unset) with `0600` permissions, including the startup page (Home, Collection, or the last visited page, restored from the route persisted with the rest of the state).

## Feature parity with the official TIDAL desktop client

Status reflects what the current source implements; *Partial* means the feature works with the stated limitation.

| Feature | Status | Notes |
| --- | --- | --- |
| Sign-in: device flow | Supported | `Client::login` / `Client::complete_login` — opens a verification URL and polls for the token. |
| Sign-in: browser PKCE | Supported | `Client::start_pkce` / `Client::finish_pkce` — paste the redirected URL back into the app. |
| Home / For you / mixes | Supported | `home_tracks`, `home_recommendations` (`/pages/home`) and the custom-mix gallery (`mixes`, `mix_tracks`). |
| Explore | Supported | `explore` tries `/pages/explore` then `/pages/explore_new_music`. |
| New releases | Supported | `new_releases` — the album modules of `/pages/explore_new_music`. |
| Search (tabs/suggestions/recent) | Supported | Top/Tracks/Albums/Artists/Playlists/Videos tabs (`SearchTab`), `search_suggestions`, recent searches kept in `recent_searches.json`. |
| Album page | Supported | `album` — metadata plus every page of `/albums/{id}/items`. The album's 200pt cover sits in the header, and the table's rows leave their own cover cell out (every row would repeat the picture), so titles start right after the number and the column header follows them (`Page::covers`). `GET /albums/{id}/review` (its text flattened to prose, with the source credit) sits under the title, artist and meta line as a three-line preview; a longer review gets a "More" button that opens the whole text in a modal (title, artist, source credit, Close; dismissed by its close button, the scrim or Escape), and a review short enough to fit has no button. No block at all when TIDAL has no review. Below the track list come the album's credits and "Related albums" from `/albums/{id}/similar`. Each part is drawn only when TIDAL returns it (`/similar` 404s for albums TIDAL has no neighbours for). |
| Artist page | Supported | `artist_page` — bio, top tracks, albums, EPs/singles, appears-on, music videos and similar artists. "Top tracks" shows the first ten with a "Show all" button that pages the rest through `/artists/{id}/toptracks`, using TIDAL's own `totalNumberOfItems` to decide whether more exist. |
| Playlist page | Supported | `playlist` metadata and `playlist_tracks`. |
| Mix page | Supported | `mix_tracks` — `/mixes/{id}/items`. |
| Collection (tabs/sort/filter) | Supported | Playlists/Albums/Artists/Tracks/Videos/Mixes/History tabs; local filter box and sort by date added, name, artist or release date; album and artist counts. History is local to tsun (see below). |
| Favorites | Supported | `favorite_tracks/albums/artists/playlists/videos` (paged through in full), `favorite_ids`, `set_favorite` add/remove for all five kinds. |
| Playlist create/edit/delete | Supported | `create_playlist`, `rename_playlist`, `delete_playlist`. |
| Playlist add/remove tracks | Supported | `add_to_playlist` and `remove_from_playlist` (by index). |
| Playlist track reordering | Supported | `move_playlist_item` — `POST /playlists/{uuid}/items/{index}` with a `toIndex` form body and the playlist ETag as `If-None-Match`; "Move up"/"Move down" in a row's "…" menu on your own playlists, applied optimistically and rolled back with a notice if TIDAL refuses. |
| Queue reordering | Supported | "Move up"/"Move down" in a queue row's "…" menu; the queue, its current track and the next-track preload stay consistent (`Queue::move_item`). |
| Local listening history | Supported | `POST`-free and local-only: tsun records what *it* plays — past 30 s or played to the end — in `$XDG_CONFIG_HOME/tsun/play_history.json` (0600, 200 most recent distinct tracks), shown in Collection's History tab and as the Home page's "Recently played" row, each with Clear history. |
| Local files (scan/play) | Supported | Settings → Local files lists music folders (typed path validated as an existing directory, or the native folder dialog / XDG FileChooser portal) and scans them on a background thread with `lofty` tags (title, artist, album artist, album, track/disc, year, duration, embedded cover, ReplayGain, sample rate/bit depth/codec). FLAC, MP3, M4A/AAC, Vorbis/OGG, WAV and AIFF play through the same player/queue as TIDAL (seek, volume, mute, normalization, output device, bit-perfect rate negotiation, gapless between local files, MPRIS, notifications). The index caches in `$XDG_CONFIG_HOME/tsun/local_index.json` by path+mtime+size; `.opus` is excluded (no pure-Rust Opus decoder in symphonia 0.5). |
| Local files (browse) | Supported | A sidebar "Local files" page with Tracks / Albums / Artists / Folders tabs, a filter box and sort (artist/title/album/year), album and artist drill-down pages, Play/Shuffle, folder browsing with "Play folder". Covers come from embedded art or `folder.jpg`/`cover.jpg`/`front.jpg`, thumbnailed to `$XDG_CACHE_HOME/tsun/local-covers/` (480px JPEG) and drawn through the same artwork pipeline, MPRIS (`file://` artUrl) and notifications. Rows reuse the shared track table without TIDAL-only actions: no heart (favourites refuse local ids), radio, credits, TIDAL links or TIDAL playlist adds; the "…" menu offers play next / add to queue / Show in file manager / Copy file path. |
| Open local files | Supported | `tsun song.flac` plays the named files as a one-off queue; dropping files onto the window does the same (X11/macOS/Windows — Wayland reports no file drags in winit 0.30); `tsun.desktop` in the repo root declares the audio MIME types for desktop "Open With" (nothing is installed outside the repo). |
| Local scrobbling | Supported | Local files emit the same playback events as TIDAL tracks, so Last.fm now-playing/scrobbling and Discord presence follow them too. A file with neither an artist nor an album-artist tag is skipped with an on-screen notice (Last.fm requires an artist); the "love on favourite" sync therefore only applies to TIDAL tracks. |
| Tray / close-to-tray | Supported | A StatusNotifierItem tray (`Tray`, `com.canonical.dbusmenu`) with Play/Pause, Previous, Next, Show and Quit; the "Close to tray" setting (off by default) makes the window's close button hide it instead of quitting. Needs a StatusNotifierWatcher (KDE, most bars); the icon simply appears when one starts. |
| Track context menu | Supported | The "…" menu (or right-click): play now, play next, add to queue, add to playlist (or a new one), favourite toggle, go to artist/album, track radio, copy link, track info; on your own playlists also Edit/Delete, "Remove from this playlist" and "Move up"/"Move down"; on a queue row "Move up"/"Move down". |
| Credits | Supported | `track_credits` — `GET /tracks/{id}/credits` in the track-info dialog; the album page lists `GET /albums/{id}/credits` below the track list. |
| Lyrics (synced) | Supported | `lyrics` — plain text plus time-synced `subtitles`; the panel highlights the current line. |
| Track / artist radio | Supported | `track_radio`, `artist_radio`. |
| Queue / shuffle / repeat | Supported | `Queue` with shuffle (reshuffles what follows the current track) and repeat off → all → one; both persist. |
| Gapless playback | Supported | The next queue track is preloaded and swapped in on the same output stream; the worker thread decides and does it, so it does not wait for a frame. |
| Seek / volume / mute | Supported | Progress-bar seek (click or drag), volume slider and mute; a round knob marks each slider's value and shows a ring while the pointer is on the slider (a drag holds it there). Volume and mute persist. |
| Output device | Supported | Settings' "Audio output" category holds a dropdown of the host's real devices (including raw ALSA `hw:`), a Refresh button and the live output format; the choice is applied at the position that was playing. |
| Settings page | Supported | A category navigation — General, Playback, Audio output, Library, Integrations and Account — beside one content column of grouped settings, folding to a strip of category chips when the window is too narrow for both (the same laid-out-width breakpoint the rest of the app uses). Rows are title + description with the control on the right: real switches with a sliding knob for booleans, a segmented control for the startup page, and option tiles for the streaming quality. It covers the startup page (Home / Collection / last visited — persisted in `settings.json` and honoured on launch), desktop notifications, "Adjust volume with the mouse wheel" (off by default, so a scroll near the volume bar never touches it; 5% steps, 1% with Shift), streaming quality, volume normalization, the output device, the local-file folders, the Discord and Last.fm integrations and signing out. Download/Offline, an explicit-content-badge toggle and Check for updates are deliberately absent: they would have no real behaviour in this client. |
| Bit-perfect / native sample rate | Partial | The device is opened at the stream's own rate and channel count when it advertises them, and nothing is resampled when they match; the OS mixer can still alter the output. |
| ReplayGain normalization | Supported | TIDAL's reported ReplayGain is applied when normalization is on (`set_normalize`). |
| Quality selection (Low/High/Lossless/Hi-Res) | Supported | `Quality::{Low, High, Lossless, HiResLossless}`; device-flow tokens are capped at HIGH for many accounts, while the PKCE client can unlock LOSSLESS and HI_RES_LOSSLESS. Lossless preferences resolve through the v2 `trackManifests` endpoint (`FLAC_HIRES`, `FLAC`, `AACLC`, unencrypted DASH converted to HLS), because the v1 `playbackinfopostpaywall` endpoint answers AAC (`HIGH`) at every tier for many tracks that do have FLAC; v1 stays as the fallback and for the AAC tiers. A rate-limited or failed request is retried with a short backoff on the same tier instead of quietly dropping to a lower one. |
| MPRIS / media keys | Supported | `org.mpris.MediaPlayer2.tsun` — play/pause/stop, next/previous, seek and volume, plus metadata, artwork, position and length; while a video is open the media keys control the video. Play/pause (media key or tray) goes straight to the player thread, so it still works while the window is hidden or not being drawn; next/previous, seek, volume and stop wait for the next frame, because they need the UI's queue and state. |
| Notifications | Supported | Freedesktop `org.freedesktop.Notifications` on track change, with cover art; off by default. |
| Last.fm scrobbling | Supported | The user's own API key and shared secret (from last.fm/api/account/create) drive `auth.getToken` → browser authorization → `auth.getSession`; `track.updateNowPlaying` on a track's start and `track.scrobble` once a play passes the official threshold (longer than 30 s, and half the track or four minutes, whichever comes first, counting real pause/seek-aware played time). Failed submissions retry from an on-disk queue with backoff; invalid ones are dropped. Local files scrobble too when they carry a title and artist. All network runs on its own thread. |
| Discord Rich Presence | Supported | `discord` — tsun's own IPC client for the Discord desktop app (`$XDG_RUNTIME_DIR/discord-ipc-N`, the Flatpak and Snap paths, `/tmp`), on its own thread with reconnect. It works out of the box under tsun's built-in Discord application (public Application ID `1280598966471491594`); the Settings card also takes an optional own Application ID (an override) and an activity name. Presence: title, artist and album, TIDAL cover art in the activity's `assets`, an elapsed/remaining bar and a "Listen on TIDAL" button (which Discord shows to other people only); each part can be switched off. Off by default. |
| Full-screen Now Playing | Supported | Overlay with large cover art, links, a seek bar, the full transport and synced lyrics; closed with Escape or the minimize button, background tinted by the cover's average colour. |
| Music videos (browse) | Supported | Videos tab in search, Videos carousel on artist pages, Videos tab in Collection; 16:9 thumbnails with duration and quality; favourite hearts. Data from `/search` (`VIDEOS`), `/artists/{id}/videos`, `/users/{id}/favorites/videos` and `/videos/{id}`. |
| Video playback | Supported | `video_stream` resolves `/videos/{id}/playbackinfopostpaywall` (GET; `videoquality=HIGH`), whose manifest is a plain `application/vnd.apple.mpegurl` master playlist. H.264 High + AAC MPEG-TS variants (up to 1920x1080) have no `EXT-X-KEY`, so they play without a CDM. Decoded with libavcodec, audio through cpal and the shared volume handle, A/V synced to the audio clock; play/pause, seek, volume/mute, MPRIS and Escape/close. |
| Keyboard shortcuts | Supported | See [Keyboard shortcuts](#keyboard-shortcuts). |
| Window title | Supported | `Artist – Title` while a track is playing, `TIDAL` otherwise. |
| Window controls | Supported (partial) | The system title bar is removed (`WindowOptions::borderless`); the top bar drags the window (and double-click maximizes) and ends with the app's own round buttons, in the same row as the search box and account button: maximize/restore, close (honours "Close to tray") and — only when the tray icon exists — "minimize to tray". Rune does not minimize borderless windows, so there is no plain minimize button; resizing is left to the compositor (Rune has no edge-resize for borderless windows), and the maximize button's icon does not change when the compositor maximizes the window itself. The bar is responsive to the width of the main area: the search box shrinks with it, and below that collapses into a round search-icon button that expands the full field on a press (focusing it, and folding back on Escape or on losing focus while empty); the back/forward buttons give up their room next, so the window controls and the account button never overflow. The account button is the avatar (or a placeholder person icon) inside a subtle outlined pill with a chevron, and opens the profile menu. The full-screen Now Playing and video views cover the bar and close with Escape. |

### Not supported

- **Widevine / DRM-protected streams** — Dolby Atmos and Sony 360 tracks, and some HI_RES_LOSSLESS manifests, are served as DRM-protected streams (EMU `application/vnd.tidal.emu` or encrypted HLS); tsun decrypts only the legacy AES-128 stream format in-app and returns a clear error for anything that needs a licensed CDM. Music videos are plain HLS today; a video manifest that is Widevine/PlayReady is rejected with the same explanation rather than guessed at.
- **Offline downloads / sync** — no download or sync code; playback is streaming only.
- **TIDAL Connect / casting** — no Connect or cast transport is implemented.
- **TIDAL listening history** — TIDAL's own history endpoints return 404 for this client's credentials, so the History tab and the Home "Recently played" row show **only what tsun itself has played**, recorded locally in `play_history.json`. Nothing is sent to TIDAL and plays from other clients never appear.
- **Artist "similar" / artist credits and roles** — `/artists/{id}/similar` answers 404 ("Similar artists for artist […] not found") for every artist probed, including Daft Punk; there is no artist-credits endpoint in the v1 API, and artists carry no roles or links, so the artist page shows only what TIDAL actually returns (bio, top tracks, albums, EPs/singles, appears-on, videos). `/albums/{id}/similar` does work and is used on the album page.
- **Podcasts / other editorial surfaces** — only the `/pages/*` modules the v1 API exposes (home, explore, new music) are consumed; podcast and other editorial endpoints are not used.
- **Opus local files** — `.opus` files are not scanned: symphonia 0.5 has no pure-Rust Opus decoder in this build. Convert them to another format, or wait for an Opus decoder.
- **Local favourites / playlists** — local files are not TIDAL objects, so they cannot be hearted, put in TIDAL playlists or used for radio. TIDAL's favourites endpoints refuse local ids by design (they never leave the app).
- **Light theme** — a single dark palette (`theme::palette`); there is no theme switch.

## Discord Rich Presence

Off by default, and it needs no setup beyond the switch: tsun publishes the presence under its
own Discord application (**built-in Application ID `1280598966471491594`** — a client id is
public, not a secret), so there is nothing to register and no key to paste. Turn on **Show what
I'm listening to** in Settings' Discord card and the presence appears. The card's status line is
live: *Not running* (Discord is closed or not installed), *Connecting…*, *Connected as <your
Discord account>* or *Error* (with Discord's own words). Starting the Discord app later, or
restarting it, reconnects on its own. The **Application ID** field is an optional override for
people who want their own application — its name, icon or uploaded art assets (create one at
[discord.com/developers/applications](https://discord.com/developers/applications) and paste its
17–20 digit id); an empty field means the built-in application, and clearing a saved override
goes back to it.

While it is on, the presence shows the track title, the artist, the album, the TIDAL cover art,
an elapsed/remaining bar that follows seeking, pausing and resuming, and a **Listen on TIDAL**
button linking to the track's TIDAL page. **Discord never shows your own buttons to you** — only
to the people who look at your profile (or to a second account), so test the button that way
rather than from your own client. The three switches turn the art, the bar and the button off
individually and take effect immediately. The **Activity name** is what Discord writes after
"Listening to" and next to your avatar for everyone; it defaults to `{title} – {artist}` (so it
reads "Listening to Around the World – Daft Punk"), is a template that also understands `{album}`,
and falls back to the application's own name when it is left empty. Local files are published the same way but
never get a TIDAL button, since they have no TIDAL page. Discord has no "paused" flag, so a paused
track is sent without timestamps and with *Paused* in the state line rather than a bar that keeps
counting. Discord allows about five presence updates per twenty seconds, so quick skips are
coalesced and the track that is playing last always wins.

Privacy: this tells Discord what you are listening to, and (for TIDAL tracks) links to it —
anyone who can see your Discord profile can see it, exactly as with the official integrations.
tsun talks only to the Discord client running on this machine, through its local socket
(`$XDG_RUNTIME_DIR/discord-ipc-N`, or the Flatpak/Snap or `/tmp` equivalent); it never talks to
Discord's servers itself, and it publishes nothing at all until a track is playing. Turning the
switch off, signing out or quitting tsun clears the presence immediately.

## Last.fm scrobbling

tsun can scrobble what you play to [Last.fm](https://www.last.fm). It ships no API key of its
own, so you register one: open
[last.fm/api/account/create](https://www.last.fm/api/account/create), create an application (any
name and description; tsun only talks to the API, never to a redirect URL), and copy its **API
key** and **shared secret**. Paste both into Settings' **Last.fm scrobbling** card and press
**Connect**; tsun opens Last.fm's authorization page in your browser, you approve tsun there, and
then press **I've authorized** to finish. The card shows **Connected as <your username>** once it
worked, and **Disconnect** forgets the session (your key and secret are kept, so reconnecting is
one click).

The key, the secret, the session key and the switches live in
`$XDG_CONFIG_HOME/tsun/lastfm.json` (0600), never in `settings.json` and never sent anywhere but
Last.fm's API.

While connected, three switches — all on except loving — control what is sent:

- **Scrobble** — send a play once it has really been listened to: the track must be longer than
  30 seconds and must have played for at least half its length or four minutes, whichever comes
  first. Pausing stops the clock and seeking neither adds nor removes time; a repeat plays and
  scrobbles again. A track only counts once per play.
- **Send now playing** — tell Last.fm what is playing as soon as a track starts
  (`track.updateNowPlaying`).
- **Love tracks on Last.fm when I favourite them on TIDAL** — mirror the heart: hearting a TIDAL
  track sent `track.love`, un-hearting removes it (`track.unlove`). Local files have no TIDAL
  favourite, so they are never loved.

Scrobbles are queued on disk (`lastfm_queue.json`) and retried with a growing backoff when the
network or Last.fm is down, so a train tunnel does not lose your plays; submissions Last.fm
rejects as permanently invalid (bad session, bad parameters, suspended key, …) are dropped
instead of retried forever, and the card says when plays are waiting. Everything — authorization
and every submission — happens on its own thread, so the window never stalls.

Local files scrobble too: their title, artist, album and duration come from the file's tags, and
the artist falls back to the album artist when the artist tag is missing. A file with neither tag
has no artist to scrobble, so it is skipped and the app says so rather than guessing one.

## Performance

Every long list in the app is one virtual list (Rune's `El::virtual_rows`): the Collection's
tracks, albums, artists, playlists, videos and history, the album, playlist, mix, artist-top-track
and search result tables, the local library's tabs and its album, artist and folder pages, and the
queue panel. Layout builds only the rows near the viewport (a viewport either side, filled over a
few frames), so a commit costs one key check per row plus the rows it mounts, however long the
list is. The page's own blocks — its header, the tab strip, the filter toolbar, a "Top tracks"
heading, the album's extras — are rows of that same list, so they scroll away with the content as
before, and the artist page's album, video and similar rows page their cards instead of building
them all.

Nothing off screen is built, requested or decoded:

- **Artwork** is fetched for what a surface actually draws. Drawing a cover notes a want
  (`Ui::want_art`) whatever draws it — a track card or row, a mix or playlist tile, a sidebar
  thumbnail, the account avatar — `poll` turns those into requests, and a cover that arrives for
  a row nothing shows only lands in the cache — it does not repaint the window. Opening a page
  with thousands of tracks no longer starts thousands of downloads. Noting the first want of a
  frame also wakes the app, so the poll that turns it into a fetch always comes and a cold start
  paints its covers without the user doing anything.
- **Derived lists** — the Collection's filter and sort, the local library's, the History and the
  queue — are cached and rebuilt only when their inputs change (a filter edit, a new library, the
  queue moving on). A track list is held as one shared allocation (`Rc<[Track]>`), handed to the
  view as a pointer clone, so no commit copies the tracks.
- **Row closures** look their item up by id when they run (a double-click finds the row's place in
  its list, a row's "…" menu re-finds its playlist item), so a row that stays mounted while rows
  above it are inserted or reordered still acts on its own track.

Measured on the user's 331-track favourites list (Collection → Tracks), headless at 1280x800 with
a scroll, debug profile with `opt-level = 1` for this crate and `3` for its dependencies:

| | before | after |
| --- | --- | --- |
| animating frames, cpu p50 / p95 | 2.78 ms / 14.44 ms | 0.54 ms / 2.44 ms |
| at-rest frames, forced cpu p50 / p95 | 0.24 ms / 3.21 ms | 0.24 ms / 0.51 ms |
| layout nodes | 5037 | ~1000 (a window of rows) |
| whole 20-second headless run, wall | 80 s | 22 s |

The bundled benchmark times the same page with 350, 5,000 and 20,000 synthetic tracks
(`cargo test -- --ignored --nocapture long_list`), in the dev profile:

| tracks | first commit | idle commit (mean) | scroll step (mean) |
| --- | --- | --- | --- |
| 350 | 2.4 ms | 0.57 ms | 0.64 ms |
| 5,000 | 4.2 ms | 0.57 ms | 1.09 ms |
| 20,000 | 10.6 ms | 0.55 ms | 1.12 ms |

An idle commit (the progress tick while a track plays) and a scroll step are flat in the list
length; a first commit pays once for the whole list's row keys, which layout then reuses until the
list changes.

## Keyboard shortcuts

While no text field has focus:

| Keys | Action |
| --- | --- |
| Space | Play/pause (the video, while the video player is open) |
| Ctrl+Right / Ctrl+Left | Next / previous track |
| Ctrl+Up / Ctrl+Down | Volume up / down |
| M | Mute |
| S | Shuffle |
| R | Repeat (off → all → one) |
| Q | Queue panel |
| / | Focus the search box |
| Escape | Close the video player, full-screen Now Playing view, context menu, dialog, sort dropdown or queue panel, or leave Settings; with the collapsed search box expanded and empty, fold it back to its icon |

These work even while a text field has focus, and open the same actions from the account menu:

| Keys | Action |
| --- | --- |
| Ctrl+F (⌘F) | Focus the search box |
| Ctrl+R (⌘R) or F5 | Refresh the current page |
| Ctrl+, (⌘,) | Open Settings |
| Alt+Left / Alt+Right | Back / forward |

This is an independent client using undocumented TIDAL endpoints, not an official TIDAL application. Endpoints, client credentials, available quality, and account permissions can change without notice. The stream quality supplied by TIDAL can be lower than requested; lossless source quality does not guarantee bit-perfect output through the operating system's audio mixer.

TIDAL's official SDK documentation states that full-length playback requires device login or authorization-code access, while publicly supported client-credentials access is limited to previews. TIDAL's [developer terms](https://developer.tidal.com/documentation/guidelines/guidelines-developer-terms) restrict third-party playback to its official Player SDK. This client uses unofficial endpoints and its own player; review the applicable terms before using or distributing it.

## References

- [Rune](https://github.com/anomalyco/rune)
- [python-tidal](https://github.com/tamland/python-tidal), for the device authorization flow
- [tidalamp](https://github.com/wh01s17/tidalamp), for lossless playback and DASH considerations
- [streamrip](https://github.com/nathom/streamrip), for the legacy encrypted stream format
- [TIDAL's Player SDK](https://github.com/tidal-music/tidal-sdk-ios/tree/main/Sources/Player), for the official playback interface and its access restrictions
