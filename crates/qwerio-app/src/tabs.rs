//! Tabs, each with its own history and an optional split.

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
    /// The second pane of this tab, if it is split.
    pub split: Option<Split>,
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

/// The second pane of a tab shows a companion tab: it has its own history
/// but never appears in the tab bar on its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Split {
    pub tab: TabId,
    pub axis: SplitAxis,
}

/// Tabs of the main area. Always holds at least one top-level tab.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TabSet {
    tabs: Vec<Tab>,
    active: TabId,
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
                split: None,
            }],
            active: first,
            focused: Pane::Primary,
            next_id: 1,
        }
    }

    /// Every tab, companions included.
    pub fn tabs(&self) -> &[Tab] {
        &self.tabs
    }

    /// The tabs shown in the tab bar: everything but split companions.
    pub fn top_level_tabs(&self) -> impl Iterator<Item = &Tab> {
        self.tabs
            .iter()
            .filter(|tab| self.owner_of(tab.id).is_none())
    }

    pub fn active(&self) -> TabId {
        self.active
    }

    /// The split of the active tab.
    pub fn split(&self) -> Option<Split> {
        self.tab(self.active).and_then(|tab| tab.split)
    }

    pub fn focused_pane(&self) -> Pane {
        self.focused
    }

    /// The tab that receives navigation: the focused pane's tab.
    pub fn focused_tab(&self) -> TabId {
        match (self.focused, self.split()) {
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

    /// The tab whose second pane shows `id`.
    fn owner_of(&self, id: TabId) -> Option<TabId> {
        self.tabs
            .iter()
            .find(|tab| tab.split.is_some_and(|split| split.tab == id))
            .map(|tab| tab.id)
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

    fn insert_tab(&mut self, after: TabId, location: Location) -> TabId {
        let id = TabId(self.next_id);
        self.next_id += 1;

        let insert_at = self
            .tabs
            .iter()
            .position(|tab| tab.id == after)
            .map_or(self.tabs.len(), |index| index + 1);

        self.tabs.insert(
            insert_at,
            Tab {
                id,
                history: NavigationHistory::new(location),
                split: None,
            },
        );
        id
    }

    /// Opens a new tab after the active one and activates it.
    pub fn open_tab(&mut self, location: Location) -> TabId {
        let id = self.insert_tab(self.active, location);
        self.activate(id);
        id
    }

    /// Shows a tab. Activating a companion shows its owner with the second
    /// pane focused.
    pub fn activate(&mut self, id: TabId) {
        if let Some(owner) = self.owner_of(id) {
            self.active = owner;
            self.focused = Pane::Secondary;
            return;
        }

        if self.tab(id).is_some() {
            self.active = id;
            self.focused = Pane::Primary;
        }
    }

    pub fn focus(&mut self, pane: Pane) {
        if pane == Pane::Primary || self.split().is_some() {
            self.focused = pane;
        }
    }

    /// Closes a tab. Closing a companion removes its owner's split; closing
    /// an owner closes its companion too. The last top-level tab cannot be
    /// closed; it is reset instead.
    pub fn close(&mut self, id: TabId, fallback: Location) {
        if let Some(owner) = self.owner_of(id) {
            self.remove_split(owner);
            return;
        }

        if self.tab(id).is_none() {
            return;
        }

        self.remove_split(id);

        if self.top_level_tabs().count() == 1 {
            if let Some(tab) = self.tab_mut(id) {
                tab.history = NavigationHistory::new(fallback);
            }
            self.focused = Pane::Primary;
            return;
        }

        let index = self
            .tabs
            .iter()
            .position(|tab| tab.id == id)
            .unwrap_or_default();
        self.tabs.remove(index);

        if self.active == id {
            let neighbor = self.tabs[index.min(self.tabs.len() - 1)].id;
            self.activate(neighbor);
            self.focused = Pane::Primary;
        }
    }

    fn remove_split(&mut self, owner: TabId) {
        let Some(split) = self.tab_mut(owner).and_then(|tab| tab.split.take()) else {
            return;
        };

        self.tabs.retain(|tab| tab.id != split.tab);

        if self.active == owner {
            self.focused = Pane::Primary;
        }
    }

    /// Splits the active tab, showing `location` in its second pane, and
    /// focuses that pane. Does nothing when the active tab is already split.
    pub fn split_active(&mut self, location: Location, axis: SplitAxis) -> Option<TabId> {
        if self.split().is_some() {
            return None;
        }

        let owner = self.active;
        let companion = self.insert_tab(owner, location);

        if let Some(tab) = self.tab_mut(owner) {
            tab.split = Some(Split {
                tab: companion,
                axis,
            });
        }

        self.focused = Pane::Secondary;
        Some(companion)
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

    fn top_level_ids(tabs: &TabSet) -> Vec<TabId> {
        tabs.top_level_tabs().map(|tab| tab.id).collect()
    }

    #[test]
    fn a_split_lives_inside_its_tab_and_routes_navigation_to_the_focused_pane() {
        let mut tabs = TabSet::new(Location::Root);
        let companion = tabs
            .split_active(folder(), SplitAxis::Horizontal)
            .expect("split");

        assert_eq!(top_level_ids(&tabs), vec![TabId(0)]);
        assert_eq!(tabs.focused_tab(), companion);
        let target = folder();
        tabs.focused_history_mut().visit(target.clone());
        assert_eq!(
            tabs.tab(companion).map(|tab| tab.history.location()),
            Some(&target)
        );

        tabs.focus(Pane::Primary);
        assert_eq!(tabs.focused_tab(), TabId(0));
    }

    #[test]
    fn switching_tabs_hides_the_split_until_its_tab_returns() {
        let mut tabs = TabSet::new(Location::Root);
        let companion = tabs
            .split_active(folder(), SplitAxis::Horizontal)
            .expect("split");
        let other = tabs.open_tab(folder());

        assert_eq!(tabs.split(), None);
        assert_eq!(tabs.focused_tab(), other);

        tabs.activate(TabId(0));
        assert_eq!(tabs.split().map(|split| split.tab), Some(companion));
        assert_eq!(tabs.focused_pane(), Pane::Primary);
    }

    #[test]
    fn closing_the_companion_removes_the_split() {
        let mut tabs = TabSet::new(Location::Root);
        let companion = tabs
            .split_active(folder(), SplitAxis::Vertical)
            .expect("split");

        tabs.close(companion, Location::Root);

        assert_eq!(tabs.split(), None);
        assert_eq!(tabs.focused_pane(), Pane::Primary);
        assert!(tabs.tab(companion).is_none());
    }

    #[test]
    fn closing_a_split_tab_closes_both_panes() {
        let mut tabs = TabSet::new(Location::Root);
        let other = tabs.open_tab(folder());
        let companion = tabs
            .split_active(folder(), SplitAxis::Vertical)
            .expect("split");

        tabs.close(other, Location::Root);

        assert_eq!(top_level_ids(&tabs), vec![TabId(0)]);
        assert!(tabs.tab(companion).is_none());
        assert_eq!(tabs.active(), TabId(0));
    }

    #[test]
    fn tab_sets_survive_serialization() {
        let mut tabs = TabSet::new(Location::Root);
        tabs.open_tab(folder());
        let json = serde_json::to_string(&tabs).expect("serialize");

        assert_eq!(serde_json::from_str::<TabSet>(&json).ok(), Some(tabs));
    }
}
