//! The table view's side of inline editing: starting edits from the grid and
//! the keyboard, and saving or discarding staged changes.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::table::TableEvent;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, WindowExt as _, h_flex,
};
use gpui_kit::{
    App, Context, InteractiveElement as _, IntoElement, ParentElement as _, Role, SharedString,
    StatefulInteractiveElement as _, Styled as _, Task, TestSupportExt as _, Window, div,
};

use super::TableView;

impl TableView {
    pub(super) fn is_editable(&self, cx: &App) -> bool {
        self.grid.read(cx).delegate().is_editable()
    }

    /// Opens editors on double click, commits the open edit when another
    /// cell is selected, and remembers right-clicked cells for the menu.
    pub(super) fn track_editing(
        &mut self,
        event: &TableEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match *event {
            TableEvent::SelectCell(row, col_ix) | TableEvent::RightClickedCell(row, col_ix) => {
                let is_right_click = matches!(event, TableEvent::RightClickedCell(..));

                self.grid.update(cx, |grid, cx| {
                    let delegate = grid.delegate_mut();
                    let column = delegate.visible().get(col_ix).copied();

                    if is_right_click {
                        delegate.right_clicked = column.map(|column| (row, column));
                    }

                    let is_other_cell =
                        column.is_none_or(|column| !delegate.is_editing(row, column));

                    if delegate.editing.is_some() && is_other_cell {
                        delegate.commit_edit(cx);
                    }
                });
            }
            TableEvent::SelectRow(_) | TableEvent::ClearSelection => {
                self.grid.update(cx, |grid, cx| {
                    grid.delegate_mut().commit_edit(cx);
                });
            }
            TableEvent::DoubleClickedCell(row, col_ix) if self.is_editable(cx) => {
                let column = self.grid.read(cx).delegate().visible().get(col_ix).copied();

                if let Some(column) = column {
                    self.edit_cell(row, column, window, cx);
                }
            }
            _ => {}
        }
    }

    pub(super) fn edit_cell(
        &mut self,
        row: usize,
        column: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.grid.update(cx, |grid, cx| {
            grid.delegate_mut().begin_edit(row, column, window, cx)
        });
    }

    /// Edits the selected cell (Enter or F2).
    pub(super) fn edit_selected_cell(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let grid = self.grid.read(cx);
        let Some((row, col_ix)) = grid.selected_cell() else {
            return;
        };

        if grid.delegate().editing.is_some() {
            return;
        }

        if let Some(column) = grid.delegate().visible().get(col_ix).copied() {
            self.edit_cell(row, column, window, cx);
        }
    }

    /// Whether leaving this view would lose staged or typed edits.
    pub fn has_unsaved_edits(&self, cx: &App) -> bool {
        self.grid.read(cx).delegate().has_unsaved_changes(cx)
    }

    pub fn is_saving(&self, cx: &App) -> bool {
        self.grid.read(cx).delegate().edit_session.is_saving()
    }

    pub fn discard_edits(&mut self, cx: &mut Context<Self>) -> bool {
        if self.is_saving(cx) {
            return false;
        }
        let had_new_rows = self.grid.update(cx, |grid, cx| {
            let delegate = grid.delegate_mut();
            let had_new_rows = delegate.edit_session.pending().new_row_count() > 0;
            delegate.edit_session.edit_pending(|edits| edits.clear());
            delegate.close_edit(cx);

            // Loaded rows move up into the new rows' place.
            if had_new_rows {
                grid.clear_selection(cx);
            }

            had_new_rows
        });

        if had_new_rows {
            self.selected_row = None;
        }
        true
    }

    /// Saves every staged change in one transaction. The open editor is
    /// committed first; nothing is saved while its text is invalid. The task
    /// resolves to whether the save succeeded.
    pub fn save_edits(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Task<bool> {
        if self.is_saving(cx) {
            return Task::ready(false);
        }

        let staged = self.grid.update(cx, |grid, cx| {
            let delegate = grid.delegate_mut();

            if !delegate.commit_edit(cx) {
                return Err(());
            }
            if delegate.is_reloading
                || matches!(
                    delegate.browser.status(),
                    qwerio_app::table::LoadStatus::Loading
                        | qwerio_app::table::LoadStatus::LoadingMore
                )
            {
                delegate.edit_error =
                    Some("Wait for the rows to finish loading before saving".into());
                cx.notify();
                return Err(());
            }

            let details = delegate.browser.details().cloned();
            Ok(details.and_then(|details| delegate.edit_session.begin_save(details)))
        });

        let batch = match staged {
            Err(()) => return Task::ready(false),
            Ok(None) => return Task::ready(true),
            Ok(Some(batch)) => batch,
        };

        let save = self
            .model
            .read(cx)
            .save_row_edits(batch.request.clone(), cx);
        cx.notify();

        cx.spawn_in(window, async move |this, cx| {
            let result = save.await;

            this.update_in(cx, |this, window, cx| {
                let is_saved = result.is_ok();

                let notification = match result {
                    Ok(rows) => {
                        let count = rows.len();
                        this.grid.update(cx, |grid, cx| {
                            let delegate = grid.delegate_mut();
                            delegate.edit_session.finish_save(
                                &mut delegate.browser,
                                &batch,
                                Some(rows),
                            );
                            cx.notify();
                        });
                        Notification::success(format!("Saved {count} {}", plural(count, "row")))
                    }
                    Err(error) => {
                        this.grid.update(cx, |grid, _| {
                            let delegate = grid.delegate_mut();
                            delegate
                                .edit_session
                                .finish_save(&mut delegate.browser, &batch, None);
                        });
                        Notification::error(format!("Saving failed: {error}"))
                    }
                };

                this.grid
                    .update(cx, |grid, cx| grid.delegate_mut().apply_deferred_pages(cx));
                window.push_notification(notification, cx);
                cx.notify();
                is_saved
            })
            .unwrap_or(false)
        })
    }

    /// Unsaved edit count with Save and Discard, shown while edits are staged.
    pub(super) fn render_edit_controls(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let count = self
            .grid
            .read(cx)
            .delegate()
            .edit_session
            .pending()
            .change_count();

        if count == 0 {
            return None;
        }

        let label: SharedString = format!("{count} unsaved {}", plural(count, "change")).into();

        Some(
            h_flex()
                .gap_1()
                .child(
                    div()
                        .id("unsaved-changes")
                        .px_1()
                        .text_xs()
                        .text_color(cx.theme().warning)
                        .role(Role::Status)
                        .aria_label(label.clone())
                        .child(label)
                        .test_support(),
                )
                .child(
                    Button::new("discard-edits")
                        .ghost()
                        .small()
                        .icon(Icon::new(gpui_kit::assets::IconName::Undo2))
                        .label("Discard")
                        .disabled(self.is_saving(cx))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.discard_edits(cx);
                        })),
                )
                .child(
                    Button::new("save-edits")
                        .primary()
                        .small()
                        .icon(Icon::new(gpui_kit::assets::IconName::Save))
                        .label(if self.is_saving(cx) {
                            "Saving…"
                        } else {
                            "Save"
                        })
                        .tooltip("Save all changes in one transaction")
                        .disabled(self.is_saving(cx))
                        .on_click(
                            cx.listener(|this, _, window, cx| this.save_edits(window, cx).detach()),
                        ),
                ),
        )
    }
}

fn plural(count: usize, noun: &str) -> String {
    if count == 1 {
        noun.to_owned()
    } else {
        format!("{noun}s")
    }
}
