//! Music video surfaces: the 16:9 cards used in search, artist pages and the
//! Collection, and the full-screen video player overlay.
//!
//! Everything here draws real state: the thumbnails come from TIDAL's image
//! service (via [`crate::art::Artwork::request_video`]), the duration and
//! quality from the catalogue object, and the player's picture and clock from
//! [`crate::video::Engine`]. Pressing a card pauses the music queue and opens
//! the overlay; Escape or the close button stops the video and returns to the
//! page underneath.

use rune::{Color, Drag, El, Phase, SurfaceFit, Tx, col, el, icon, row, text};

use crate::app::Ui;
use crate::ui::cards::title_with_badge;
use crate::theme::{
    BODY, BUTTON, CAPS, H1, ICON_CLOSE, ICON_HEART, ICON_HEART_FILLED, ICON_PAUSE,
    ICON_PLAY, ICON_VOLUME_HIGH, ICON_VOLUME_LOW, ICON_VOLUME_MUTE, Palette, SMALL, SUBTITLE, TRACK,
};
use crate::tidal::{FavKind, Video};
use crate::ui::player_bar::format_time;

/// Card width in points; the thumbnail follows 16:9.
const CARD_W: f32 = 232.0;

/// One video card: a 16:9 thumbnail with a play glyph, then title and artist.
pub(crate) fn video_card(s: &Ui, p: Palette, video: &Video) -> El {
    let play = video.clone();
    let image = video.image.as_deref();
    let mut thumb = el()
        .key(format!("video-thumb-{}", video.id))
        .w(CARD_W)
        .aspect_ratio(16.0 / 9.0)
        .radius(6.0)
        .fill(p.card)
        .clip_rounded(6.0);
    match s.wanted_art(crate::app::ArtKind::Video, image) {
        Some(pixels) => {
            thumb = thumb.pixels_fit(pixels.pixels(), SurfaceFit::Fill).center().child(
                el()
                    .size(46.0, 46.0)
                    .radius(23.0)
                    .center()
                    .fill(Color::BLACK.alpha(0.55))
                    .child(icon(&ICON_PLAY, 22.0).color(Color::WHITE)),
            );
        }
        None => {
            thumb = thumb.center().child(icon(&ICON_PLAY, 34.0).color(p.faint));
        }
    }
    let subtitle = if video.artist.is_empty() {
        video.duration.map(format_time).unwrap_or_default()
    } else {
        match video.duration {
            Some(duration) => format!("{} · {}", video.artist, format_time(duration)),
            None => video.artist.clone(),
        }
    };
    col()
        .key(format!("video-{}", video.id))
        .role("button")
        .label(format!("Play {}", video.title))
        .w(CARD_W)
        .gap(7.0)
        .focusable(true)
        .on_press(move |s: &mut Ui| s.play_video(play.clone()))
        .child(thumb)
        .child(title_with_badge(p, text(video.title.clone()).font(TRACK).color(p.text).truncate(), video.explicit))
        .child(text(subtitle).font(SMALL).color(p.dim).w_full().truncate())
}

/// A paged Videos carousel for the artist page.
pub(crate) fn video_carousel(s: &Ui, p: Palette, key: &str, title: &str, videos: &[Video]) -> El {
    crate::ui::cards::carousel_rows(
        s,
        p,
        key,
        title,
        videos.len(),
        || crate::ui::cards::ViewAll::videos(title, videos),
        |index| video_card(s, p, &videos[index]),
    )
}

// ---------------------------------------------------------------------------------------------
// Full-screen player

/// The video player: a large 16:9 picture, the title/artist, transport, seek
/// bar, volume and close.
pub(crate) fn video_overlay(s: &Ui, p: Palette) -> El {
    let mut root = col()
        .key("video-player")
        .absolute(0.0, 0.0)
        .w_full()
        .h_full()
        .fill(Color::BLACK)
        .padding(24.0)
        .gap(14.0);

    let meta = s.video_meta().cloned().unwrap_or_else(|| Video {
        title: "Video".to_owned(),
        ..Video::default()
    });
    let mut top = row().w_full().center_y().gap(12.0)
        .child(text("VIDEO".to_owned()).font(CAPS).color(p.faint).grow(1.0));
    if let Some(session) = s.video_session() {
        let (width, height) = session.engine.resolution();
        if width > 0 && height > 0 {
            top = top.child(text(format!("{width} × {height}")).font(SMALL).color(p.faint));
        }
    }
    top = top.child(close_button(p));
    root = root.child(top);

    let mut stage = el().w_full().grow(1.0).min_h(0.0);
    match s.video_session() {
        Some(session) => match session.engine.status() {
            crate::video::Status::Failed(error) => {
                stage = stage.center().child(
                    col().gap(10.0).center().max_w(560.0)
                        .child(text("This video could not be played").font(H1).color(p.text))
                        .child(text(error).font(BODY).color(p.danger).wrap_anywhere())
                        .child(primary_video_button(p, "video-retry", "Try again", |s: &mut Ui| {
                            s.replay_video()
                        })),
                );
            }
            crate::video::Status::Loading => {
                stage = stage.center().child(
                    col().gap(8.0).center()
                        .child(icon(&ICON_PLAY, 40.0).color(p.faint))
                        .child(text("Loading video…").font(BODY).color(p.dim)),
                );
            }
            _ => {
                stage = stage
                    .center()
                    .child(
                        el()
                            .w_full()
                            .h_full()
                            .pixels_fit(session.engine.pixels(), SurfaceFit::Fit),
                    );
            }
        },
        None => {
            stage = stage.center().child(
                text("Resolving the video stream…").font(BODY).color(p.dim),
            );
        }
    }
    root = root.child(stage);

    root = root.child(details(s, p, &meta));
    root
}

fn details(s: &Ui, p: Palette, meta: &Video) -> El {
    let duration = s.video_duration_secs();
    let position = s.video_position_secs().min(duration);
    let fraction = s
        .scrub
        .unwrap_or_else(|| {
            if duration > 0 { position as f32 / duration as f32 } else { 0.0 }
        })
        .clamp(0.0, 1.0);
    let shown = s
        .scrub
        .map(|scrub| (scrub * duration as f32) as u64)
        .unwrap_or(position);
    let paused = s.video_session().map(|session| session.engine.is_paused()).unwrap_or(false);
    let ended = s
        .video_session()
        .map(|session| session.engine.status() == crate::video::Status::Ended)
        .unwrap_or(false);

    let mut title_row = row().w_full().center_y().gap(10.0);
    let title = text(meta.title.clone()).font(H1).color(p.text).truncate().grow(1.0).min_w(0.0);
    title_row = title_row.child(title_with_badge(p, title, meta.explicit));
    let id = meta.id.to_string();
    let favorite = s.is_favorite(FavKind::Video, &id);
    title_row = title_row.child(
        el()
            .key("video-heart")
            .role("button")
            .label(if favorite { "Remove from favorites" } else { "Add to favorites" })
            .size(34.0, 34.0)
            .center()
            .radius(17.0)
            .hover(p.card_hover)
            .focusable(true)
            .on_press(move |s: &mut Ui| {
                let id = id.clone();
                s.toggle_favorite(FavKind::Video, &id);
            })
            .child(icon(
                if favorite { &ICON_HEART_FILLED } else { &ICON_HEART },
                18.0,
            ).color(if favorite { p.accent } else { p.dim })),
    );

    let artist = if meta.artist.is_empty() {
        text("TIDAL video".to_owned()).font(SUBTITLE).color(p.faint)
    } else {
        text(meta.artist.clone()).font(SUBTITLE).color(p.dim)
    };
    let quality = meta.quality.clone().unwrap_or_default();

    let mut bottom = col().w_full().gap(8.0);
    bottom = bottom
        .child(title_row)
        .child(
            row().w_full().gap(10.0).center_y()
                .child(artist.truncate().grow(1.0).min_w(0.0))
                .child(text(quality).font(SMALL).color(p.faint)),
        );

    let transport = if ended {
        primary_video_button(p, "video-replay", "Replay", |s: &mut Ui| s.replay_video())
    } else {
        let label = if paused { "Play video" } else { "Pause video" };
        el()
            .key("video-play-pause")
            .role("button")
            .label(label)
            .size(44.0, 44.0)
            .center()
            .radius(22.0)
            .fill(p.text)
            .hover(p.text.mix(Color::WHITE, 0.15))
            .press(p.text.mix(Color::BLACK, 0.1))
            .focusable(s.video_session().is_some())
            .disabled(s.video_session().is_none())
            .on_press(|s: &mut Ui| s.toggle_video_pause())
            .child(icon(if paused { &ICON_PLAY } else { &ICON_PAUSE }, 22.0).color(p.accent_ink))
    };

    let volume_icon = if s.settings.muted {
        &ICON_VOLUME_MUTE
    } else if s.settings.volume < 0.5 {
        &ICON_VOLUME_LOW
    } else {
        &ICON_VOLUME_HIGH
    };
    bottom = bottom.child(
        row().w_full().gap(12.0).center_y()
            .child(transport)
            .child(text(format_time(shown)).font(SMALL).color(p.faint))
            .child(seek_bar(p, fraction))
            .child(text(format_time(duration)).font(SMALL).color(p.faint))
            .child(
                el()
                    .key("video-mute")
                    .role("button")
                    .label(if s.settings.muted { "Unmute" } else { "Mute" })
                    .size(30.0, 30.0)
                    .radius(15.0)
                    .center()
                    .hover(p.card_hover)
                    .focusable(true)
                    .on_press(|s: &mut Ui| s.toggle_mute())
                    .child(icon(volume_icon, 17.0).color(if s.settings.muted { p.faint } else { p.text })),
            )
            .child(volume_bar(p, if s.settings.muted { 0.0 } else { s.settings.volume })),
    );
    bottom
}

fn seek_bar(p: Palette, fraction: f32) -> El {
    let fraction = fraction.clamp(0.0, 1.0);
    row()
        .key("video-seek")
        .role("slider")
        .label("Seek video")
        .grow(1.0)
        .h(16.0)
        .center_y()
        .focusable(true)
        .on_drag(|s: &mut Ui, d: &mut Drag, _tx: &mut Tx| {
            let fraction = if d.w > 0.0 { d.x / d.w } else { 0.0 };
            s.scrub_video(fraction, d.phase == Phase::Up);
        })
        .child(
            row()
                .w_full()
                .h(4.0)
                .radius(2.0)
                .fill(p.card_hover)
                .child(el().grow(fraction.max(0.0001)).h(4.0).radius(2.0).fill(p.text))
                .child(el().grow((1.0 - fraction).max(0.0001)).h(4.0)),
        )
}

fn volume_bar(p: Palette, volume: f32) -> El {
    let volume = volume.clamp(0.0, 1.0);
    row()
        .key("video-volume")
        .role("slider")
        .label("Video volume")
        .w(104.0)
        .h(16.0)
        .center_y()
        .focusable(true)
        .on_drag(|s: &mut Ui, d: &mut Drag, _tx: &mut Tx| {
            let fraction = if d.w > 0.0 { d.x / d.w } else { 0.0 };
            s.scrub_volume(fraction, d.phase == Phase::Up);
        })
        .child(
            row()
                .w_full()
                .h(4.0)
                .radius(2.0)
                .fill(p.card_hover)
                .child(el().grow(volume.max(0.0001)).h(4.0).radius(2.0).fill(p.text))
                .child(el().grow((1.0 - volume).max(0.0001)).h(4.0)),
        )
}

fn primary_video_button(
    p: Palette,
    key: &str,
    label: &str,
    on: impl Fn(&mut Ui) + 'static,
) -> El {
    el()
        .key(key.to_owned())
        .role("button")
        .label(label.to_owned())
        .h(40.0)
        .px(22.0)
        .center()
        .radius(20.0)
        .fill(p.accent)
        .hover(p.accent.mix(Color::WHITE, 0.12))
        .press(p.accent.mix(Color::BLACK, 0.15))
        .focusable(true)
        .on_press(on)
        .child(text(label.to_owned()).font(BUTTON).color(p.accent_ink))
}

fn close_button(p: Palette) -> El {
    el()
        .key("video-close")
        .role("button")
        .label("Close video")
        .size(34.0, 34.0)
        .center()
        .radius(17.0)
        .hover(p.card_hover)
        .focusable(true)
        .on_press(|s: &mut Ui| s.close_video())
        .child(icon(&ICON_CLOSE, 18.0).color(p.text))
}
