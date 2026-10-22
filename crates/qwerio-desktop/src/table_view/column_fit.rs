//! Fitting a column to its content when its resize handle is double-clicked.
use super::TableGridDelegate;
use super::cell_editor::DEFAULT_LABEL;
use crate::grid::{DrawnCell, fit_width};
use gpui_kit::component::table::TableDelegate as _;
use gpui_kit::{App, Pixels, Window};
use qwerio_core::display::{CellStyle, NULL_LABEL};

/// A boolean cell's checkbox, 1rem, and the 0.25rem gap before its note.
const CHECKBOX_REMS: f32 = 1.;
const CHECKBOX_GAP_REMS: f32 = 0.25;
/// A tinted cell's horizontal padding, 0.25rem on each side.
const TINT_PADDING_REMS: f32 = 0.5;

impl TableGridDelegate {
    /// The width grid column `col_ix` needs for its header and every row,
    /// staged edits and new rows included, as they are drawn.
    pub(super) fn fit_width(&self, col_ix: usize, window: &Window, cx: &App) -> Option<Pixels> {
        let column = *self.visible().get(col_ix)?;
        let header = &self.browser.columns().get(column)?.name;
        let cells = (0..self.rows_count(cx))
            .map(|row| self.drawn_cell(row, column, window, cx))
            .collect::<Vec<_>>();

        Some(fit_width(header, true, cells, window, cx))
    }

    fn drawn_cell(&self, row: usize, column: usize, window: &Window, cx: &App) -> DrawnCell {
        let rem_size = window.rem_size();
        let tint_padding = match self.cell_tint(row, column, cx) {
            Some(_) => rem_size * TINT_PADDING_REMS,
            None => Pixels::ZERO,
        };
        let value = self.cell_value(row, column);

        let cell = if self.is_checkbox_column(column) {
            let note = match value {
                None => DEFAULT_LABEL,
                Some(value) if value.is_null() => NULL_LABEL,
                Some(_) => "",
            };
            let checkbox = if note.is_empty() {
                rem_size * CHECKBOX_REMS
            } else {
                rem_size * (CHECKBOX_REMS + CHECKBOX_GAP_REMS)
            };

            DrawnCell {
                text: note.into(),
                style: CellStyle::Null,
                extra: checkbox,
            }
        } else {
            match value {
                Some(value) => DrawnCell::preview(value),
                None => DrawnCell {
                    text: DEFAULT_LABEL.into(),
                    style: CellStyle::Null,
                    extra: Pixels::ZERO,
                },
            }
        };

        DrawnCell {
            extra: cell.extra + tint_padding,
            ..cell
        }
    }
}
