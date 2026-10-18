//! Per-tab back/forward history. History records traversal (including
//! relationship jumps); breadcrumbs, by contrast, show structure.

use qwerio_core::filter::{Filter, SortKey};
use qwerio_core::value::Value;
use qwerio_workspace::explorer::Location;
use serde::{Deserialize, Serialize};

/// Entries kept in each direction.
const MAX_HISTORY: usize = 100;

/// View state restored when returning to a location. Loaded rows are never
/// stored; they are fetched again so restored views are always current.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ViewState {
    pub filters: Vec<Filter>,
    pub sort: Vec<SortKey>,
    pub hidden_columns: Vec<String>,
    /// Ordering-key values of the selected row, matched after reloading.
    pub selected_row_key: Option<Vec<Value>>,
    /// Index of the first visible row when the view was left.
    pub scroll_row: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NavigationEntry {
    pub location: Location,
    pub view: ViewState,
}

impl NavigationEntry {
    pub fn new(location: Location) -> Self {
        Self {
            location,
            view: ViewState::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NavigationHistory {
    back: Vec<NavigationEntry>,
    current: NavigationEntry,
    forward: Vec<NavigationEntry>,
}

impl NavigationHistory {
    pub fn new(location: Location) -> Self {
        Self {
            back: Vec::new(),
            current: NavigationEntry::new(location),
            forward: Vec::new(),
        }
    }

    pub fn current(&self) -> &NavigationEntry {
        &self.current
    }

    pub fn location(&self) -> &Location {
        &self.current.location
    }

    /// Navigates to a location with a fresh view. Re-visiting the current
    /// location does nothing.
    pub fn visit(&mut self, location: Location) {
        self.visit_with_view(location, ViewState::default());
    }

    /// Navigates with a prepared view, such as relationship filters.
    pub fn visit_with_view(&mut self, location: Location, view: ViewState) {
        if self.current.location == location && self.current.view == view {
            return;
        }

        let previous = std::mem::replace(&mut self.current, NavigationEntry { location, view });
        push_bounded(&mut self.back, previous);
        self.forward.clear();
    }

    pub fn can_go_back(&self) -> bool {
        !self.back.is_empty()
    }

    pub fn can_go_forward(&self) -> bool {
        !self.forward.is_empty()
    }

    pub fn go_back(&mut self) -> Option<&NavigationEntry> {
        let previous = self.back.pop()?;
        let current = std::mem::replace(&mut self.current, previous);
        push_bounded(&mut self.forward, current);
        Some(&self.current)
    }

    pub fn go_forward(&mut self) -> Option<&NavigationEntry> {
        let next = self.forward.pop()?;
        let current = std::mem::replace(&mut self.current, next);
        push_bounded(&mut self.back, current);
        Some(&self.current)
    }

    /// Records the current view's filters, sort, selection, and scroll.
    pub fn update_view(&mut self, view: ViewState) {
        self.current.view = view;
    }

    /// Drops entries whose location no longer exists (deleted documents or
    /// folders), keeping the current entry.
    pub fn retain(&mut self, mut keep: impl FnMut(&Location) -> bool) {
        self.back.retain(|entry| keep(&entry.location));
        self.forward.retain(|entry| keep(&entry.location));
    }
}

fn push_bounded(entries: &mut Vec<NavigationEntry>, entry: NavigationEntry) {
    entries.push(entry);

    if entries.len() > MAX_HISTORY {
        entries.remove(0);
    }
}

#[cfg(test)]
mod tests {
    use qwerio_core::filter::Filter;
    use qwerio_workspace::manifest::FolderId;

    use super::*;

    #[test]
    fn back_and_forward_restore_views() {
        let folder = Location::Folder(FolderId::new());
        let mut history = NavigationHistory::new(Location::Root);
        history.visit(folder.clone());
        history.update_view(ViewState {
            scroll_row: 40,
            ..ViewState::default()
        });
        history.visit(Location::Root);

        let back = history.go_back().cloned();
        assert_eq!(back.as_ref().map(|entry| &entry.location), Some(&folder));
        assert_eq!(back.map(|entry| entry.view.scroll_row), Some(40));
        assert!(history.can_go_forward());

        history.go_forward();
        assert_eq!(history.location(), &Location::Root);
    }

    #[test]
    fn visiting_clears_forward_history_and_ignores_repeats() {
        let first = Location::Folder(FolderId::new());
        let second = Location::Folder(FolderId::new());
        let mut history = NavigationHistory::new(Location::Root);

        history.visit(first.clone());
        history.visit(first.clone());
        history.go_back();
        history.visit(second.clone());

        assert!(!history.can_go_forward());
        assert_eq!(
            history.go_back().map(|entry| entry.location.clone()),
            Some(Location::Root)
        );
    }

    #[test]
    fn relationship_jumps_to_the_same_table_with_new_filters_are_recorded() {
        let mut history = NavigationHistory::new(Location::Root);
        let view = ViewState {
            filters: vec![Filter::is_null("parent_id")],
            ..ViewState::default()
        };

        history.visit_with_view(Location::Root, view.clone());

        assert!(history.can_go_back());
        assert_eq!(history.current().view, view);
    }

    #[test]
    fn history_is_bounded() {
        let mut history = NavigationHistory::new(Location::Root);

        for _ in 0..(MAX_HISTORY + 20) {
            history.visit(Location::Folder(FolderId::new()));
        }

        let mut steps = 0;
        while history.go_back().is_some() {
            steps += 1;
        }

        assert_eq!(steps, MAX_HISTORY);
    }

    #[test]
    fn removed_locations_leave_history() {
        let removed = Location::Folder(FolderId::new());
        let mut history = NavigationHistory::new(Location::Root);
        history.visit(removed.clone());
        history.visit(Location::Folder(FolderId::new()));

        history.retain(|location| *location != removed);

        assert_eq!(
            history.go_back().map(|entry| entry.location.clone()),
            Some(Location::Root)
        );
    }
}
