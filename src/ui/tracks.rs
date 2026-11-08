//! The shared track table — and the scaffolding every long page uses to stay cheap however
//! many rows it holds.
//!
//! Rows carry a hover highlight, a favourite heart, and artist and album names that navigate to
//! their pages. Double-clicking a row plays the whole table from that row through
//! [`Ui::play_tracks`], so next/previous follow the list the user clicked in. The row's number
//! becomes a round play button while the row is hovered, selected or playing; pressing it plays
//! the table from that row, and on the row that is playing it pauses instead.
//!
//! Every list of tracks — and every grid of cards — is a [`Page`]: the page's own blocks (its
//! header, its filter toolbar, a "Top tracks" heading, the trailing sections) and its rows are
//! rows of one [`El::virtual_rows`] list, which is the only child of the page's scroll viewport.
//! Layout builds just the rows near the viewport, so a commit costs one key check per row plus
//! the rows it mounts, however long the list is, and a page header scrolls away with the rows
//! instead of staying pinned. Row handlers look their item up by id, never by index: a row kept
//! while rows above it are inserted or removed still acts on its own track.
//!
//! The lists a page draws come from [`Tracks`] (a list the app mutates) or [`DerivedTracks`] /
//! [`Derived`] (a filtered, sorted view of other state, rebuilt only when its inputs change), so
//! a commit clones a pointer rather than every track. Both hand the view a [`TrackList`] /
//! [`IndexList`] whose generation is the page's row-key version: while it is unchanged, layout
//! skips reading a key per row (`El::virtual_rows_version`).

use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::hash::{Hash, Hasher};
use std::rc::Rc;

use rune::{Color, El, GrowthSpec, ScrollSpec, col, el, icon, row, text};

use crate::app::Ui;
use crate::theme::{CAPS, ICON_PAUSE, ICON_PLAY, SMALL, TRACK, Palette};
use crate::tidal::{Album, Artist, FavKind, Track};
use crate::ui::cards::{columns, cover_tile, heart_button, link_text, quality_badge};
use crate::ui::context_menu::{MenuState, TrackMenu, menu_button};
use crate::util::format_duration;

/// The gap between two rows of a page, in points (the track table's own row spacing).
const ROW_GAP: f32 = 1.0;
/// The width of a row's leading cell, which holds its number at rest and its round play button
/// when the row is hovered, selected or playing. Both fit it, so a hover never moves a column.
const NUMBER_W: f32 = 24.0;
/// The size of the cover a track row draws beside its number, and the width its column takes in
/// the table's column header. A page with [`Page::covers`] off drops the cell from both.
const COVER_W: f32 = 44.0;
/// The height a track row lays out at, which pages use as the estimate for rows no layout has
/// visited yet.
pub(crate) const TRACK_H: f32 = 56.0;
/// The heights a card grid's row is estimated at before layout has measured one: an album or
/// playlist tile at 168 points, an artist tile at 128, a 232-point video card.
pub(crate) const ALBUM_H: f32 = 230.0;
pub(crate) const PLAYLIST_H: f32 = 230.0;
pub(crate) const MIX_H: f32 = 230.0;
pub(crate) const ARTIST_H: f32 = 172.0;
pub(crate) const VIDEO_H: f32 = 180.0;
/// The gap between the cards of a grid row.
const GRID_GAP: f32 = 16.0;
/// The default page padding: the side margin, and the room above and below the rows.
const PAGE_PX: f32 = 48.0;
const PAGE_PT: f32 = 20.0;
const PAGE_PB: f32 = 48.0;

// ---------------------------------------------------------------------------------------------
// Shared track lists

/// A page's track list as the view draws it: one shared allocation, cloned by pointer however
/// long the list is.
#[derive(Clone)]
pub(crate) struct TrackList {
    pub(crate) rows: Rc<[Track]>,
    /// Bumped whenever the list changes, so layout re-reads the rows' keys only then.
    pub(crate) generation: u64,
}

/// The same for a list of indices into other state (a library's albums, say).
#[derive(Clone)]
pub(crate) struct IndexList {
    pub(crate) rows: Rc<[usize]>,
    pub(crate) generation: u64,
}

impl IndexList {
    /// Every index in order, for a list that is neither filtered nor sorted.
    pub(crate) fn all(len: usize) -> Self {
        IndexList { rows: (0..len).collect::<Vec<usize>>().into(), generation: 0 }
    }
}

/// A track list the app holds and mutates: its `Vec<Track>` and one shared copy the view draws,
/// rebuilt lazily after a change, so a commit costs a pointer clone and not a deep copy.
#[derive(Default)]
pub(crate) struct Tracks {
    items: Vec<Track>,
    shared: RefCell<Option<Rc<[Track]>>>,
    generation: Cell<u64>,
}

impl Tracks {
    /// Replaces the whole list.
    pub(crate) fn set(&mut self, items: Vec<Track>) {
        self.items = items;
        self.invalidate();
    }

    /// Empties the list.
    pub(crate) fn clear(&mut self) {
        self.set(Vec::new());
    }

    /// The list, for reading.
    pub(crate) fn items(&self) -> &[Track] {
        &self.items
    }

    /// The list, for editing. The shared copy is rebuilt on the next [`Tracks::shared`].
    pub(crate) fn edit(&mut self) -> &mut Vec<Track> {
        self.invalidate();
        &mut self.items
    }

    pub(crate) fn len(&self) -> usize {
        self.items.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// An owned copy of the list, for the play queue and the context menus.
    pub(crate) fn to_vec(&self) -> Vec<Track> {
        self.items.clone()
    }

    /// The list as the view draws it.
    pub(crate) fn shared(&self) -> TrackList {
        let rows = self
            .shared
            .borrow_mut()
            .get_or_insert_with(|| Rc::from(self.items.as_slice()))
            .clone();
        TrackList { rows, generation: self.generation.get() }
    }

    fn invalidate(&mut self) {
        self.shared.replace(None);
        self.generation.set(self.generation.get().wrapping_add(1));
    }
}

/// A derived track list — a filtered, sorted view of other state — recomputed only when the
/// inputs it was built from change. `version` must cover every input of `build`.
#[derive(Default)]
pub(crate) struct DerivedTracks {
    built: Cell<Option<u64>>,
    generation: Cell<u64>,
    rows: RefCell<Rc<[Track]>>,
}

impl DerivedTracks {
    /// The derived list for `version`, built by `build` when the version changed.
    pub(crate) fn get(&self, version: u64, build: impl FnOnce() -> Vec<Track>) -> TrackList {
        if self.built.get() != Some(version) {
            self.rows.replace(build().into());
            self.built.set(Some(version));
            self.generation.set(self.generation.get().wrapping_add(1));
        }
        TrackList { rows: self.rows.borrow().clone(), generation: self.generation.get() }
    }
}

/// The same for a derived list of indices into other state.
#[derive(Default)]
pub(crate) struct Derived {
    built: Cell<Option<u64>>,
    generation: Cell<u64>,
    rows: RefCell<Rc<[usize]>>,
}

impl Derived {
    /// The derived indices for `version`, built by `build` when the version changed.
    pub(crate) fn get(&self, version: u64, build: impl FnOnce() -> Vec<usize>) -> IndexList {
        if self.built.get() != Some(version) {
            self.rows.replace(build().into());
            self.built.set(Some(version));
            self.generation.set(self.generation.get().wrapping_add(1));
        }
        IndexList { rows: self.rows.borrow().clone(), generation: self.generation.get() }
    }

    /// The indices as they stand, without rebuilding anything: for a row that has to look up
    /// which item of the source list it stands for.
    pub(crate) fn peek(&self) -> Rc<[usize]> {
        self.rows.borrow().clone()
    }
}

/// A filtered, sorted view of a track library: the tracks a page draws, and the indices into the
/// library they came from, so Play and Shuffle can hand the very selection on screen to the
/// queue without searching for it again.
pub(crate) struct LibraryList {
    pub(crate) tracks: TrackList,
    pub(crate) indices: Rc<[usize]>,
}

/// The cache behind a [`LibraryList`], rebuilt only when its inputs change.
#[derive(Default)]
pub(crate) struct DerivedLibrary {
    built: Cell<Option<u64>>,
    generation: Cell<u64>,
    tracks: RefCell<Rc<[Track]>>,
    indices: RefCell<Rc<[usize]>>,
}

impl DerivedLibrary {
    /// The view for `version`: `select` returns the indices to show, and `tracks` maps them to
    /// the rendered tracks (both run only when the version changed).
    pub(crate) fn get(
        &self,
        version: u64,
        select: impl FnOnce() -> Vec<usize>,
        tracks: impl FnOnce(&[usize]) -> Vec<Track>,
    ) -> LibraryList {
        if self.built.get() != Some(version) {
            let indices = select();
            self.tracks.replace(tracks(&indices).into());
            self.indices.replace(indices.into());
            self.built.set(Some(version));
            self.generation.set(self.generation.get().wrapping_add(1));
        }
        LibraryList {
            tracks: TrackList { rows: self.tracks.borrow().clone(), generation: self.generation.get() },
            indices: self.indices.borrow().clone(),
        }
    }
}

/// The hash a derived list is keyed by, and a page's row-key version: whatever decides the
/// rows. [`Tracks`] and the derived caches hand back a generation, so a page's version is
/// simply that generation; this is for the inputs of a derived list.
pub(crate) fn version(parts: impl Hash) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    parts.hash(&mut hasher);
    hasher.finish()
}

/// The position of the track with `id` in `list`, preferring the occurrence nearest `near`. A
/// row's handler resolves its item this way instead of trusting the index it was built with, so
/// a row that outlives an insert, a removal or a reorder still acts on its own track; a list that
/// holds the same track more than once (TIDAL's favourites do) resolves to the occurrence nearest
/// the row the user pressed. `None` when the list no longer holds the track at all.
pub(crate) fn row_of(list: &[Track], id: u64, near: usize) -> Option<usize> {
    list.iter()
        .enumerate()
        .filter(|(_, track)| track.id == id)
        .map(|(index, _)| index)
        .min_by_key(|index| index.abs_diff(near))
}

// ---------------------------------------------------------------------------------------------
// A page: one virtual list

/// A block of a page that is not one of its list rows: the page header, the filter toolbar, a
/// section heading, the album's extras. It is built from the window's state when its row
/// mounts, so the blocks above the fold cost nothing once they are scrolled off.
pub(crate) type Block = Rc<dyn Fn(&Ui, Palette) -> El>;

/// One such block, keyed `key`, with the space the page leaves above and below it.
#[derive(Clone)]
pub(crate) struct Section {
    key: String,
    before: f32,
    after: f32,
    build: Block,
}

/// A block of a page, keyed `key` (its row key is `{page}-{key}`).
pub(crate) fn section(key: impl Into<String>, build: impl Fn(&Ui, Palette) -> El + 'static) -> Section {
    Section { key: key.into(), before: 0.0, after: 0.0, build: Rc::new(build) }
}

impl Section {
    /// Points of space above this block, on top of the list's [`ROW_GAP`].
    pub(crate) fn before(mut self, points: f32) -> Self {
        self.before = points;
        self
    }

    /// Points of space below this block, on top of the list's [`ROW_GAP`].
    pub(crate) fn after(mut self, points: f32) -> Self {
        self.after = points;
        self
    }
}

/// What a page's rows are, between its leading and trailing blocks.
enum Body {
    /// The shared track table: the `#`/`TITLE`/`ALBUM`/`TIME` column header when `columns`,
    /// then one row per track. `covers` draws the 44pt cover cell the table's rows and its
    /// column header share (the album page turns it off: its cover is in the header).
    Tracks { rows: Rc<[Track]>, columns: bool, covers: bool },
    /// One row per track, keyed by track id, built by the page's own cell: a list whose rows are
    /// not the shared table (the queue panel).
    TrackRows { rows: Rc<[Track]>, cell: Rc<dyn Fn(&Ui, Palette, usize, &Track) -> El> },
    /// `len` items, `per_row` of them to a row: a card grid ([`columns`] to a row) or a plain
    /// list of full-width rows (`per_row` 1).
    Items { len: usize, per_row: usize, cell: Rc<dyn Fn(&Ui, Palette, usize) -> El> },
    /// No rows at all: a page that shows its blocks only (an empty library, a failure note).
    Blocks,
}

/// A page whose content is one virtual list. `key` is the scroll viewport's element key; the
/// rows stack inside it is `{key}-rows`.
pub(crate) struct Page {
    key: String,
    leading: Vec<Section>,
    trailing: Vec<Section>,
    body: Body,
    /// The body list's generation, else 0.
    generation: u64,
    estimate: f32,
    /// Whether the shared table's rows draw their own covers (see [`Page::covers`]).
    covers: bool,
    px: f32,
    pt: f32,
    pb: f32,
}

/// A page holding the shared track table for `list`. `columns` draws the numbered column header
/// (pass `false` for an empty list, where the page shows a note instead of a table).
pub(crate) fn tracks_page(key: &str, list: TrackList, columns: bool, estimate: f32) -> Page {
    Page::new(key, Body::Tracks { rows: list.rows, columns, covers: true }, list.generation, estimate)
}

/// A page holding one row per track, keyed by track id, built by `cell`: for a list whose rows
/// are not the shared table (the queue panel's rows, with their own layout and actions).
pub(crate) fn track_rows_page(
    key: &str,
    list: TrackList,
    estimate: f32,
    cell: impl Fn(&Ui, Palette, usize, &Track) -> El + 'static,
) -> Page {
    Page::new(key, Body::TrackRows { rows: list.rows, cell: Rc::new(cell) }, list.generation, estimate)
}

/// A page holding a grid of cards from `0..len`, [`columns`] of them to a row.
pub(crate) fn grid_page(
    s: &Ui,
    key: &str,
    list: IndexList,
    estimate: f32,
    cell: impl Fn(&Ui, Palette, usize) -> El + 'static,
) -> Page {
    let per_row = columns(s).max(1);
    Page::new(
        key,
        Body::Items { len: list.rows.len(), per_row, cell: Rc::new(cell) },
        list.generation,
        estimate,
    )
}

/// A page holding one full-width row per index of `len` (the local artists list, folder rows).
pub(crate) fn rows_page(
    key: &str,
    len: usize,
    generation: u64,
    estimate: f32,
    cell: impl Fn(&Ui, Palette, usize) -> El + 'static,
) -> Page {
    Page::new(key, Body::Items { len, per_row: 1, cell: Rc::new(cell) }, generation, estimate)
}

/// A page with no list of its own: the blocks alone.
pub(crate) fn blocks_page(key: &str) -> Page {
    Page::new(key, Body::Blocks, 0, 0.0)
}

/// A hash of every carousel's paging offset and of the cards across: part of a page's row
/// version, because a carousel is a block inside a cached row and must be rebuilt when it pages.
/// The offsets are summed, so the order of the map does not matter.
fn carousel_stamp(s: &Ui) -> u64 {
    use std::hash::Hasher;
    let offsets = s.carousel_offsets.iter().fold(0u64, |sum, (key, offset)| {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        key.hash(&mut hasher);
        offset.hash(&mut hasher);
        sum.wrapping_add(hasher.finish())
    });
    version((offsets, s.columns.get()))
}

impl Page {
    fn new(key: &str, body: Body, generation: u64, estimate: f32) -> Self {
        Page {
            key: key.to_owned(),
            leading: Vec::new(),
            trailing: Vec::new(),
            body,
            generation,
            estimate,
            covers: true,
            px: PAGE_PX,
            pt: PAGE_PT,
            pb: PAGE_PB,
        }
    }

    /// A block above the page's rows, in order.
    pub(crate) fn leading(mut self, section: Section) -> Self {
        self.leading.push(section);
        self
    }

    /// Blocks above the ones already added: a page that builds its own header blocks and then
    /// has the tab strip and toolbar put in front of them.
    pub(crate) fn leading_first(mut self, sections: Vec<Section>) -> Self {
        self.leading.splice(0..0, sections);
        self
    }

    /// A block below the page's rows, in order.
    pub(crate) fn trailing(mut self, section: Section) -> Self {
        self.trailing.push(section);
        self
    }

    /// The page's padding: the side margin and the room above and below the rows.
    pub(crate) fn padding(mut self, px: f32, pt: f32, pb: f32) -> Self {
        self.px = px;
        self.pt = pt;
        self.pb = pb;
        self
    }

    /// Whether the shared table's rows draw their own cover, and its column header keeps the
    /// cover column (the default). The album page turns it off: its cover is in the header, and
    /// every row would repeat the same picture, so the title column starts right after the
    /// number — the column header follows it, so the table stays aligned.
    pub(crate) fn covers(mut self, covers: bool) -> Self {
        self.covers = covers;
        self
    }

    /// The page: a scroll viewport holding the rows list alone. `s` is the window's state; the
    /// rows themselves are built by rune's own call, but the carousels' paging offsets are part
    /// of the row version, so paging a carousel rebuilds the block that holds it.
    pub(crate) fn render(&self, s: &Ui, p: Palette) -> El {
        let plan = Rc::new(Plan {
            key: self.key.clone(),
            leading: self.leading.clone(),
            trailing: self.trailing.clone(),
            body: match &self.body {
                Body::Tracks { rows, columns, .. } => {
                    Body::Tracks { rows: rows.clone(), columns: *columns, covers: self.covers }
                }
                Body::TrackRows { rows, cell } => {
                    Body::TrackRows { rows: rows.clone(), cell: cell.clone() }
                }
                Body::Items { len, per_row, cell } => {
                    Body::Items { len: *len, per_row: *per_row, cell: cell.clone() }
                }
                Body::Blocks => Body::Blocks,
            },
        });
        let count = plan.count();
        let columns = matches!(plan.body, Body::Tracks { columns: true, .. });
        let version = version((
            self.generation,
            count,
            plan.leading.len(),
            plan.trailing.len(),
            columns,
            self.covers,
            carousel_stamp(s),
        ));
        let key_plan = plan.clone();
        let build_plan = plan.clone();
        let rows_key = format!("{}-rows", self.key);
        let stack = col()
            .key(rows_key)
            .w_full()
            .px(self.px)
            .pt(self.pt)
            .pb(self.pb)
            .gap(ROW_GAP)
            .growth(GrowthSpec::default())
            .virtual_rows(
                count,
                self.estimate.max(1.0),
                move |_: &Ui, index| Cow::Owned(key_plan.key_of(index)),
                move |s: &Ui, index| build_plan.row(s, p, index),
            )
            .virtual_rows_version(version);
        col()
            .key(self.key.clone())
            .w_full()
            .grow(1.0)
            .min_h(0.0)
            .scroll(ScrollSpec { stick_bottom: false })
            .child(stack)
    }
}

/// The rows of a rendered page: counts, keys and builders, all read from state so the list
/// captures nothing borrowed.
struct Plan {
    key: String,
    leading: Vec<Section>,
    trailing: Vec<Section>,
    body: Body,
}

impl Plan {
    /// The rows before the body: the leading blocks and the column header.
    fn head(&self) -> usize {
        match &self.body {
            Body::Tracks { columns, .. } => self.leading.len() + usize::from(*columns),
            Body::TrackRows { .. } | Body::Items { .. } | Body::Blocks => self.leading.len(),
        }
    }

    /// The body's row count.
    fn body_len(&self) -> usize {
        match &self.body {
            Body::Tracks { rows, .. } | Body::TrackRows { rows, .. } => rows.len(),
            Body::Items { len, per_row, .. } => len.div_ceil((*per_row).max(1)),
            Body::Blocks => 0,
        }
    }

    fn count(&self) -> usize {
        self.head() + self.body_len() + self.trailing.len()
    }

    /// Row `index`'s key: a track's id, a block's own key, or a body row's number. Layout keeps
    /// a row's measured height while its key stays.
    fn key_of(&self, index: usize) -> String {
        let head = self.head();
        if index < self.leading.len() {
            return format!("{}-{}", self.key, self.leading[index].key);
        }
        if index < head {
            return format!("{}-columns", self.key);
        }
        let len = self.body_len();
        if index < head + len {
            let at = index - head;
            return match &self.body {
                Body::Tracks { rows, .. } | Body::TrackRows { rows, .. } => {
                    format!("track-{}", rows[at].id)
                }
                _ => format!("{}-row-{at}", self.key),
            };
        }
        format!("{}-{}", self.key, self.trailing[index - head - len].key)
    }

    /// Row `index`'s element.
    fn row(&self, s: &Ui, p: Palette, index: usize) -> El {
        let head = self.head();
        if index < self.leading.len() {
            return block_row(&self.leading[index], s, p);
        }
        if index < head {
            return column_header(p, self.track_covers());
        }
        let len = self.body_len();
        if index < head + len {
            return self.body_row(s, p, index - head);
        }
        block_row(&self.trailing[index - head - len], s, p)
    }

    /// Whether the shared table's rows and its column header draw a cover column.
    fn track_covers(&self) -> bool {
        match &self.body {
            Body::Tracks { covers, .. } => *covers,
            _ => true,
        }
    }

    fn body_row(&self, s: &Ui, p: Palette, index: usize) -> El {
        match &self.body {
            Body::Tracks { rows, covers, .. } => track_row(s, p, index, rows, *covers),
            Body::TrackRows { rows, cell } => cell(s, p, index, &rows[index]),
            Body::Items { len, per_row, cell } => {
                let per_row = (*per_row).max(1);
                let mut line = row().w_full().gap(GRID_GAP).items_start();
                for slot in 0..per_row {
                    let item = index * per_row + slot;
                    if item < *len {
                        line = line.child(cell(s, p, item));
                    } else {
                        // An empty slot in the last row still holds its share of the width, so a
                        // card that grows (a mix or a track card, unlike the fixed-width album,
                        // artist, playlist and video cards) keeps the size it has in a full row.
                        line = line.child(el().grow(1.0).min_w(0.0));
                    }
                }
                line
            }
            Body::Blocks => el(),
        }
    }
}

/// One block's row: its element, with the page's own spacing around it.
fn block_row(section: &Section, s: &Ui, p: Palette) -> El {
    let mut block = col().w_full();
    if section.before > 0.0 {
        block = block.pt(section.before);
    }
    if section.after > 0.0 {
        block = block.pb(section.after);
    }
    block.child((section.build)(s, p))
}

/// The table's column header, aligned with [`track_row`]'s columns. `covers` keeps the cover
/// column, which [`track_row`] then draws in every row.
fn column_header(p: Palette, covers: bool) -> El {
    let header = row()
        .w_full()
        .h(30.0)
        .px(12.0)
        .gap(16.0)
        .center_y()
        .child(text("#").w(24.0).font(CAPS).color(p.faint));
    let header = if covers { header.child(el().w(COVER_W)) } else { header };
    header
        .child(text("TITLE").grow(4.0).font(CAPS).color(p.faint))
        .child(text("ALBUM").grow(2.0).font(CAPS).color(p.faint))
        .child(text("TIME").w(48.0).font(CAPS).color(p.faint))
        .child(el().w(48.0))
        .child(el().w(32.0))
        .child(el().w(32.0))
}

/// One track row: the number (or the play button the row shows while it is hovered, selected or
/// playing), the cover when the page draws one (`covers`), title, artist and album links,
/// duration and quality, the favourite heart and the "…" menu.
fn track_row(s: &Ui, p: Palette, index: usize, rows: &Rc<[Track]>, covers: bool) -> El {
    let track = &rows[index];
    let id = track.id;
    let selected = s.selected == Some(id);

    let artist = track.artist_id.map(|artist_id| Artist {
        id: artist_id,
        name: track.artist.clone(),
        picture: None,
    });
    let album = track.album_id.map(|album_id| Album {
        id: album_id,
        title: track.album.clone(),
        cover: track.cover.clone(),
        ..Album::default()
    });

    let body = match artist {
        Some(artist) => link_text(p, track.artist.clone(), SMALL, move |s: &mut Ui| {
            s.open_artist_with(artist.clone())
        })
        .key(format!("artist-link-{id}")),
        None => text(track.artist.clone()).font(SMALL).color(p.dim).truncate(),
    };
    let album_cell = match album {
        Some(album) => link_text(p, track.album.clone(), SMALL, move |s: &mut Ui| {
            s.open_album_with(album.clone())
        })
        .key(format!("album-link-{id}"))
        .grow(2.0)
        .min_w(0.0),
        None => text(track.album.clone()).font(SMALL).color(p.dim).grow(2.0).min_w(0.0).truncate(),
    };

    // The menu resolves the row's place in the list from state by id when it opens, so a row
    // that outlives a reorder or an insert still acts on its own track.
    let menu_key = format!("track-menu-{id}");
    let row_menu = TrackMenu {
        track: track.clone(),
        list: Some(rows.clone()),
        index,
        playlist: None,
        queue: false,
    };
    // Local files have no TIDAL favourite: the same 32px cell stays empty so columns line up.
    let favourite = if crate::local::is_local_id(id) {
        el().w(32.0)
    } else {
        heart_button(s, p, FavKind::Track, &id.to_string(), 32.0)
    };
    let list = rows.clone();
    let mut line = row()
        .key(format!("track-{id}"))
        .w_full()
        .h(TRACK_H)
        .px(12.0)
        .gap(16.0)
        .center_y()
        .radius(6.0)
        .fill(if selected { p.card } else { Color::TRANSPARENT })
        .hover(p.card_hover)
        .on_press(move |s: &mut Ui| s.selected = Some(id))
        // The row under the pointer shows its play button: the number is replaced while the
        // pointer rests here (the leave of the row we were on is ignored, so entering one row
        // while leaving another always leaves the entered row's id in place).
        .on_hover_change(move |s: &mut Ui, inside, _tx| {
            if inside {
                s.row_hover = Some(id);
            } else if s.row_hover == Some(id) {
                s.row_hover = None;
            }
        })
        .on_double_press(move |s: &mut Ui| {
            // Find the row's place by id: the row may have been built at another index.
            let start = row_of(&list, id, index).unwrap_or(0);
            s.play_tracks(list.to_vec(), start);
        })
        .on_secondary_press({
            // Right-click opens the same menu as the "…" button, at the pointer.
            let menu = row_menu.clone();
            let key = menu_key.clone();
            move |s: &mut Ui, press: rune::PointerPress, _: &mut rune::Tx| {
                s.open_menu(MenuState::track(key.clone(), menu.clone()).at(press))
            }
        })
        .child(number_cell(s, p, index, track, rows));
    // A page that already shows the album's cover above the list leaves it out of every row,
    // so the title column starts right after the number (its column header does the same).
    if covers {
        line = line.child(cover_tile(s, p, track.cover.as_deref(), COVER_W));
    }
    line
        .child(
            col().grow(4.0).min_w(0.0).gap(3.0)
                .child(text(track.title.clone()).font(TRACK).color(p.text).truncate())
                .child(body),
        )
        .child(album_cell)
        .child(
            text(track.duration.map(format_duration).unwrap_or_default())
                .w(48.0)
                .font(SMALL)
                .color(p.dim),
        )
        .child(quality_cell(p, track))
        .child(favourite)
        .child(menu_button(p, &menu_key, "Track options", row_menu))
}

/// The row's leading cell: the track's number at rest, and — while the row is hovered, selected
/// or the track that is playing — the round play button the row shows in its place, as Apple
/// Music and Spotify do. The button plays the whole table from this row through
/// [`Ui::play_tracks`], so next/previous follow the list the user pressed in; on the row that is
/// the current track it pauses (or resumes) instead. Both fill the same [`NUMBER_W`] cell, so a
/// hover never moves the columns after it.
fn number_cell(s: &Ui, p: Palette, index: usize, track: &Track, rows: &Rc<[Track]>) -> El {
    let id = track.id;
    let current = s.now_playing.as_ref().is_some_and(|playing| playing.id == id);
    // This row's track is the one the worker is opening: the button waits it out rather than
    // asking for it again.
    let pending = s.pending_play.as_ref().is_some_and(|playing| playing.id == id);
    if !(current || pending || s.row_hover == Some(id) || s.selected == Some(id)) {
        return text(format!("{}", index + 1))
            .key(format!("number-{id}"))
            .w(NUMBER_W)
            .font(SMALL)
            .color(p.faint);
    }
    // The playing row's button pauses; every other row's plays from where it stands.
    let pause = current && !s.paused;
    let list = rows.clone();
    let mut button = el()
        .key(format!("track-play-{id}"))
        .role("button")
        .label(if pause {
            format!("Pause {}", track.title)
        } else {
            format!("Play {} from here", track.title)
        })
        .size(NUMBER_W, NUMBER_W)
        .radius(NUMBER_W / 2.0)
        .center()
        .fill(if current { p.accent } else { Color::TRANSPARENT })
        .hover(if current { p.accent.mix(Color::WHITE, 0.12) } else { p.card_hover })
        .press(p.accent.alpha(0.25))
        .focusable(!pending)
        .disabled(pending)
        .on_press(move |s: &mut Ui| {
            // Read the state, not the build: the row may have been built before the track it
            // stands for started, or before it began playing.
            if s.now_playing.as_ref().is_some_and(|playing| playing.id == id) {
                s.toggle_pause();
            } else if !s.pending_play.as_ref().is_some_and(|playing| playing.id == id) {
                // Find the row's place by id: the row may have been built at another index.
                let start = row_of(&list, id, index).unwrap_or(0);
                s.play_tracks(list.to_vec(), start);
            }
        });
    button = if pending {
        button.child(text("…").font(SMALL).color(p.dim))
    } else {
        button.child(
            icon(if pause { &ICON_PAUSE } else { &ICON_PLAY }, 13.0)
                .color(if current { p.accent_ink } else { p.text }),
        )
    };
    button
}

/// The MAX/HI-RES badge for a row, or an empty cell so the columns stay aligned.
fn quality_cell(p: Palette, track: &Track) -> El {
    let cell = el().w(48.0).h(18.0).center();
    match crate::util::quality_badge(track.quality.as_deref()) {
        Some(label) => cell.child(quality_badge(p, label)),
        None => cell,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{NowPlaying, init};
    use crate::theme::palette;
    use std::time::{Duration, Instant};

    /// The page under a full-height root, as a page's window provides it.
    fn rows_view(s: &Ui, _: &mut rune::Cx<Ui>) -> El {
        page_view(s, true)
    }

    /// The same page with the table's cover column turned off, as the album page does.
    fn bare_rows_view(s: &Ui, _: &mut rune::Cx<Ui>) -> El {
        page_view(s, false)
    }

    fn page_view(s: &Ui, covers: bool) -> El {
        col().w_full().h_full().child(
            tracks_page("results-page", s.results.shared(), true, TRACK_H)
                .covers(covers)
                .render(s, palette()),
        )
    }

    fn keys_for(track: Track) -> Vec<String> {
        let mut s = init();
        s.results.set(vec![track]);
        let mut rt = rune::Runtime::new(s, rows_view, (900.0, 300.0), rune::Env::new(true, 1.0));
        rt.frame(0.0);
        rt.elements().into_iter().filter_map(|element| element.key).collect()
    }

    /// A synthetic track list of `count` tracks.
    fn tracks(count: usize) -> Tracks {
        let mut list = Tracks::default();
        list.set(
            (0..count)
                .map(|index| Track {
                    id: 1_000_000 + index as u64,
                    title: format!("Track {index}"),
                    artist: format!("Artist {}", index % 50),
                    album: format!("Album {}", index % 200),
                    duration: Some(180 + (index % 120) as u64),
                    ..Track::default()
                })
                .collect(),
        );
        list
    }

    fn page_of(count: usize) -> rune::Runtime<Ui> {
        let mut s = init();
        s.results = tracks(count);
        let mut rt = rune::Runtime::new(s, rows_view, (900.0, 700.0), rune::Env::new(true, 1.0));
        rt.frame(0.0);
        rt
    }

    /// The rows mounted right now.
    fn mounted(rt: &rune::Runtime<Ui>) -> Vec<String> {
        rt.elements()
            .into_iter()
            .filter_map(|element| element.key)
            .filter(|key| key.starts_with("track-") && !key.starts_with("track-menu-") && !key.starts_with("track-play-"))
            .collect()
    }

    /// The point and key of the first row under `y` in the window, and nothing above it.
    fn row_at(rt: &rune::Runtime<Ui>, y: f32) -> (String, f32, f32) {
        for element in rt.elements() {
            let Some(key) = element.key else { continue };
            if !key.starts_with("track-") || key.starts_with("track-menu-") || key.starts_with("track-play-") {
                continue;
            }
            let [x, top, _, height] = element.rect;
            if top <= y && y <= top + height {
                return (key, x + 20.0, top + height / 2.0);
            }
        }
        panic!("no row covers y = {y}");
    }

    /// A right-click on a row opens that row's context menu (the same one its "…" button opens),
    /// for the track it stands for.
    #[test]
    fn right_clicking_a_row_opens_its_context_menu() {
        let _env = crate::settings::lock_config_env();
        let mut rt = page_of(200);
        for _ in 0..8 {
            rt.frame(1.0 / 120.0);
        }
        let (key, x, y) = row_at(&rt, 300.0);
        let expected: u64 = key.strip_prefix("track-").unwrap().parse().unwrap();
        assert!(rt.state.menu.is_none(), "no menu before the click");
        rt.pointer_move(x, y);
        rt.frame(1.0 / 120.0);
        rt.secondary_down(x, y);
        rt.secondary_up(x, y);
        rt.frame(1.0 / 120.0);
        let menu = rt.state.menu.as_ref().expect("the right-click opened a menu");
        match &menu.target {
            crate::ui::context_menu::MenuTarget::Track(track) => assert_eq!(track.track.id, expected),
            _ => panic!("a track row opens a track menu"),
        }
    }

    /// Main's rule: a local file's row has no heart at all, while a TIDAL row keeps one.
    #[test]
    fn local_track_rows_have_no_heart() {
        // `init` reads the settings file, whose directory the environment names.
        let _env = crate::settings::lock_config_env();
        let id = crate::local::local_id(std::path::Path::new("/music/song.flac"));
        let local = Track {
            id,
            title: "Song".to_owned(),
            artist: "Artist".to_owned(),
            album: "Album".to_owned(),
            duration: Some(120),
            ..Track::default()
        };
        let keys = keys_for(local);
        assert!(
            !keys.iter().any(|key| key.starts_with("heart-")),
            "a local row must not draw a heart: {keys:?}"
        );
        assert!(keys.iter().any(|key| key == &format!("track-{id}")), "the row itself is there");

        let tidal = Track { id: 7, title: "Song".to_owned(), ..Track::default() };
        let keys = keys_for(tidal);
        assert!(keys.iter().any(|key| key == "heart-track-7"), "TIDAL rows keep their heart: {keys:?}");
    }

    /// A 5,000-track page mounts a window of rows, not the list: layout builds only what is near
    /// the viewport, so the page's cost cannot grow with the list.
    #[test]
    fn a_long_page_mounts_only_a_window_of_rows() {
        // `init` reads the settings file, whose directory the environment names.
        let _env = crate::settings::lock_config_env();
        let mut rt = page_of(5_000);
        for _ in 0..8 {
            rt.frame(1.0 / 120.0);
        }
        let rows = mounted(&rt);
        assert!(!rows.is_empty(), "the viewport's rows are there");
        assert!(rows.len() < 60, "{} rows of a 5,000-row page are mounted", rows.len());
        assert!(rt.elements().len() < 700, "{} elements for a 5,000-row page", rt.elements().len());
    }

    /// Pressing a row after scrolling plays the track that row stands for, though the rows above
    /// and below it were never built.
    #[test]
    fn pressing_a_row_in_the_middle_plays_that_track() {
        // `init` reads the settings file, whose directory the environment names.
        let _env = crate::settings::lock_config_env();
        let mut rt = page_of(5_000);
        for _ in 0..8 {
            rt.frame(1.0 / 120.0);
        }
        // Scroll a long way down (a negative delta scrolls down), then let the window settle.
        for _ in 0..60 {
            rt.wheel(450.0, 400.0, -2_500.0);
            rt.frame(1.0 / 120.0);
        }
        for _ in 0..10 {
            rt.frame(1.0 / 120.0);
        }
        let (key, x, y) = row_at(&rt, 350.0);
        let expected: u64 = key.strip_prefix("track-").unwrap().parse().unwrap();
        assert!(expected > 1_001_000, "the row is well into the list: {key}");
        // A double press on the row: the press that pairs into one is inside the interval.
        for _ in 0..2 {
            rt.pointer_down(x, y);
            rt.pointer_up(x, y);
            rt.frame(1.0 / 120.0);
        }
        assert_eq!(rt.state.queue.current_id(), Some(expected), "the pressed row's track plays");
    }

    /// Hovering a row of a long virtual page replaces its number with the play button, without
    /// moving the columns, and pressing that button plays the track the row stands for — the
    /// rows above and below it were never built.
    #[test]
    fn pressing_the_play_button_of_a_mid_row_plays_that_track() {
        // `init` reads the settings file, whose directory the environment names.
        let _env = crate::settings::lock_config_env();
        let mut rt = page_of(5_000);
        for _ in 0..8 {
            rt.frame(1.0 / 120.0);
        }
        for _ in 0..60 {
            rt.wheel(450.0, 400.0, -2_500.0);
            rt.frame(1.0 / 120.0);
        }
        for _ in 0..10 {
            rt.frame(1.0 / 120.0);
        }
        let rect = |rt: &rune::Runtime<Ui>, key: &str| {
            rt.elements()
                .into_iter()
                .find(|element| !element.exiting && element.key.as_deref() == Some(key))
                .map(|element| element.rect)
        };
        let (key, x, y) = row_at(&rt, 350.0);
        let expected: u64 = key.strip_prefix("track-").unwrap().parse().unwrap();
        assert!(expected > 1_001_000, "the row is well into the list: {key}");
        // At rest the row shows its number, and no button at all.
        let row_before = rect(&rt, &key).expect("the row is mounted");
        let number = rect(&rt, &format!("number-{expected}")).expect("a resting row shows its number");
        assert_eq!(number[2], NUMBER_W, "the number's cell is the width a button needs");
        assert!(rect(&rt, &format!("track-play-{expected}")).is_none(), "no button before the hover");
        // The pointer rests on the row: the number gives way to the play button.
        rt.pointer_move(x, y);
        for _ in 0..3 {
            rt.frame(1.0 / 120.0);
        }
        assert_eq!(rt.state.row_hover, Some(expected), "the row under the pointer is hovered");
        assert!(rect(&rt, &format!("number-{expected}")).is_none(), "the number gives way to the button");
        let button = rect(&rt, &format!("track-play-{expected}")).expect("the hovered row shows its play button");
        // The cell keeps its place and width, so nothing after it moves.
        assert_eq!(rect(&rt, &key).expect("the row is still mounted"), row_before, "the row does not move");
        assert_eq!([button[0], button[2]], [number[0], number[2]], "the number's column keeps its width");
        // Press the button itself: the list plays from this row, though it is deep in the page.
        let (bx, by) = rt.find(&format!("track-play-{expected}")).unwrap();
        rt.pointer_down(bx, by);
        rt.pointer_up(bx, by);
        rt.frame(1.0 / 120.0);
        assert_eq!(rt.state.queue.current_id(), Some(expected), "the row's track plays from its place");
    }

    /// The album page turns the table's covers off: the rows and the column header both drop the
    /// cover column, so the title starts right after the number — and exactly where the header's
    /// own TITLE does — while the columns to the right of the growing title/album cells stay put.
    #[test]
    fn a_page_without_row_covers_drops_the_column_without_moving_the_others() {
        // `init` reads the settings file, whose directory the environment names.
        let _env = crate::settings::lock_config_env();
        let rect = |rt: &rune::Runtime<Ui>, key: &str| {
            rt.elements()
                .into_iter()
                .find(|element| !element.exiting && element.key.as_deref() == Some(key))
                .unwrap_or_else(|| panic!("no element keyed {key}"))
                .rect
        };
        let text_at = |rt: &rune::Runtime<Ui>, text: &str| {
            rt.elements()
                .into_iter()
                .find(|element| !element.exiting && element.text.as_deref() == Some(text))
                .unwrap_or_else(|| panic!("no element showing {text}"))
                .rect[0]
        };
        let page = |view: fn(&Ui, &mut rune::Cx<Ui>) -> El, rows: Tracks| {
            let mut s = init();
            s.results = rows;
            let mut rt = rune::Runtime::new(s, view, (900.0, 700.0), rune::Env::new(true, 1.0));
            for _ in 0..4 {
                rt.frame(0.0);
            }
            rt
        };
        let with = page(rows_view, tracks(40));
        let without = page(bare_rows_view, tracks(40));
        // The last 24 rows are the trailing ones either way: layout estimates them at TRACK_H.
        let row = format!("track-{}", 1_000_000);
        assert_eq!(
            rect(&with, &row)[..3],
            rect(&without, &row)[..3],
            "the row keeps its place and width"
        );
        // The heart closes the row's columns: it does not move when the cover goes.
        let heart = "heart-track-1000000";
        assert_eq!(rect(&with, heart)[0], rect(&without, heart)[0], "the columns after the title stay put");
        // The title column starts one cover (and its gap) earlier, in the row and in the header.
        let shift = COVER_W + 16.0;
        assert_eq!(
            text_at(&with, "Track 0") - text_at(&without, "Track 0"),
            shift,
            "the title starts where the cover was"
        );
        assert_eq!(
            text_at(&with, "TITLE") - text_at(&without, "TITLE"),
            shift,
            "and the column header follows it"
        );
        assert_eq!(
            text_at(&without, "TITLE"),
            text_at(&without, "Track 0"),
            "the row's title sits under the header's TITLE"
        );
    }

    /// The numbers behind the performance note: at 350, 5,000 and 20,000 tracks the first
    /// commit, an idle commit after an unrelated field changed, and a scroll step all stay
    /// roughly constant (the idle one is the per-tick commit while a track plays). Run with
    /// `cargo test -- --ignored --nocapture long_list`.
    #[test]
    #[ignore = "timing benchmark: run with --ignored --nocapture long_list"]
    fn long_list_commit_and_scroll_cost() {
        // `init` reads the settings file, whose directory the environment names.
        let _env = crate::settings::lock_config_env();
        for count in [350usize, 5_000, 20_000] {
            let mut rt = page_of(count);
            // The first commit builds the page: the view plus the viewport's rows.
            let first = {
                let mut s = init();
                s.results = tracks(count);
                let mut fresh = rune::Runtime::new(s, rows_view, (900.0, 700.0), rune::Env::new(true, 1.0));
                let start = Instant::now();
                fresh.frame(0.0);
                start.elapsed()
            };
            // An idle commit: the clock ticks (the played track's elapsed second) and the view
            // runs again with nothing about the list changed.
            let playing = NowPlaying {
                id: 1_000_000,
                title: "Track 0".to_owned(),
                artist: "Artist 0".to_owned(),
                album: "Album 0".to_owned(),
                cover: None,
                album_id: None,
                artist_id: None,
                elapsed: 0,
                duration: Some(180),
                quality: None,
                output: None,
            };
            rt.state.now_playing = Some(playing);
            for _ in 0..10 {
                rt.frame(1.0 / 60.0);
            }
            let (idle_mean, idle_max) = time(&mut rt, 30, |rt| {
                if let Some(playing) = &mut rt.state.now_playing {
                    playing.elapsed += 1;
                }
                rt.invalidate();
            });
            let (scroll_mean, scroll_max) = time(&mut rt, 60, |rt| {
                rt.wheel(450.0, 350.0, -700.0);
            });
            println!(
                "{count:>6} tracks: first commit {first:?}, idle mean {idle_mean:?} max {idle_max:?}, \
                 scroll mean {scroll_mean:?} max {scroll_max:?}"
            );
        }
    }

    /// `frames` timed commits, each asking for a commit first (as a real change does).
    fn time(
        rt: &mut rune::Runtime<Ui>,
        frames: usize,
        mut step: impl FnMut(&mut rune::Runtime<Ui>),
    ) -> (Duration, Duration) {
        let mut total = Duration::ZERO;
        let mut worst = Duration::ZERO;
        for _ in 0..frames {
            step(rt);
            let start = Instant::now();
            rt.frame(1.0 / 60.0);
            let taken = start.elapsed();
            total += taken;
            worst = worst.max(taken);
        }
        (total / frames as u32, worst)
    }
}
