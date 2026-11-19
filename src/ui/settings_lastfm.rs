//! The Settings cards for the scrobbling services.
//!
//! One card per entry in [`crate::lastfm::SERVICES`] — Last.fm and scrobble.now — built by the
//! same function: the status badge, the connect flow (Last.fm asks for the user's own API key
//! and shared secret; scrobble.now ships with tsun's), the connected-as line, the three switches
//! and the status line under them. Every control does something real: Connect fetches a token
//! and opens the service's authorization page in the browser, "I've authorized" exchanges it for
//! a session, and the switches are handed to that service's scrobble thread where they gate what
//! is sent. Widget keys carry the service's id (`lastfm-*`, `scrobblenow-*`), so the two cards
//! are told apart by tests and automation.

use rune::{El, TextStyle, Tx, col, row, text, text_input};

use crate::app::{Notice, Ui};
use crate::lastfm::{Client, Connection, Pref};
use crate::theme::{SMALL, Palette};
use crate::ui::settings_widgets::{self as w, Tone};
use crate::widgets::{primary_button, secondary_button};

/// One card per running scrobbling service, in the order [`crate::lastfm::SERVICES`] lists them
/// (Last.fm first, then scrobble.now).
pub(crate) fn scrobbling_cards(s: &Ui, p: Palette) -> El {
    let mut cards = col().w_full().gap(14.0);
    for client in &s.scrobbling {
        cards = cards.child(scrobble_card(client, p));
    }
    cards
}

/// One service's card.
pub(crate) fn scrobble_card(client: &Client, p: Palette) -> El {
    let service = client.service;
    let id = service.id;
    let (tone, label, hint) = match &client.conn {
        Connection::Connected { username } => (
            Tone::Ok,
            format!("Connected as {username}"),
            "Every play past the threshold is scrobbled from here.".to_owned(),
        ),
        Connection::Authorizing { .. } => (
            Tone::Warn,
            "Waiting for authorization".to_owned(),
            format!("Approve tsun in the browser page {} opened, then confirm below.", service.name),
        ),
        Connection::Disconnected => {
            (Tone::Off, "Not connected".to_owned(), service.connect_hint.to_owned())
        }
    };
    let mut card = w::card(p)
        .key(format!("{id}-card"))
        .child(
            row().w_full().padding(14.0).child(w::card_header(
                p,
                &format!("{} scrobbling", service.name),
                service.description,
                Some(w::status_badge(p, tone, &label).key(format!("{id}-status"))),
            )),
        )
        .child(
            row()
                .w_full()
                .px(14.0)
                .pb(12.0)
                .child(w::note(w::tone_color(tone, p), &hint)),
        );
    match &client.conn {
        Connection::Connected { .. } => {
            card = card
                .child(w::divider(p))
                .child(w::toggle_row(
                    p,
                    &format!("{id}-scrobble"),
                    "Scrobble",
                    Some("Send plays once they pass half the track or four minutes."),
                    client.scrobble,
                    move |s: &mut Ui| s.scrobble_set_pref(service, Pref::Scrobble),
                ))
                .child(w::divider(p))
                .child(w::toggle_row(
                    p,
                    &format!("{id}-now-playing"),
                    "Send now playing",
                    Some(&format!("Tell {} what is playing as soon as a track starts.", service.name)),
                    client.now_playing,
                    move |s: &mut Ui| s.scrobble_set_pref(service, Pref::NowPlaying),
                ))
                .child(w::divider(p))
                .child(w::toggle_row(
                    p,
                    &format!("{id}-love"),
                    "Love tracks when I favourite them on TIDAL",
                    Some("Mirrors the heart: a TIDAL favourite loves the track, removing it un-loves it. Local files have no TIDAL favourite, so they are never loved."),
                    client.love,
                    move |s: &mut Ui| s.scrobble_set_pref(service, Pref::Love),
                ))
                .child(w::divider(p))
                .child(
                    row().w_full().padding(14.0).justify_end().child(secondary_button(
                        p,
                        "Disconnect",
                        true,
                        move |s: &mut Ui, _: &mut Tx| s.scrobble_disconnect(service),
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
                        .child(primary_button(p, "I've authorized", true, move |s: &mut Ui, _: &mut Tx| {
                            s.scrobble_confirm(service)
                        }))
                        .child(secondary_button(p, "Cancel", true, move |s: &mut Ui, _: &mut Tx| {
                            s.scrobble_disconnect(service)
                        })),
                );
        }
        Connection::Disconnected => {
            if service.builtin.is_none() {
                card = card
                    .child(w::divider(p))
                    .child(
                        col()
                            .w_full()
                            .padding(14.0)
                            .gap(16.0)
                            .child(key_field(client, p))
                            .child(secret_field(client, p)),
                    );
            }
            card = card.child(
                row()
                    .w_full()
                    .px(14.0)
                    .pb(14.0)
                    .gap(12.0)
                    .center_y()
                    .justify_end()
                    .child(
                        text(format!(
                            "Connect opens {} in your browser to authorize this app.",
                            service.name
                        ))
                        .font(SMALL)
                        .color(p.faint)
                        .wrap(),
                    )
                    .child(primary_button(p, "Connect", true, move |s: &mut Ui, _: &mut Tx| {
                        s.scrobble_connect(service)
                    })),
            );
        }
    }
    if let Some(message) = &client.message {
        card = card.child(
            text(message.clone())
                .font(SMALL)
                .color(if client.error { p.danger } else { p.dim })
                .px(14.0)
                .pb(12.0)
                .wrap(),
        );
    }
    card
}

/// The API-key field (services that need the user's own credentials).
fn key_field(client: &Client, p: Palette) -> El {
    let service = client.service;
    w::field_row(
        p,
        "API key",
        w::field_frame(
            text_input(&client.key, TextStyle::new(13.0, 400.0))
                .key(format!("{}-api-key", service.id))
                .single_line()
                .grow(1.0)
                .color(p.text)
                .placeholder("API key", p.faint)
                .on_edit(move |s: &mut Ui| &mut s.scrobble_client_mut(service).expect("the service is running").key)
                .on_submit(move |s: &mut Ui, _: &mut Tx| s.scrobble_save_credentials(service)),
            p,
        ),
        None,
    )
}

/// The shared-secret field, masked.
fn secret_field(client: &Client, p: Palette) -> El {
    let service = client.service;
    w::field_row(
        p,
        "Shared secret",
        w::field_frame(
            text_input(&client.secret, TextStyle::new(13.0, 400.0))
                .key(format!("{}-secret", service.id))
                .single_line()
                .grow(1.0)
                .color(p.text)
                .placeholder("Shared secret", p.faint)
                .secure()
                .on_edit(move |s: &mut Ui| &mut s.scrobble_client_mut(service).expect("the service is running").secret)
                .on_submit(move |s: &mut Ui, _: &mut Tx| s.scrobble_save_credentials(service)),
            p,
        ),
        Some(service.credentials_help),
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
    use crate::lastfm::{Client, Store, LASTFM, SCROBBLENOW};
    use crate::media_events::{MediaEvent, MediaTrack};
    use crate::theme::palette;

    fn cards_view(s: &Ui, _: &mut rune::Cx<Ui>) -> El {
        scrobbling_cards(s, palette())
    }

    fn runtime(state: Ui) -> rune::Runtime<Ui> {
        // Tall enough that every row of both cards is laid out inside the window.
        let mut rt = rune::Runtime::new(state, cards_view, (620.0, 1700.0), rune::Env::new(true, 1.0));
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

    /// Clients with the given stores, but no threads (so the test touches neither the
    /// filesystem nor the network and cannot race another test over `XDG_CONFIG_HOME`).
    fn clients(lastfm: &Store, scrobblenow: &Store) -> Vec<Client> {
        vec![Client::for_test(&LASTFM, lastfm), Client::for_test(&SCROBBLENOW, scrobblenow)]
    }

    /// A connected store with the default switches.
    fn connected_store(username: &str) -> Store {
        Store {
            api_key: "key".to_owned(),
            secret: "secret".to_owned(),
            session_key: "session".to_owned(),
            username: username.to_owned(),
            ..Store::default()
        }
    }

    #[test]
    fn the_lastfm_card_asks_for_a_key_and_secret() {
        let mut state = init();
        state.scrobbling = clients(&Store::default(), &Store::default());
        let rt = runtime(state);
        assert!(rt.find("lastfm-api-key").is_some(), "the API key field is there");
        assert!(rt.find("lastfm-secret").is_some(), "the secret field is there");
        let shown = texts(&rt);
        assert!(shown.iter().any(|text| text == "Connect"), "{shown:?}");
        assert!(shown.iter().any(|text| text.contains("last.fm/api/account/create")), "{shown:?}");
    }

    /// scrobble.now needs nothing pasted: tsun ships its key, so the card is just Connect.
    #[test]
    fn the_scrobblenow_card_needs_no_key() {
        let mut state = init();
        state.scrobbling = clients(&Store::default(), &Store::default());
        let rt = runtime(state);
        assert!(rt.find("scrobblenow-card").is_some());
        assert!(rt.find("scrobblenow-api-key").is_none(), "no key field: tsun ships one");
        assert!(rt.find("scrobblenow-secret").is_none(), "no secret field either");
        let shown = texts(&rt);
        assert!(
            shown.iter().any(|text| text.contains("no API key is needed")),
            "{shown:?}"
        );
        assert!(
            shown.iter().any(|text| text.contains("Connect opens scrobble.now in your browser")),
            "{shown:?}"
        );
    }

    #[test]
    fn a_connected_card_shows_the_account_and_the_switches() {
        for (service, username) in [(&LASTFM, "listener"), (&SCROBBLENOW, "scrobbler")] {
            let mut state = init();
            state.scrobbling = clients(&connected_store("listener"), &connected_store("scrobbler"));
            let rt = runtime(state);
            let shown = texts(&rt);
            assert!(
                shown.iter().any(|text| text == &format!("Connected as {username}")),
                "{service:?}: {shown:?}"
            );
            for toggle in ["scrobble", "now-playing", "love"] {
                let key = format!("{}-{toggle}", service.id);
                assert!(rt.find(&key).is_some(), "{key} is missing: {shown:?}");
            }
            // The card names its own service, never the other one's.
            assert!(shown.iter().any(|text| text == &format!("{} scrobbling", service.name)));
        }
    }

    #[test]
    fn the_two_cards_are_told_apart_by_their_keys() {
        let mut state = init();
        state.scrobbling = clients(&connected_store("listener"), &connected_store("scrobbler"));
        let rt = runtime(state);
        for id in ["lastfm", "scrobblenow"] {
            assert!(rt.find(&format!("{id}-card")).is_some(), "{id} card");
            assert!(rt.find(&format!("{id}-status")).is_some(), "{id} status");
        }
        assert!(rt.find("lastfm-scrobble").is_some());
        assert!(rt.find("scrobblenow-scrobble").is_some());
        let shown = texts(&rt);
        assert!(shown.iter().any(|text| text == "Last.fm scrobbling"), "{shown:?}");
        assert!(shown.iter().any(|text| text == "scrobble.now scrobbling"), "{shown:?}");
    }

    #[test]
    fn pressing_a_switch_flips_only_that_services_state() {
        let mut state = init();
        state.scrobbling = clients(&connected_store("listener"), &connected_store("scrobbler"));
        let mut rt = runtime(state);
        let scrobble = |rt: &rune::Runtime<Ui>, id: &str| {
            rt.state.scrobbling.iter().find(|c| c.service.id == id).unwrap().scrobble
        };
        assert!(scrobble(&rt, "lastfm") && scrobble(&rt, "scrobblenow"), "on by default");
        click(&mut rt, "scrobblenow-scrobble");
        assert!(!scrobble(&rt, "scrobblenow"), "the press turns the second service off");
        assert!(scrobble(&rt, "lastfm"), "and leaves the first alone");
        click(&mut rt, "lastfm-love");
        let love = |rt: &rune::Runtime<Ui>, id: &str| {
            rt.state.scrobbling.iter().find(|c| c.service.id == id).unwrap().love
        };
        assert!(love(&rt, "lastfm"), "Last.fm's love switch is on");
        assert!(!love(&rt, "scrobblenow"), "scrobble.now's is still off");
    }

    #[test]
    fn a_local_track_without_an_artist_says_why_it_is_not_scrobbled() {
        let mut state = init();
        state.scrobbling = clients(&connected_store("listener"), &connected_store("scrobbler"));
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
        assert!(state.media.take_notes().is_empty(), "one track is one notice, not one per service");
    }
}
