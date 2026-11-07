//! Signed-out screen: session check, both sign-in flows (browser PKCE and device login),
//! failures and retries.

use rune::{El, ScrollSpec, TextStyle, Tx, col, el, icon, row, text, text_input};

use crate::app::{Login, Notice, Ui};
use crate::theme::{BODY, H1, H2, ICON_NOTE, SMALL, Palette};
use crate::widgets::{primary_button, secondary_button, section_label};

pub(crate) fn sign_in_content(s: &Ui, p: Palette) -> El {
    let mut card = col()
        .w(520.0)
        .gap(16.0)
        .padding(32.0)
        .radius(16.0)
        .fill(p.card)
        .border(1.0, p.border)
        .shadow(0.0, 40.0, rune::Color::BLACK.alpha(0.35))
        .child(icon(&ICON_NOTE, 34.0).color(p.accent));

    match &s.login {
        Login::Checking => {
            card = card
                .child(text("Checking your session").font(H2).color(p.text))
                .child(
                    text("Looking for a saved TIDAL login…")
                        .font(BODY)
                        .color(p.dim)
                        .wrap_anywhere(),
                );
        }
        Login::LoggedOut => {
            card = card
                .child(text("Sign in to TIDAL").font(H1).color(p.text))
                .child(
                    text(
                        "Browser sign-in can unlock hi-res audio. Device login is simpler, but TIDAL often limits it to HIGH quality.",
                    )
                    .font(BODY)
                    .color(p.dim)
                    .wrap_anywhere(),
                )
                .child(
                    row()
                        .gap(10.0)
                        .child(primary_button(p, "Sign in with browser", true, |s: &mut Ui, _: &mut Tx| {
                            s.start_pkce()
                        }))
                        .child(secondary_button(p, "Use device login", true, |s: &mut Ui, _: &mut Tx| {
                            s.start_login()
                        })),
                );
        }
        Login::DeviceStarting => {
            card = card
                .child(text("Connecting to TIDAL").font(H2).color(p.text))
                .child(
                    text("Starting device login…")
                        .font(BODY)
                        .color(p.dim)
                        .wrap_anywhere(),
                )
                .child(primary_button(p, "Connecting…", false, |_: &mut Ui, _: &mut Tx| {}));
        }
        Login::PkceStarting => {
            card = card
                .child(text("Connecting to TIDAL").font(H2).color(p.text))
                .child(
                    text("Requesting a browser sign-in link…")
                        .font(BODY)
                        .color(p.dim)
                        .wrap_anywhere(),
                )
                .child(primary_button(p, "Connecting…", false, |_: &mut Ui, _: &mut Tx| {}));
        }
        Login::PkceAwaiting { url } => {
            let open = url.clone();
            let copy = url.clone();
            let can_finish = !s.pkce_submitting && !s.pkce_redirect.text.trim().is_empty();
            card = card
                .child(text("Finish browser sign-in").font(H1).color(p.text))
                .child(
                    text(
                        "Open the link, approve access, then copy the full URL your browser was redirected to and paste it below.",
                    )
                    .font(BODY)
                    .color(p.dim)
                    .wrap_anywhere(),
                )
                .child(link_card(url, p))
                .child(
                    row()
                        .gap(10.0)
                        .child(primary_button(p, "Open in browser", true, move |s: &mut Ui, tx: &mut Tx| {
                            if let Err(error) = tx.open_url(&open) {
                                s.notice = Some(Notice::error(format!("Couldn't open the link: {error}")));
                            }
                        }))
                        .child(secondary_button(p, "Copy link", true, move |_: &mut Ui, tx: &mut Tx| {
                            tx.copy(copy.clone())
                        }))
                        .child(secondary_button(p, "Start over", true, |s: &mut Ui, _: &mut Tx| {
                            s.start_pkce()
                        })),
                )
                .child(
                    row()
                        .w_full()
                        .h(40.0)
                        .px(12.0)
                        .radius(8.0)
                        .fill(p.bg)
                        .border(1.0, p.border)
                        .center_y()
                        .child(
                            text_input(&s.pkce_redirect, TextStyle::new(13.0, 400.0))
                                .key("pkce-redirect")
                                .single_line()
                                .grow(1.0)
                                .color(p.text)
                                .placeholder("Paste the redirected URL", p.faint)
                                .on_edit(|s: &mut Ui| &mut s.pkce_redirect)
                                .on_submit(|s: &mut Ui, _: &mut Tx| s.finish_pkce()),
                        ),
                )
                .child(primary_button(
                    p,
                    if s.pkce_submitting { "Finishing…" } else { "Finish sign-in" },
                    can_finish,
                    |s: &mut Ui, _: &mut Tx| s.finish_pkce(),
                ))
                .child(
                    row()
                        .gap(8.0)
                        .center_y()
                        .child(el().size(6.0, 6.0).radius(3.0).fill(p.accent))
                        .child(text(if s.pkce_submitting {
                            "Exchanging the authorization code…"
                        } else {
                            "Waiting for the redirected URL."
                        }).font(SMALL).color(p.dim)),
                );
        }
        Login::DeviceAwaiting { url } => {
            let open = url.clone();
            let copy = url.clone();
            card = card
                .child(text("Authorize this device").font(H1).color(p.text))
                .child(
                    text(
                        "Open the link below and approve access. This window updates automatically when you're done.",
                    )
                    .font(BODY)
                    .color(p.dim)
                    .wrap_anywhere(),
                )
                .child(link_card(url, p))
                .child(
                    row()
                        .gap(10.0)
                        .child(primary_button(p, "Open in browser", true, move |s: &mut Ui, tx: &mut Tx| {
                            if let Err(error) = tx.open_url(&open) {
                                s.notice = Some(Notice::error(format!("Couldn't open the link: {error}")));
                            }
                        }))
                        .child(secondary_button(p, "Copy link", true, move |_: &mut Ui, tx: &mut Tx| {
                            tx.copy(copy.clone())
                        })),
                )
                .child(
                    row()
                        .gap(8.0)
                        .center_y()
                        .child(el().size(6.0, 6.0).radius(3.0).fill(p.accent))
                        .child(text("Waiting for authorization…").font(SMALL).color(p.dim)),
                );
        }
        Login::LoginFailed(error) => {
            card = card
                .child(text("Sign-in failed").font(H2).color(p.text))
                .child(text(error.clone()).font(BODY).color(p.danger).wrap_anywhere())
                .child(
                    row()
                        .gap(10.0)
                        .child(primary_button(p, "Try browser sign-in", true, |s: &mut Ui, _: &mut Tx| {
                            s.start_pkce()
                        }))
                        .child(secondary_button(p, "Use device login", true, |s: &mut Ui, _: &mut Tx| {
                            s.start_login()
                        })),
                );
        }
        Login::InitFailed(error) => {
            card = card
                .child(text("TIDAL is unavailable").font(H2).color(p.text))
                .child(text(error.clone()).font(BODY).color(p.danger).wrap_anywhere())
                .child(primary_button(p, "Try again", true, |s: &mut Ui, _: &mut Tx| {
                    s.retry_init()
                }));
        }
        Login::LoggedIn => {}
    }

    col()
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .scroll(ScrollSpec { stick_bottom: false })
        .child(col().w_full().center_y().padding(40.0).child(card))
}

fn link_card(url: &str, p: Palette) -> El {
    col()
        .w_full()
        .gap(6.0)
        .padding(14.0)
        .radius(8.0)
        .fill(p.bg)
        .border(1.0, p.border)
        .child(section_label("AUTHORIZATION LINK", p))
        .child(
            text(url.to_owned())
                .font(TextStyle::new(13.0, 400.0).mono())
                .color(p.accent)
                .selectable(true)
                .wrap_anywhere(),
        )
}
