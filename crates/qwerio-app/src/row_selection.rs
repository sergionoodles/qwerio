//! Rows picked for copying and exporting, by plain, toggle (Ctrl or Cmd),
//! and range (Shift) clicks. Rows are numbered from the first row of the
//! query, so the selection stays put when pages are appended or released.

use std::collections::BTreeSet;
use std::ops::Range;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionGesture {
    /// Selects only this row.
    Replace,
    /// Adds or removes this row.
    Toggle,
    /// Selects every row between the last clicked row and this one.
    Extend,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RowSelection {
    rows: BTreeSet<usize>,
    /// Where the next range starts: the last row clicked without Shift.
    anchor: Option<usize>,
}

impl RowSelection {
    pub fn apply(&mut self, row: usize, gesture: SelectionGesture) {
        match gesture {
            SelectionGesture::Replace => {
                self.rows = BTreeSet::from([row]);
                self.anchor = Some(row);
            }
            SelectionGesture::Toggle => {
                if !self.rows.remove(&row) {
                    self.rows.insert(row);
                }
                self.anchor = Some(row);
            }
            SelectionGesture::Extend => {
                let anchor = self.anchor.unwrap_or(row);
                self.rows = (anchor.min(row)..=anchor.max(row)).collect();
                self.anchor = Some(anchor);
            }
        }
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn contains(&self, row: usize) -> bool {
        self.rows.contains(&row)
    }

    /// The selected rows within `available`, in order.
    pub fn rows_within(&self, available: Range<usize>) -> impl Iterator<Item = usize> + '_ {
        self.rows.range(available).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn selected(selection: &RowSelection) -> Vec<usize> {
        selection.rows_within(0..usize::MAX).collect()
    }

    #[test]
    fn plain_clicks_replace_and_toggles_add_or_remove() {
        let mut selection = RowSelection::default();

        selection.apply(4, SelectionGesture::Replace);
        selection.apply(2, SelectionGesture::Toggle);
        selection.apply(7, SelectionGesture::Toggle);
        selection.apply(4, SelectionGesture::Toggle);
        assert_eq!(selected(&selection), vec![2, 7]);

        selection.apply(5, SelectionGesture::Replace);
        assert_eq!(selected(&selection), vec![5]);
    }

    #[test]
    fn ranges_extend_from_the_anchor_in_either_direction() {
        let mut selection = RowSelection::default();

        selection.apply(5, SelectionGesture::Replace);
        selection.apply(8, SelectionGesture::Extend);
        assert_eq!(selected(&selection), vec![5, 6, 7, 8]);

        // Extending again re-ranges from the same anchor.
        selection.apply(3, SelectionGesture::Extend);
        assert_eq!(selected(&selection), vec![3, 4, 5]);
    }

    #[test]
    fn extending_without_an_anchor_selects_the_row() {
        let mut selection = RowSelection::default();

        selection.apply(3, SelectionGesture::Extend);

        assert_eq!(selected(&selection), vec![3]);
        assert_eq!(selection.rows_within(4..10).count(), 0);
    }
}
