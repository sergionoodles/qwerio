//! Tabs, each with its own history, and one optional split.

use qwerio_workspace::explorer::Location;
use serde::{Deserialize, Serialize};

use crate::navigation::NavigationHistory;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TabId(u64);

impl TabId {
    pub fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Tab {
    pub id: TabId,
    pub history: NavigationHistory,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SplitAxis {
    Horizontal,
    Vertical,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Pane {
    Primary,
    Secondary,
}

/// The second pane shows one other tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Split {
    pub tab: TabId,
    pub axis: SplitAxis,
}

/// Tabs of the main area. Always holds at least one tab.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TabSet {
    tabs: Vec<Tab>,
    active: TabId,
    split: Option<Split>,
    focused: Pane,
    next_id: u64,
}

impl TabSet {
    pub fn new(location: Location) -> Self {
        let first = TabId(0);

        Self {
            tabs: vec![Tab {
                id: first,
                history: NavigationHistory::new(location),
            }],
            active: first,
            split: None,
            focused: Pane::Primary,
            next_id: 1,
        }
    }

    pub fn tabs(&self) -> &[Tab] {
        &self.tabs
    }

    pub fn active(&self) -> TabId {
        self.active
    }

    pub fn split(&self) -> Option<Split> {
        self.split
    }

    pub fn focused_pane(&self) -> Pane {
        self.focused
    }

    /// The tab that receives navigation: the focused pane's tab.
    pub fn focused_tab(&self) -> TabId {
        match (self.focused, self.split) {
            (Pane::Secondary, Some(split)) => split.tab,
            _ => self.active,
        }
    }

    pub fn tab(&self, id: TabId) -> Option<&Tab> {
        self.tabs.iter().find(|tab| tab.id == id)
    }

    pub fn tab_mut(&mut self, id: TabId) -> Option<&mut Tab> {
        self.tabs.iter_mut().find(|tab| tab.id == id)
    }

    pub fn focused_history_mut(&mut self) -> &mut NavigationHistory {
        let focused = self.focused_tab();
        let index = self
            .tabs
            .iter()
            .position(|tab| tab.id == focused)
            .unwrap_or_default();

        &mut self.tabs[index].history
    }

    /// Opens a new tab after the focused one and focuses it.
    pub fn open_tab(&mut self, location: Location) -> TabId {
        let id = TabId(self.next_id);
        self.next_id += 1;

        let focused = self.focused_tab();
        let insert_at = self
            .tabs
            .iter()
            .position(|tab| tab.id == focused)
            .map_or(self.tabs.len(), |index| index + 1);

        self.tabs.insert(
            insert_at,
            Tab {
                id,
                history: NavigationHistory::new(location),
            },
        );
        self.show_in_focused_pane(id);
        id
    }

    fn show_in_focused_pane(&mut self, id: TabId) {
        match (self.focused, &mut self.split) {
            (Pane::Secondary, Some(split)) => split.tab = id,
            _ => self.active = id,
        }

        self.drop_redundant_split();
    }

    pub fn activate(&mut self, id: TabId) {
        if self.tab(id).is_some() {
            self.show_in_focused_pane(id);
        }
    }

    pub fn focus(&mut self, pane: Pane) {
        if pane == Pane::Primary || self.split.is_some() {
            self.focused = pane;
        }
    }

    /// Closes a tab. The last tab cannot be closed; it is reset instead.
    pub fn close(&mut self, id: TabId, fallback: Location) {
        let Some(index) = self.tabs.iter().position(|tab| tab.id == id) else {
            return;
        };

        if self.tabs.len() == 1 {
            self.tabs[0].history = NavigationHistory::new(fallback);
            self.split = None;
            self.focused = Pane::Primary;
            return;
        }

        self.tabs.remove(index);
        let neighbor = self.tabs[index.min(self.tabs.len() - 1)].id;

        if self.split.is_some_and(|split| split.tab == id) {
            self.split = None;
            self.focused = Pane::Primary;
        }

        if self.active == id {
            self.active = neighbor;
        }

        self.drop_redundant_split();
    }

    /// Shows `id` in the second pane, or toggles the split off when `id` is
    /// already there.
    pub fn split_with(&mut self, id: TabId, axis: SplitAxis) {
        if self.tab(id).is_none() {
            return;
        }

        if self.split.is_some_and(|split| split.tab == id) {
            self.unsplit();
            return;
        }

        self.split = Some(Split { tab: id, axis });
        self.focused = Pane::Secondary;
        self.drop_redundant_split();
    }

    pub fn unsplit(&mut self) {
        self.split = None;
        self.focused = Pane::Primary;
    }

    /// Both panes cannot show the same tab.
    fn drop_redundant_split(&mut self) {
        if self.split.is_some_and(|split| split.tab == self.active) {
            self.unsplit();
        }
    }

    /// Removes history entries and tabs for locations that no longer exist.
    pub fn retain_locations(&mut self, keep: impl Fn(&Location) -> bool, fallback: Location) {
        for tab in &mut self.tabs {
            tab.history.retain(&keep);

            if !keep(tab.history.location()) {
                tab.history.visit(fallback.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use qwerio_workspace::manifest::FolderId;

    use super::*;

    fn folder() -> Location {
        Location::Folder(FolderId::new())
    }

    #[test]
    fn opening_tabs_inserts_after_the_focused_tab() {
        let mut tabs = TabSet::new(Location::Root);
        let second = tabs.open_tab(folder());
        tabs.activate(TabId(0));
        let third = tabs.open_tab(folder());

        let order: Vec<TabId> = tabs.tabs().iter().map(|tab| tab.id).collect();
        assert_eq!(order, vec![TabId(0), third, second]);
        assert_eq!(tabs.active(), third);
    }

    #[test]
    fn closing_the_active_tab_selects_a_neighbor_and_keeps_one_tab() {
        let mut tabs = TabSet::new(Location::Root);
        let second = tabs.open_tab(folder());
        tabs.close(second, Location::Root);
        assert_eq!(tabs.active(), TabId(0));

        let lone = tabs.active();
        tabs.close(lone, Location::Root);
        assert_eq!(tabs.tabs().len(), 1);
    }

    #[test]
    fn split_shows_another_tab_and_routes_navigation_to_the_focused_pane() {
        let mut tabs = TabSet::new(Location::Root);
        let other = tabs.open_tab(folder());
        tabs.activate(TabId(0));
        tabs.split_with(other, SplitAxis::Horizontal);

        assert_eq!(tabs.focused_tab(), other);
        let target = folder();
        tabs.focused_history_mut().visit(target.clone());
        assert_eq!(
            tabs.tab(other).map(|tab| tab.history.location()),
            Some(&target)
        );

        tabs.focus(Pane::Primary);
        assert_eq!(tabs.focused_tab(), TabId(0));
    }

    #[test]
    fn closing_the_split_tab_removes_the_split() {
        let mut tabs = TabSet::new(Location::Root);
        let other = tabs.open_tab(folder());
        tabs.activate(TabId(0));
        tabs.split_with(other, SplitAxis::Vertical);

        tabs.close(other, Location::Root);

        assert_eq!(tabs.split(), None);
        assert_eq!(tabs.focused_pane(), Pane::Primary);
    }

    #[test]
    fn a_tab_is_never_shown_in_both_panes() {
        let mut tabs = TabSet::new(Location::Root);
        let other = tabs.open_tab(folder());
        tabs.activate(TabId(0));
        tabs.split_with(other, SplitAxis::Vertical);
        tabs.focus(Pane::Primary);

        tabs.activate(other);

        assert_eq!(tabs.split(), None);
        assert_eq!(tabs.active(), other);
    }

    #[test]
    fn tab_sets_survive_serialization() {
        let mut tabs = TabSet::new(Location::Root);
        tabs.open_tab(folder());
        let json = serde_json::to_string(&tabs).expect("serialize");

        assert_eq!(serde_json::from_str::<TabSet>(&json).ok(), Some(tabs));
    }
}
