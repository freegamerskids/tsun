//! The Local files page and its album/artist drill-downs.
//!
//! Tracks, albums, artists and folders all come from the scanned [`crate::local::Library`]; the
//! rows reuse the shared track table, so playback queues, the "…" menu and the row layout match
//! the rest of the app (minus the TIDAL-only actions). Filtering and sorting are local.
//!
//! Every tab is one virtual page (see [`crate::ui::tracks`]), so a library of thousands of files
//! costs a window of rows per commit: the title, the tab strip, the toolbar and the tab's own
//! summary are the list's leading blocks, and the file rows follow. The filtered, sorted view
//! behind them is rebuilt only when the filter, the sort or the library changes.

use rune::{Color, El, TextStyle, col, el, row, spacer, text, text_input};

use crate::app::Ui;
use crate::local::{LocalSort, LocalTab, LocalTrack};
use crate::theme::{BODY, BUTTON, CAPS, H1, H2, SMALL, TRACK, Palette};
use crate::ui::cards::cover_tile;
use crate::ui::tracks::{
    ALBUM_H, LibraryList, Page, TRACK_H, blocks_page, grid_page, rows_page, section, tracks_page,
    version,
};
use crate::widgets::{primary_button, secondary_button};

/// The Local files root page.
pub(crate) fn local_content(s: &Ui, p: Palette) -> El {
    let (mut body, note) = local_page(s);
    body = body.leading_first(vec![
        section("title", title).after(17.0),
        section("tabs", tabs).after(17.0),
        section("toolbar", toolbar).after(17.0),
    ]);
    if let Some(message) = note {
        body = body.trailing(
            section("empty", move |_, p| empty(p, &message)).before(17.0),
        );
    }
    body.render(s, p)
}

/// The page for the open tab, and the message to show when it has nothing.
fn local_page(s: &Ui) -> (Page, Option<String>) {
    if s.settings.local_folders.is_empty() {
        return (
            messages_page(),
            Some("Add a music folder in Settings → Local files to build your library.".to_owned()),
        );
    }
    if matches!(s.local_state, crate::app::SearchState::Searching) && s.local.is_empty() {
        return (messages_page(), Some("Scanning your music folders…".to_owned()));
    }
    if s.local.is_empty() {
        return (messages_page(), Some("No music files found in the configured folders yet.".to_owned()));
    }
    match s.local_tab {
        LocalTab::Tracks => (tracks_tab(s), None),
        LocalTab::Albums => (albums_tab(s), None),
        LocalTab::Artists => (artists_tab(s), None),
        LocalTab::Folders => (folders_tab(s), None),
    }
}

/// A page with no rows of its own, for the messages above.
fn messages_page() -> Page {
    blocks_page("local-content")
}

fn title(s: &Ui, p: Palette) -> El {
    row().w_full().center_y().gap(12.0)
        .child(text("Local files").font(H1).color(p.text))
        .child(spacer())
        .child(primary_button(p, "Play all", !s.local.is_empty(), |s: &mut Ui, _| {
            s.play_local_indices(&(0..s.local.tracks.len()).collect::<Vec<_>>());
        }))
        .child(secondary_button(p, "Shuffle", !s.local.is_empty(), |s: &mut Ui, _| {
            s.shuffle_local_indices(&(0..s.local.tracks.len()).collect::<Vec<_>>());
        }))
}

fn tabs(s: &Ui, p: Palette) -> El {
    let mut row = row().w_full().gap(8.0);
    for (tab, label) in [
        (LocalTab::Tracks, "Tracks"),
        (LocalTab::Albums, "Albums"),
        (LocalTab::Artists, "Artists"),
        (LocalTab::Folders, "Folders"),
    ] {
        let active = s.local_tab == tab;
        row = row.child(
            el().key(format!("local-tab-{label}")).role("tab").label(label.to_owned())
                .h(34.0).px(16.0).center().radius(17.0)
                .fill(if active { p.text } else { Color::TRANSPARENT })
                .hover(if active { p.text } else { p.card_hover })
                .focusable(true)
                .on_press(move |s: &mut Ui| s.set_local_tab(tab))
                .child(text(label.to_owned()).font(BUTTON).color(if active { p.accent_ink } else { p.dim })),
        );
    }
    row
}

fn toolbar(s: &Ui, p: Palette) -> El {
    let mut bar = row().w_full().gap(12.0).center_y()
        .child(
            row().w(240.0).h(34.0).px(12.0).gap(6.0).center_y().radius(17.0).fill(p.card)
                .child(text_input(&s.local_filter, TextStyle::new(13.0, 400.0))
                    .key("local-filter-input").single_line().grow(1.0).color(p.text)
                    .placeholder("Filter", p.faint)
                    .on_edit(|s: &mut Ui| &mut s.local_filter)),
        )
        .child(
            el().key("local-sort").role("button").label("Sort")
                .h(34.0).px(14.0).gap(8.0).center_y().radius(17.0)
                .fill(p.card).hover(p.card_hover).focusable(true)
                .on_press(|s: &mut Ui| s.cycle_local_sort())
                .child(text(format!("Sort: {}", s.local_sort.label())).font(BUTTON).color(p.text))
                .child(text("↻").font(SMALL).color(p.dim)),
        );
    if !s.local_filter.text.is_empty() {
        bar = bar.child(
            el().key("local-filter-clear").role("button").label("Clear filter")
                .size(20.0, 20.0).radius(10.0).center().hover(p.card_hover).focusable(true)
                .on_press(|s: &mut Ui| s.clear_local_filter())
                .child(text("×").font(TextStyle::new(14.0, 500.0)).color(p.dim)),
        );
    }
    bar
}

// ---------------------------------------------------------------------------------------------
// Filtering and sorting

fn needle(s: &Ui) -> String {
    s.local_filter.text.trim().to_lowercase()
}

fn matches(track: &LocalTrack, needle: &str) -> bool {
    needle.is_empty()
        || track.title.to_lowercase().contains(needle)
        || track.display_artist().to_lowercase().contains(needle)
        || track.album.to_lowercase().contains(needle)
}

fn sort_key(track: &LocalTrack, sort: LocalSort) -> (String, u32, u32, String) {
    let year = track.year.clone().unwrap_or_default();
    match sort {
        LocalSort::Title => (track.title.to_lowercase(), 0, 0, year),
        LocalSort::Album => (
            track.album.to_lowercase(),
            track.disc_number.unwrap_or(0),
            track.track_number.unwrap_or(0),
            year,
        ),
        LocalSort::Year => (year, 0, 0, track.title.to_lowercase()),
        LocalSort::Artist => (
            track.display_artist().to_lowercase(),
            track.disc_number.unwrap_or(0),
            track.track_number.unwrap_or(0),
            track.title.to_lowercase(),
        ),
    }
}

/// The filtered, sorted track indices of the whole library.
fn select_indices(s: &Ui) -> Vec<usize> {
    let needle = needle(s);
    let mut indices: Vec<usize> = s
        .local
        .tracks
        .iter()
        .enumerate()
        .filter(|(_, track)| matches(track, &needle))
        .map(|(index, _)| index)
        .collect();
    indices.sort_by_key(|index| sort_key(&s.local.tracks[*index], s.local_sort));
    indices
}

/// The Tracks tab's filtered, sorted view, from the page's cache.
fn filtered(s: &Ui) -> LibraryList {
    s.local_tracks
        .get(version((s.data_rev, needle(s), s.local_sort)), || select_indices(s), |indices| {
            s.local.track_list(indices)
        })
}

fn tracks_tab(s: &Ui) -> Page {
    let view = filtered(s);
    let count = view.indices.len();
    tracks_page("local-content", view.tracks.clone(), true, TRACK_H)
        .leading(section("count", move |s, p| tracks_summary(s, p, count)).after(11.0))
}

/// The Tracks tab's summary row: the count, and Play/Shuffle for what the filter shows.
fn tracks_summary(_s: &Ui, p: Palette, count: usize) -> El {
    row().gap(10.0)
        .child(text(format!("{count} tracks")).font(SMALL).color(p.faint))
        .child(spacer())
        .child(secondary_button(p, "Play", count > 0, |s: &mut Ui, _| {
            let indices = filtered(s).indices.to_vec();
            s.play_local_indices(&indices);
        }))
        .child(secondary_button(p, "Shuffle", count > 0, |s: &mut Ui, _| {
            let indices = filtered(s).indices.to_vec();
            s.shuffle_local_indices(&indices);
        }))
}

fn albums_tab(s: &Ui) -> Page {
    let needle = needle(s);
    let list = s.local_albums.get(version((s.data_rev, needle.clone(), s.local_sort)), || {
        let mut albums: Vec<usize> = (0..s.local.albums.len())
            .filter(|&index| {
                let album = &s.local.albums[index];
                needle.is_empty()
                    || album.title.to_lowercase().contains(&needle)
                    || album.artist.to_lowercase().contains(&needle)
            })
            .collect();
        albums.sort_by_key(|&index| match s.local_sort {
            LocalSort::Title => s.local.albums[index].title.to_lowercase(),
            LocalSort::Year => s.local.albums[index].year.clone().unwrap_or_default(),
            _ => format!(
                "{}\u{1}{}",
                s.local.albums[index].artist.to_lowercase(),
                s.local.albums[index].title.to_lowercase()
            ),
        });
        albums
    });
    grid_page(s, "local-content", list, ALBUM_H, |s, p, index| {
        // `index` is a position in the sorted view; the album it names is looked up in the
        // library, so a re-scan between commits cannot open the wrong one.
        let Some(&album_index) = s.local_albums.peek().get(index) else { return el() };
        let Some(album) = s.local.albums.get(album_index) else { return el() };
        let key = album.key.clone();
        col().key(format!("local-album-{key}")).role("button")
            .label(format!("Open {}", album.title))
            .w(168.0).gap(8.0).focusable(true)
            .on_press(move |s: &mut Ui| s.open_local_album(key.clone()))
            .child(cover_tile(s, p, album.cover.as_deref(), 168.0))
            .child(text(album.title.clone()).font(TRACK).color(p.text).w_full().truncate())
            .child(
                text(match &album.year {
                    Some(year) => format!("{} · {year}", album.artist),
                    None => album.artist.clone(),
                })
                .font(SMALL).color(p.dim).w_full().truncate(),
            )
            .child(text(format!("{} tracks", album.tracks.len())).font(SMALL).color(p.faint).w_full())
    })
}

fn artists_tab(s: &Ui) -> Page {
    let needle = needle(s);
    let list = s.local_artists.get(version((s.data_rev, needle.clone())), || {
        let mut artists: Vec<usize> = (0..s.local.artists.len())
            .filter(|&index| needle.is_empty() || s.local.artists[index].name.to_lowercase().contains(&needle))
            .collect();
        artists.sort_by_key(|&index| s.local.artists[index].name.to_lowercase());
        artists
    });
    rows_page(
        "local-content",
        list.rows.len(),
        list.generation,
        TRACK_H,
        move |s, p, index| {
            let Some(&artist_index) = s.local_artists.peek().get(index) else { return el() };
            let Some(artist) = s.local.artists.get(artist_index) else { return el() };
            let name = artist.name.clone();
            let cover = artist
                .tracks
                .first()
                .and_then(|track| s.local.tracks.get(*track))
                .and_then(|track| track.cover.clone());
            row().key(format!("local-artist-{name}")).role("button")
                .label(format!("Open {}", artist.name))
                .w_full().h(TRACK_H).px(12.0).gap(14.0).center_y().radius(6.0)
                .hover(p.card_hover).focusable(true)
                .on_press(move |s: &mut Ui| s.open_local_artist(name.clone()))
                .child(cover_tile(s, p, cover.as_deref(), 40.0))
                .child(text(artist.name.clone()).font(TRACK).color(p.text).grow(1.0).truncate())
                .child(text(format!("{} albums", artist.albums.len())).font(SMALL).color(p.dim))
                .child(text(format!("{} tracks", artist.tracks.len())).font(SMALL).color(p.faint))
        },
    )
}

fn folders_tab(s: &Ui) -> Page {
    let Some(folder) = &s.local_folder else {
        let roots = s.local.roots.clone();
        // A handful of configured roots: no rows of its own, just the block that holds them.
        return blocks_page("local-content").leading(section(
            "roots",
            move |s, p| {
                let mut list = col().w_full().gap(4.0);
                for (index, root) in roots.iter().enumerate() {
                    let count = s.local.tracks_under(root).len();
                    list = list.child(folder_row(p, index, root, count));
                }
                col().w_full().gap(10.0)
                    .child(text("Folders").font(H2).color(p.text))
                    .child(list)
            },
        ));
    };

    let up = s
        .local_folders()
        .iter()
        .any(|root| root == folder)
        .then_some("Sources")
        .unwrap_or("Up");
    let subfolders = s.local.subfolders(folder);
    let path = folder.clone();
    let list = s.local_page_tracks.get(version((s.data_rev, path.clone())), || {
        s.local.track_list(&s.local.tracks_in(&path))
    });
    let blank = list.rows.is_empty();
    let mut page = tracks_page("local-content", list, !blank, TRACK_H);
    page = page.leading(
        section("folder", move |s, p| folder_header(s, p, up, &subfolders)).after(11.0),
    );
    if blank {
        page = page.trailing(
            section("empty", |_, p| {
                col().w_full().child(text("No tracks directly in this folder.").font(SMALL).color(p.faint))
            })
            .before(11.0),
        );
    }
    page
}

/// The open folder's heading: the Up button, its path, Play folder, and its subfolders.
fn folder_header(s: &Ui, p: Palette, up: &'static str, subfolders: &[std::path::PathBuf]) -> El {
    let mut body = col().w_full().gap(12.0)
        .child(row().w_full().center_y().gap(10.0)
            .child(
                el().key("local-folder-up").role("button").label(up)
                    .h(28.0).px(12.0).center().radius(14.0).fill(p.card)
                    .hover(p.card_hover).focusable(true)
                    .on_press(|s: &mut Ui| s.local_folder_up())
                    .child(text(up).font(SMALL).color(p.text)),
            )
            .child(text(s.local_folder.as_ref().map(|folder| folder.display().to_string()).unwrap_or_default())
                .font(SMALL).color(p.dim).grow(1.0).truncate())
            .child(secondary_button(p, "Play folder", true, |s: &mut Ui, _| {
                if let Some(folder) = s.local_folder.clone() {
                    let indices = s.local.tracks_under(&folder);
                    s.play_local_indices(&indices);
                }
            })));
    if !subfolders.is_empty() {
        let mut list = col().w_full().gap(1.0);
        for (index, child) in subfolders.iter().enumerate() {
            let count = s.local.tracks_under(child).len();
            list = list.child(folder_row(p, index, child, count));
        }
        body = body.child(caps("Folders", p)).child(list);
    }
    body.child(caps("Tracks", p))
}

fn folder_row(p: Palette, index: usize, path: &std::path::Path, count: usize) -> El {
    let open = path.to_path_buf();
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
        .unwrap_or_else(|| path.display().to_string());
    row().key(format!("local-folder-row-{index}")).role("button")
        .label(format!("Open folder {name}"))
        .w_full().h(48.0).px(12.0).gap(12.0).center_y().radius(6.0)
        .hover(p.card_hover).focusable(true)
        .on_press(move |s: &mut Ui| s.open_local_folder(open.clone()))
        .child(text("▸").font(TRACK).color(p.dim))
        .child(text(name).font(TRACK).color(p.text).grow(1.0).truncate())
        .child(text(path.display().to_string()).font(SMALL).color(p.faint).truncate())
        .child(text(format!("{count} tracks")).font(SMALL).color(p.dim))
}

/// One local album's page.
pub(crate) fn local_album_content(s: &Ui, p: Palette) -> El {
    let Some(album) = s.local_album.as_deref().and_then(|key| s.local.album(key)) else {
        return empty_page(p, "That album is no longer in the local library.");
    };
    let key = album.key.clone();
    let list = s
        .local_page_tracks
        .get(version((s.data_rev, key.clone())), || s.local.album_tracks(&key));
    let mut page = tracks_page("local-album-content", list.clone(), true, TRACK_H);
    if !list.rows.is_empty() {
        page = page.leading(section("header", |s, p| local_album_header(s, p)).after(19.0));
    }
    page.render(s, p)
}

fn local_album_header(s: &Ui, p: Palette) -> El {
    let Some(album) = s.local_album.as_deref().and_then(|key| s.local.album(key)) else {
        return el();
    };
    let key = album.key.clone();
    let artist = album.artist.clone();
    let details = match &album.year {
        Some(year) => format!("{artist} · {year} · {} tracks", album.tracks.len()),
        None => format!("{artist} · {} tracks", album.tracks.len()),
    };
    let rounds = album.tracks.len();
    row().w_full().gap(22.0).center_y()
        .child(cover_tile(s, p, album.cover.as_deref(), 160.0))
        .child(col().grow(1.0).min_w(0.0).gap(8.0)
            .child(text(album.title.clone()).font(H1).color(p.text).truncate())
            .child(
                el().role("button").label("Open artist").focusable(true)
                    .on_press(move |s: &mut Ui| s.open_local_artist(artist.clone()))
                    .child(text(details.clone()).font(BODY).color(p.dim)),
            )
            .child(row().gap(10.0)
                .child(primary_button(p, "Play", rounds > 0, {
                    let key = key.clone();
                    move |s: &mut Ui, _| s.play_local_album(&key)
                }))
                .child(secondary_button(p, "Shuffle", rounds > 0, move |s: &mut Ui, _| {
                    s.shuffle_local_album(&key)
                }))))
}

/// One local artist's page.
pub(crate) fn local_artist_content(s: &Ui, p: Palette) -> El {
    let Some(artist) = s.local_artist.as_deref().and_then(|name| s.local.artist(name)) else {
        return empty_page(p, "That artist is no longer in the local library.");
    };
    let name = artist.name.clone();
    let list = s
        .local_page_tracks
        .get(version((s.data_rev, name.clone())), || s.local.artist_tracks(&name));
    let blank = list.rows.is_empty();
    let mut page = tracks_page("local-artist-content", list, !blank, TRACK_H);
    page = page.leading(section("header", |s, p| local_artist_header(s, p)).after(19.0));
    if !blank {
        page = page.leading(section("tracks-label", |_, p| caps("Tracks", p)).after(11.0));
    }
    page.render(s, p)
}

fn local_artist_header(s: &Ui, p: Palette) -> El {
    let Some(artist) = s.local_artist.as_deref().and_then(|name| s.local.artist(name)) else {
        return el();
    };
    let name = artist.name.clone();
    let albums: Vec<(String, String, usize, Option<String>)> = artist
        .albums
        .iter()
        .filter_map(|key| s.local.album(key))
        .map(|album| (album.key.clone(), album.title.clone(), album.tracks.len(), album.cover.clone()))
        .collect();
    let rounds = artist.tracks.len();
    let mut album_rows = col().w_full().gap(1.0);
    for (index, (key, title, count, cover)) in albums.into_iter().enumerate() {
        album_rows = album_rows.child(
            row().key(format!("local-artist-album-{index}")).role("button")
                .label(format!("Open {title}"))
                .w_full().h(56.0).px(12.0).gap(12.0).center_y().radius(6.0)
                .hover(p.card_hover).focusable(true)
                .on_press(move |s: &mut Ui| s.open_local_album(key.clone()))
                .child(cover_tile(s, p, cover.as_deref(), 40.0))
                .child(text(title).font(TRACK).color(p.text).grow(1.0).truncate())
                .child(text(format!("{count} tracks")).font(SMALL).color(p.faint)),
        );
    }
    col().w_full().gap(20.0)
        .child(row().w_full().gap(20.0).center_y()
            .child(text(artist.name.clone()).font(H1).color(p.text).grow(1.0).truncate())
            .child(primary_button(p, "Play", rounds > 0, {
                let play = name.clone();
                move |s: &mut Ui, _| s.play_local_artist(&play)
            }))
            .child(secondary_button(p, "Shuffle", rounds > 0, {
                let shuffle = name.clone();
                move |s: &mut Ui, _| s.shuffle_local_artist(&shuffle)
            })))
        .child(text(artist.name.clone()).font(H2).color(p.text))
        .child(album_rows)
}

fn caps(title: &str, p: Palette) -> El {
    text(title).font(CAPS).color(p.faint)
}

fn empty(p: Palette, message: &str) -> El {
    text(message.to_owned()).font(BODY).color(p.dim).wrap_anywhere()
}

fn empty_page(p: Palette, message: &str) -> El {
    col().w_full().h_full().px(48.0).pt(32.0).child(empty(p, message))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{Page, SearchState, init};
    use crate::local::{Library, LocalAlbum, LocalArtist};
    use std::path::PathBuf;

    /// A library of `count` files in `count / 10` albums by `count / 20` artists.
    fn library(count: usize) -> Library {
        let mut library = Library::default();
        library.roots = vec![PathBuf::from("/music")];
        for index in 0..count {
            let id = crate::local::LOCAL_ID_BIT | (index as u64 + 1);
            library.tracks.push(LocalTrack {
                id,
                path: PathBuf::from(format!("/music/album{}/song{index}.flac", index % 10)),
                title: format!("Song {index}"),
                artist: format!("Artist {}", index % (count / 20).max(1)),
                album_artist: String::new(),
                album: format!("Album {}", index % 10),
                track_number: Some((index % 12) as u32 + 1),
                disc_number: Some(1),
                year: Some("2020".to_owned()),
                duration: Some(200),
                codec: "FLAC".to_owned(),
                sample_rate: Some(44_100),
                bit_depth: Some(16),
                bitrate_kbps: None,
                replaygain: None,
                cover: None,
                mtime: 0,
                size: 0,
            });
        }
        for index in 0..10 {
            let tracks: Vec<usize> = (0..count).filter(|i| i % 10 == index).collect();
            library.albums.push(LocalAlbum {
                key: format!("album-{index}"),
                title: format!("Album {index}"),
                artist: format!("Artist {}", index % (count / 20).max(1)),
                year: Some("2020".to_owned()),
                cover: None,
                duration: None,
                tracks,
            });
        }
        for index in 0..(count / 20).max(1) {
            let tracks: Vec<usize> = (0..count).filter(|i| i % (count / 20).max(1) == index).collect();
            library.artists.push(LocalArtist {
                name: format!("Artist {index}"),
                albums: library.albums.iter().map(|album| album.key.clone()).collect(),
                tracks,
            });
        }
        for (index, track) in library.tracks.iter().enumerate() {
            library.by_id.insert(track.id, index);
        }
        library
    }

    /// The local pages render from the library and mount a window of rows, not the list: the
    /// Tracks, Albums, Artists and Folders tabs, and the album and artist pages.
    #[test]
    fn the_local_pages_mount_a_window_of_rows() {
        let _env = crate::settings::lock_config_env();
        let mut s = init();
        s.local = library(2_000);
        s.page = Page::Local;
        s.local_state = SearchState::Done;
        s.settings.local_folders = vec!["/music".to_owned()];
        let mut rt = rune::Runtime::new(s, crate::ui::view, (1280.0, 800.0), rune::Env::new(true, 1.0));
        rt.frame(0.0);
        for _ in 0..6 {
            rt.frame(1.0 / 120.0);
        }
        let keys = |rt: &rune::Runtime<Ui>| -> Vec<String> {
            rt.elements().into_iter().filter_map(|element| element.key).collect()
        };
        let rows = keys(&rt).iter().filter(|key| key.starts_with("track-") && !key.starts_with("track-menu-")).count();
        assert!(rows > 0 && rows < 60, "{rows} rows of a 2,000-track library");

        for (tab, prefix, expected) in [
            (LocalTab::Albums, "local-album-", 10usize),
            (LocalTab::Artists, "local-artist-", 20),
            (LocalTab::Folders, "local-folder-", 1),
        ] {
            rt.update(|s, _| s.set_local_tab(tab));
            for _ in 0..6 {
                rt.frame(1.0 / 120.0);
            }
            let found = keys(&rt).iter().filter(|key| key.starts_with(prefix)).count();
            assert!(found >= expected, "{tab:?}: {found} keys starting {prefix}");
        }

        // The album page: its header plus a window of the album's tracks.
        rt.update(|s, _| s.open_local_album("album-3".to_owned()));
        for _ in 0..6 {
            rt.frame(1.0 / 120.0);
        }
        let keys_now = keys(&rt);
        assert!(keys_now.iter().any(|key| key == "local-album-content-rows"), "{keys_now:?}");
        assert!(keys_now.iter().any(|key| key.starts_with("track-")), "the album's tracks");

        // The artist page: its header, its album rows and a window of its tracks.
        rt.update(|s, _| s.open_local_artist("Artist 2".to_owned()));
        for _ in 0..6 {
            rt.frame(1.0 / 120.0);
        }
        let keys_now = keys(&rt);
        assert!(keys_now.iter().any(|key| key == "local-artist-content-rows"), "{keys_now:?}");
        assert!(keys_now.iter().any(|key| key.starts_with("track-")), "the artist's tracks");
        assert!(keys_now.iter().any(|key| key.starts_with("local-artist-album-")), "the artist's albums");

        // A folder page: the header, its subfolders and a window of the folder's tracks.
        rt.update(|s, _| {
            s.page = Page::Local;
            s.set_local_tab(LocalTab::Folders);
            s.open_local_folder(PathBuf::from("/music"));
        });
        for _ in 0..6 {
            rt.frame(1.0 / 120.0);
        }
        let keys_now = keys(&rt);
        assert!(keys_now.iter().any(|key| key == "local-folder-up"), "{keys_now:?}");
        let rows = keys_now
            .iter()
            .filter(|key| key.starts_with("track-") && !key.starts_with("track-menu-"))
            .count();
        assert!(rows > 0 && rows < 60, "{rows} rows in a 2,000-track folder");
    }
}
