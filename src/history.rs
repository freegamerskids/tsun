//! Navigation history for the top-bar Back and Forward buttons.
//!
//! Every place the user can be is a [`Route`]: a [`Page`] plus the id payload
//! that page needs to be reopened (`Route::Album(42)`, `Route::Playlist(uuid)`,
//! …). [`History`] keeps the visited routes in a list with a cursor, so going
//! back and then somewhere new drops the forward entries, as a browser does.

use crate::app::Page;

/// A place in the app: the page kind and the id it is showing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Route {
    Home,
    Search,
    Explore,
    Collection,
    Album(u64),
    Artist(u64),
    Playlist(String),
    Mix(String),
    /// The Local files library, and its album/artist drill-downs.
    Local,
    LocalAlbum(String),
    LocalArtist(String),
    /// One carousel's whole list, opened by its "View all" button. The id names the snapshot the
    /// page holds in `Ui::view_all`.
    ViewAll(u64),
}

impl Route {
    /// A stable, single-token form of this route, persisted in settings so
    /// "Last visited" can restore it. Ids follow a colon: `album:42`,
    /// `playlist:<uuid>`.
    pub(crate) fn persist(&self) -> String {
        match self {
            Route::Home => "home".to_owned(),
            Route::Search => "search".to_owned(),
            Route::Explore => "explore".to_owned(),
            Route::Collection => "collection".to_owned(),
            Route::Album(id) => format!("album:{id}"),
            Route::Artist(id) => format!("artist:{id}"),
            Route::Playlist(id) => format!("playlist:{id}"),
            Route::Mix(id) => format!("mix:{id}"),
            Route::Local => "local".to_owned(),
            Route::LocalAlbum(key) => format!("localalbum:{key}"),
            Route::LocalArtist(name) => format!("localartist:{name}"),
            Route::ViewAll(id) => format!("viewall:{id}"),
        }
    }

    /// The route a [`Route::persist`] string names, or `None` when it does not
    /// name one (an older or hand-edited settings file).
    pub(crate) fn restore(value: &str) -> Option<Route> {
        let (kind, id) = match value.split_once(':') {
            Some((kind, id)) => (kind, Some(id)),
            None => (value, None),
        };
        match (kind, id) {
            ("home", _) => Some(Route::Home),
            ("search", _) => Some(Route::Search),
            ("explore", _) => Some(Route::Explore),
            ("collection", _) => Some(Route::Collection),
            ("album", Some(id)) => id.parse().ok().map(Route::Album),
            ("artist", Some(id)) => id.parse().ok().map(Route::Artist),
            ("playlist", Some(id)) if !id.is_empty() => Some(Route::Playlist(id.to_owned())),
            ("mix", Some(id)) if !id.is_empty() => Some(Route::Mix(id.to_owned())),
            ("local", _) => Some(Route::Local),
            ("localalbum", Some(key)) if !key.is_empty() => Some(Route::LocalAlbum(key.to_owned())),
            ("localartist", Some(name)) if !name.is_empty() => Some(Route::LocalArtist(name.to_owned())),
            _ => None,
        }
    }

    /// The [`Page`] this route selects in `ui::content`.
    pub(crate) fn page(&self) -> Page {
        match self {
            Route::Home => Page::Home,
            Route::Search => Page::Search,
            Route::Explore => Page::Explore,
            Route::Collection => Page::Collection,
            Route::Album(_) => Page::Album,
            Route::Artist(_) => Page::Artist,
            Route::Playlist(_) => Page::Playlist,
            Route::Mix(_) => Page::Mix,
            Route::Local => Page::Local,
            Route::LocalAlbum(_) => Page::LocalAlbum,
            Route::LocalArtist(_) => Page::LocalArtist,
            Route::ViewAll(_) => Page::ViewAll,
        }
    }
}

/// The visited routes, oldest first, with the index of the current one.
#[derive(Debug, Default)]
pub(crate) struct History {
    entries: Vec<Route>,
    current: Option<usize>,
}

impl History {
    /// Makes `route` the current entry, dropping any forward entries. Returns
    /// `false` when `route` is already current, in which case nothing changes.
    pub(crate) fn push(&mut self, route: Route) -> bool {
        if self.current().is_some_and(|current| *current == route) {
            return false;
        }
        let at = self.current.map_or(0, |index| index + 1);
        self.entries.truncate(at);
        self.entries.push(route);
        self.current = Some(self.entries.len() - 1);
        true
    }

    /// The route the user is on, when there is any history.
    pub(crate) fn current(&self) -> Option<&Route> {
        self.current.and_then(|index| self.entries.get(index))
    }

    pub(crate) fn can_back(&self) -> bool {
        self.current.is_some_and(|index| index > 0)
    }

    pub(crate) fn can_forward(&self) -> bool {
        self.current.is_some_and(|index| index + 1 < self.entries.len())
    }

    /// Steps one entry back and returns it, or `None` at the start.
    pub(crate) fn back(&mut self) -> Option<Route> {
        let next = self.current?.checked_sub(1)?;
        self.current = Some(next);
        self.entries.get(next).cloned()
    }

    /// Steps one entry forward and returns it, or `None` at the end.
    pub(crate) fn forward(&mut self) -> Option<Route> {
        let next = self.current? + 1;
        if next >= self.entries.len() {
            return None;
        }
        self.current = Some(next);
        self.entries.get(next).cloned()
    }

    /// Forgets everything and starts again at `route`.
    pub(crate) fn reset(&mut self, route: Route) {
        self.entries.clear();
        self.entries.push(route);
        self.current = Some(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn history() -> History {
        let mut history = History::default();
        history.reset(Route::Home);
        history
    }

    #[test]
    fn routes_round_trip_through_their_persisted_form() {
        for route in [
            Route::Home,
            Route::Search,
            Route::Explore,
            Route::Collection,
            Route::Album(1550545),
            Route::Artist(8847),
            Route::Playlist("a24bc16f-3d3f-491f-955e-616419c5182b".to_owned()),
            Route::Mix("discovery".to_owned()),
        ] {
            let persisted = route.persist();
            assert_eq!(Route::restore(&persisted), Some(route.clone()), "{persisted}");
        }
    }

    #[test]
    fn unrecognised_persisted_routes_restore_to_nothing() {
        assert_eq!(Route::restore(""), None);
        assert_eq!(Route::restore("album"), None);
        assert_eq!(Route::restore("album:not-a-number"), None);
        assert_eq!(Route::restore("playlist:"), None);
        assert_eq!(Route::restore("nonsense:1"), None);
    }

    #[test]
    fn a_fresh_history_cannot_move() {
        let history = history();
        assert!(!history.can_back());
        assert!(!history.can_forward());
        assert_eq!(history.current(), Some(&Route::Home));
    }

    #[test]
    fn back_and_forward_walk_the_entries() {
        let mut history = history();
        assert!(history.push(Route::Search));
        assert!(history.push(Route::Album(7)));
        assert!(history.can_back());
        assert!(!history.can_forward());
        assert_eq!(history.back(), Some(Route::Search));
        assert!(history.can_forward());
        assert!(history.can_back());
        assert_eq!(history.back(), Some(Route::Home));
        assert!(!history.can_back());
        assert_eq!(history.back(), None);
        assert_eq!(history.current(), Some(&Route::Home));
        assert_eq!(history.forward(), Some(Route::Search));
        assert_eq!(history.forward(), Some(Route::Album(7)));
        assert_eq!(history.forward(), None);
        assert_eq!(history.current(), Some(&Route::Album(7)));
        assert!(!history.can_forward());
    }

    #[test]
    fn pushing_after_back_drops_the_forward_entries() {
        let mut history = history();
        history.push(Route::Search);
        history.push(Route::Album(7));
        assert_eq!(history.back(), Some(Route::Search));
        assert!(history.can_forward());
        assert!(history.push(Route::Artist(3)));
        assert!(!history.can_forward());
        assert_eq!(history.back(), Some(Route::Search));
        assert_eq!(history.forward(), Some(Route::Artist(3)));
    }

    #[test]
    fn pushing_the_current_route_changes_nothing() {
        let mut history = history();
        assert!(history.push(Route::Search));
        assert!(!history.push(Route::Search));
        assert!(!history.can_forward());
        assert_eq!(history.back(), Some(Route::Home));
        assert_eq!(history.forward(), Some(Route::Search));
    }

    #[test]
    fn payloads_distinguish_routes() {
        let mut history = history();
        assert!(history.push(Route::Album(1)));
        assert!(history.push(Route::Album(2)));
        assert!(history.push(Route::Playlist("a".to_owned())));
        assert!(history.push(Route::Playlist("b".to_owned())));
        assert_eq!(history.back(), Some(Route::Playlist("a".to_owned())));
        assert_eq!(history.back(), Some(Route::Album(2)));
    }

    #[test]
    fn routes_select_their_pages() {
        assert!(matches!(Route::Album(1).page(), Page::Album));
        assert!(matches!(Route::Artist(1).page(), Page::Artist));
        assert!(matches!(Route::Playlist("a".to_owned()).page(), Page::Playlist));
        assert!(matches!(Route::Mix("a".to_owned()).page(), Page::Mix));
        assert!(matches!(Route::Explore.page(), Page::Explore));
        assert!(matches!(Route::Collection.page(), Page::Collection));
        assert!(matches!(Route::Search.page(), Page::Search));
        assert!(matches!(Route::Home.page(), Page::Home));
        assert!(matches!(Route::ViewAll(9).page(), Page::ViewAll));
    }

    /// A "View all" page holds a snapshot only this run has, so its route persists (for the
    /// settings file's own record) but is never restored: a restart opens Home instead of an
    /// empty page.
    #[test]
    fn a_view_all_route_is_not_restored() {
        let route = Route::ViewAll(9);
        assert_eq!(route.persist(), "viewall:9");
        assert_eq!(Route::restore(&route.persist()), None);
    }
}
