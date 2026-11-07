//! The Settings card for Last.fm scrobbling.
//!
//! The card owns the feature's whole user-facing side: the user's own API key and shared secret
//! (tsun ships none — they come from <https://www.last.fm/api/account/create>), the Connect /
//! "I've authorized" / Disconnect buttons, the connected-as line and the three switches. Every
//! control does something real: Connect fetches a token and opens Last.fm's authorization page
//! in the browser, "I've authorized" exchanges it for a session, and the switches are handed to
//! the scrobble thread where they gate what is sent.

use rune::{El, TextStyle, Tx, col, row, text, text_input};

use crate::app::{Notice, Ui};
use crate::lastfm::{Connection, LastFmPref};
use crate::theme::{SMALL, Palette};
use crate::ui::settings_widgets::{self as w, Tone};
use crate::widgets::{primary_button, secondary_button};

/// The Last.fm card.
pub(crate) fn lastfm_card(s: &Ui, p: Palette) -> El {
    let Some(lastfm) = &s.lastfm else { return col() };
    let (tone, label, hint) = match &lastfm.conn {
        Connection::Connected { username } => (
            Tone::Ok,
            format!("Connected as {username}"),
            "Every play past the threshold is scrobbled from here.",
        ),
        Connection::Authorizing { .. } => (
            Tone::Warn,
            "Waiting for authorization".to_owned(),
            "Approve tsun in the browser page Last.fm opened, then confirm below.",
        ),
        Connection::Disconnected => (
            Tone::Off,
            "Not connected".to_owned(),
            "Add your own Last.fm API key and shared secret, then press Connect.",
        ),
    };
    let mut card = w::card(p)
        .key("lastfm-card")
        .child(
            row().w_full().padding(14.0).child(w::card_header(
                p,
                "Last.fm scrobbling",
                "Send what you play to your Last.fm account.",
                Some(w::status_badge(p, tone, &label).key("lastfm-status")),
            )),
        )
        .child(
            row()
                .w_full()
                .px(14.0)
                .pb(12.0)
                .child(w::note(w::tone_color(tone, p), hint)),
        );
    match &lastfm.conn {
        Connection::Connected { .. } => {
            card = card
                .child(w::divider(p))
                .child(w::toggle_row(
                    p,
                    "lastfm-scrobble",
                    "Scrobble",
                    Some("Send plays once they pass half the track or four minutes."),
                    lastfm.scrobble,
                    |s: &mut Ui| s.lastfm_set_pref(LastFmPref::Scrobble),
                ))
                .child(w::divider(p))
                .child(w::toggle_row(
                    p,
                    "lastfm-now-playing",
                    "Send now playing",
                    Some("Tell Last.fm what is playing as soon as a track starts."),
                    lastfm.now_playing,
                    |s: &mut Ui| s.lastfm_set_pref(LastFmPref::NowPlaying),
                ))
                .child(w::divider(p))
                .child(w::toggle_row(
                    p,
                    "lastfm-love",
                    "Love tracks when I favourite them on TIDAL",
                    Some("Mirrors the heart: a TIDAL favourite loves the track, removing it un-loves it. Local files have no TIDAL favourite, so they are never loved."),
                    lastfm.love,
                    |s: &mut Ui| s.lastfm_set_pref(LastFmPref::Love),
                ))
                .child(w::divider(p))
                .child(
                    row().w_full().padding(14.0).justify_end().child(secondary_button(
                        p,
                        "Disconnect",
                        true,
                        |s: &mut Ui, _: &mut rune::Tx| s.lastfm_disconnect(),
                    )),
                );
        }
        Connection::Authorizing { url } => {
            let open = url.clone();
            let copy = url.clone();
            card = card
                .child(w::divider(p))
                .child(col().w_full().padding(14.0).gap(12.0).child(link_card(url, p)))
                .child(
                    row()
                        .w_full()
                        .px(14.0)
                        .pb(14.0)
                        .gap(10.0)
                        .center_y()
                        .justify_end()
                        .child(primary_button(p, "Open authorization page", true, move |s: &mut Ui, tx: &mut Tx| {
                            if let Err(error) = tx.open_url(&open) {
                                s.notice = Some(Notice::error(format!("Couldn't open the link: {error}")));
                            }
                        }))
                        .child(secondary_button(p, "Copy link", true, move |_: &mut Ui, tx: &mut Tx| {
                            tx.copy(copy.clone())
                        }))
                        .child(primary_button(p, "I've authorized", true, |s: &mut Ui, _: &mut rune::Tx| {
                            s.lastfm_confirm()
                        }))
                        .child(secondary_button(p, "Cancel", true, |s: &mut Ui, _: &mut rune::Tx| {
                            s.lastfm_disconnect()
                        })),
                );
        }
        Connection::Disconnected => {
            card = card
                .child(w::divider(p))
                .child(
                    col()
                        .w_full()
                        .padding(14.0)
                        .gap(16.0)
                        .child(key_field(s, p))
                        .child(secret_field(s, p)),
                )
                .child(
                    row()
                        .w_full()
                        .px(14.0)
                        .pb(14.0)
                        .gap(12.0)
                        .center_y()
                        .justify_end()
                        .child(
                            text("Connect opens Last.fm in your browser to authorize this app.")
                                .font(SMALL)
                                .color(p.faint)
                                .wrap(),
                        )
                        .child(primary_button(p, "Connect", true, |s: &mut Ui, _: &mut rune::Tx| {
                            s.lastfm_connect()
                        })),
                );
        }
    }
    if let Some(message) = &lastfm.message {
        card = card.child(
            text(message.clone())
                .font(SMALL)
                .color(if lastfm.error { p.danger } else { p.dim })
                .px(14.0)
                .pb(12.0)
                .wrap(),
        );
    }
    card
}

/// The API-key field.
fn key_field(s: &Ui, p: Palette) -> El {
    let field = s.lastfm.as_ref().expect("Last.fm is running");
    w::field_row(
        p,
        "API key",
        w::field_frame(
            text_input(&field.key, TextStyle::new(13.0, 400.0))
                .key("lastfm-api-key")
                .single_line()
                .grow(1.0)
                .color(p.text)
                .placeholder("API key", p.faint)
                .on_edit(|s: &mut Ui| &mut s.lastfm.as_mut().expect("Last.fm is running").key)
                .on_submit(|s: &mut Ui, _: &mut Tx| s.lastfm_save_credentials()),
            p,
        ),
        None,
    )
}

/// The shared-secret field, masked.
fn secret_field(s: &Ui, p: Palette) -> El {
    let field = s.lastfm.as_ref().expect("Last.fm is running");
    w::field_row(
        p,
        "Shared secret",
        w::field_frame(
            text_input(&field.secret, TextStyle::new(13.0, 400.0))
                .key("lastfm-secret")
                .single_line()
                .grow(1.0)
                .color(p.text)
                .placeholder("Shared secret", p.faint)
                .secure()
                .on_edit(|s: &mut Ui| &mut s.lastfm.as_mut().expect("Last.fm is running").secret)
                .on_submit(|s: &mut Ui, _: &mut Tx| s.lastfm_save_credentials()),
            p,
        ),
        Some("tsun ships no API key: register an application at last.fm/api/account/create and paste its API key and shared secret here. They are stored in lastfm.json (0600), never in settings.json."),
    )
}

/// The authorization link, shown as text the user can copy from.
fn link_card(url: &str, p: Palette) -> El {
    row()
        .w_full()
        .px(12.0)
        .py(10.0)
        .radius(8.0)
        .fill(w::recess(p))
        .border(1.0, p.border)
        .center_y()
        .child(text(url.to_owned()).font(SMALL).color(p.dim).grow(1.0).min_w(0.0).truncate_middle())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::init;
    use crate::lastfm::{LastFm, Store};
    use crate::media_events::{MediaEvent, MediaTrack};
    use crate::theme::palette;

    fn card_view(s: &Ui, _: &mut rune::Cx<Ui>) -> El {
        lastfm_card(s, palette())
    }

    fn runtime(state: Ui) -> rune::Runtime<Ui> {
        // Tall enough that every row of the card is laid out inside the window.
        let mut rt = rune::Runtime::new(state, card_view, (620.0, 1000.0), rune::Env::new(true, 1.0));
        rt.frame(0.0);
        rt
    }

    fn texts(rt: &rune::Runtime<Ui>) -> Vec<String> {
        rt.elements().into_iter().filter_map(|element| element.text).collect()
    }

    fn click(rt: &mut rune::Runtime<Ui>, key: &str) {
        let (x, y) = rt.find(key).unwrap_or_else(|| panic!("no element {key:?}"));
        rt.pointer_move(x, y);
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(0.0);
    }

    /// A client with the given store, but no thread (so the test touches neither the filesystem
    /// nor the network and cannot race another test over `XDG_CONFIG_HOME`).
    fn client(store: &Store) -> LastFm {
        LastFm::for_test(store)
    }

    /// A connected store with the default switches.
    fn connected_store() -> Store {
        Store {
            api_key: "key".to_owned(),
            secret: "secret".to_owned(),
            session_key: "session".to_owned(),
            username: "listener".to_owned(),
            ..Store::default()
        }
    }

    #[test]
    fn the_disconnected_card_asks_for_a_key_and_secret() {
        let mut state = init();
        state.lastfm = Some(client(&Store::default()));
        let rt = runtime(state);
        assert!(rt.find("lastfm-api-key").is_some(), "the API key field is there");
        assert!(rt.find("lastfm-secret").is_some(), "the secret field is there");
        let shown = texts(&rt);
        assert!(shown.iter().any(|text| text == "Connect"), "{shown:?}");
        assert!(shown.iter().any(|text| text.contains("last.fm/api/account/create")), "{shown:?}");
    }

    #[test]
    fn the_connected_card_shows_the_account_and_the_switches() {
        let mut state = init();
        state.lastfm = Some(client(&connected_store()));
        let rt = runtime(state);
        let shown = texts(&rt);
        assert!(shown.iter().any(|text| text == "Connected as listener"), "{shown:?}");
        assert!(rt.find("lastfm-scrobble").is_some());
        assert!(rt.find("lastfm-now-playing").is_some());
        assert!(rt.find("lastfm-love").is_some());
    }

    #[test]
    fn pressing_a_switch_flips_the_state_the_thread_is_told_about() {
        let mut state = init();
        state.lastfm = Some(client(&connected_store()));
        let mut rt = runtime(state);
        assert!(rt.state.lastfm.as_ref().unwrap().scrobble, "on by default");
        click(&mut rt, "lastfm-scrobble");
        assert!(!rt.state.lastfm.as_ref().unwrap().scrobble, "the press turns it off");
        click(&mut rt, "lastfm-scrobble");
        assert!(rt.state.lastfm.as_ref().unwrap().scrobble, "and back on");
        assert!(!rt.state.lastfm.as_ref().unwrap().love, "loving starts off");
        click(&mut rt, "lastfm-love");
        assert!(rt.state.lastfm.as_ref().unwrap().love);
    }

    #[test]
    fn a_local_track_without_an_artist_says_why_it_is_not_scrobbled() {
        let mut state = init();
        state.lastfm = Some(client(&connected_store()));
        state.emit_media(MediaEvent::Started {
            track: MediaTrack {
                id: "42".to_owned(),
                title: "Untagged".to_owned(),
                artist: String::new(),
                album: String::new(),
                duration_secs: Some(120),
                cover_url: None,
                local: true,
            },
            position_secs: 0.0,
        });
        let notice = state.notice.expect("a notice explaining the skip");
        assert!(notice.text.contains("no artist tag"), "{}", notice.text);
        assert!(!notice.error);
    }
}
