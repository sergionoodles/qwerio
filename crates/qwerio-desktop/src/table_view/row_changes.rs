//! Adding, duplicating, and deleting rows. New rows are shown above the
//! loaded rows and deleted rows stay visible, struck through, until the
//! changes are saved together or discarded.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, h_flex};
use gpui_kit::{
    App, Context, Focusable as _, IntoElement, ParentElement as _, Styled as _, Window,
};
use qwerio_app::table_edits::GridRow;
use qwerio_core::cell_input::CellEditor;

use super::{TableGridDelegate, TableView};

impl TableGridDelegate {
    pub(super) fn is_new_row(&self, row: usize) -> bool {
        matches!(self.edit_session.pending().grid_row(row), GridRow::New(_))
    }

    pub(super) fn is_deleted_row(&self, row: usize) -> bool {
        self.loaded_row(row)
            .is_some_and(|index| self.edit_session.pending().is_deleted(&self.browser, index))
    }

    /// The first visible column a new row's editor opens on: one typed into,
    /// not a checkbox or a generated column.
    fn first_typed_column(&self, row: usize) -> Option<(usize, usize)> {
        self.visible().into_iter().enumerate().find(|(_, column)| {
            self.cell_editing(row, *column)
                .is_ok_and(|editing| editing.kind != CellEditor::Boolean)
        })
    }
}

impl TableView {
    /// The grid row of the selected row or cell.
    fn selected_grid_row(&self, cx: &App) -> Option<usize> {
        let grid = self.grid.read(cx);

        grid.selected_cell()
            .map(|(row, _)| row)
            .or_else(|| grid.selected_row())
    }

    /// Adds a new row above the others, empty or copied from a grid row, and
    /// selects it. An empty row opens its first typed column for editing.
    pub(super) fn add_row(
        &mut self,
        copy_from: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.is_editable(cx) || self.is_saving(cx) {
            return;
        }

        let selected_col = self.grid.read(cx).selected_cell().map(|(_, col)| col);

        let editor = self.grid.update(cx, |grid, cx| {
            let delegate = grid.delegate_mut();

            if !delegate.commit_edit(cx) {
                return None;
            }

            let values = copy_from
                .map(|row| {
                    delegate.edit_session.pending().copy_values(
                        &delegate.browser,
                        delegate.edit_session.pending().grid_row(row),
                    )
                })
                .unwrap_or_default();
            let row = delegate
                .edit_session
                .edit_pending(|edits| edits.add_row(values))?;
            delegate.edit_error = None;

            let typed_column = delegate.first_typed_column(row);
            let col_ix = selected_col
                .filter(|_| copy_from.is_some())
                .or(typed_column.map(|(col_ix, _)| col_ix))
                .unwrap_or_default();

            grid.set_selected_cell(row, col_ix, cx);
            let handle = grid.focus_handle(cx);
            window.focus(&handle, cx);

            typed_column
                .filter(|_| copy_from.is_none())
                .map(|(_, column)| (row, column))
        });

        if let Some((row, column)) = editor {
            self.edit_cell(row, column, window, cx);
        }

        cx.notify();
    }

    pub(super) fn duplicate_selected_row(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(row) = self.selected_grid_row(cx) {
            self.add_row(Some(row), window, cx);
        }
    }

    /// Removes a new row, or marks a loaded row for deletion and back.
    pub(super) fn toggle_delete_row(&mut self, row: usize, cx: &mut Context<Self>) {
        if !self.is_editable(cx) || self.is_saving(cx) {
            return;
        }

        let is_new_row_removed = self.grid.update(cx, |grid, cx| {
            let delegate = grid.delegate_mut();
            let is_editing_row = delegate
                .editing
                .as_ref()
                .is_some_and(|edit| edit.row == row);

            let is_new_row_removed = match delegate.edit_session.pending().grid_row(row) {
                GridRow::New(index) => {
                    // Rows below move up: an open editor would point elsewhere.
                    delegate.close_edit(cx);
                    delegate
                        .edit_session
                        .edit_pending(|edits| edits.remove_new_row(index));
                    true
                }
                GridRow::Loaded(index) => {
                    if is_editing_row {
                        delegate.close_edit(cx);
                    }

                    if !delegate
                        .edit_session
                        .edit_pending(|edits| edits.toggle_deleted(&delegate.browser, index))
                        .unwrap_or(false)
                    {
                        delegate.edit_error =
                            Some("This row cannot be identified; refresh the table".into());
                    }

                    false
                }
            };

            cx.notify();
            is_new_row_removed
        });

        if is_new_row_removed {
            self.selected_row = None;
            self.grid.update(cx, |grid, cx| grid.clear_selection(cx));
        }

        cx.notify();
    }

    pub(super) fn delete_selected_row(&mut self, cx: &mut Context<Self>) {
        if let Some(row) = self.selected_grid_row(cx) {
            self.toggle_delete_row(row, cx);
        }
    }

    /// Add, Duplicate, and Delete (or Restore) for editable tables, as icons
    /// so the toolbar keeps room for Save.
    pub(super) fn render_row_controls(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        if !self.is_editable(cx) {
            return None;
        }

        let selected = self.selected_grid_row(cx);
        let is_deleted =
            selected.is_some_and(|row| self.grid.read(cx).delegate().is_deleted_row(row));
        let delete_label = if is_deleted {
            "Restore Row"
        } else {
            "Delete Row"
        };

        Some(
            h_flex()
                .gap_1()
                .child(
                    Button::new("add-row")
                        .ghost()
                        .small()
                        .icon(Icon::new(gpui_kit::assets::IconName::Plus))
                        .accessibility_label("Add Row")
                        .tooltip("Add Row")
                        .disabled(self.is_saving(cx))
                        .on_click(
                            cx.listener(|this, _, window, cx| this.add_row(None, window, cx)),
                        ),
                )
                .child(
                    Button::new("duplicate-row")
                        .ghost()
                        .small()
                        .icon(Icon::new(gpui_kit::assets::IconName::CopyPlus))
                        .accessibility_label("Duplicate Row")
                        .tooltip("Duplicate Row: add a copy of the selected row")
                        .disabled(selected.is_none() || self.is_saving(cx))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.duplicate_selected_row(window, cx)
                        })),
                )
                .child(
                    Button::new("delete-row")
                        .ghost()
                        .small()
                        .icon(Icon::new(if is_deleted {
                            gpui_kit::assets::IconName::Undo2
                        } else {
                            gpui_kit::assets::IconName::Trash
                        }))
                        .accessibility_label(delete_label)
                        .tooltip(delete_label)
                        .disabled(selected.is_none() || self.is_saving(cx))
                        .on_click(cx.listener(|this, _, _, cx| this.delete_selected_row(cx))),
                ),
        )
    }
}
