//! Leaving a table with unsaved row changes: closing its tab, switching to
//! another tab, or navigating away first asks to save or discard them.

use std::cell::RefCell;
use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::DialogClose;
use gpui_kit::component::{WindowExt as _, h_flex};
use gpui_kit::{App, Context, Entity, ParentElement as _, Styled as _, Window, px};
use qwerio_app::tabs::TabId;

use super::{TabContent, WorkspaceShell};
use crate::table_view::TableView;

const DIALOG_WIDTH: f32 = 440.;

/// The navigation that waits for the edits to be saved or discarded.
type Proceed = Box<dyn FnOnce(&mut WorkspaceShell, &mut Window, &mut Context<WorkspaceShell>)>;

impl WorkspaceShell {
    fn unsaved_table(&self, id: TabId, cx: &App) -> Option<Entity<TableView>> {
        match self.slots.get(&id).map(|slot| &slot.content) {
            Some(TabContent::Table(table)) if table.read(cx).has_unsaved_edits(cx) => {
                Some(table.clone())
            }
            _ => None,
        }
    }

    /// Runs `proceed` right away, or once the user saved or discarded the
    /// unsaved changes of tab `id`. Cancelling keeps the tab as it is.
    pub(super) fn leave_tab(
        &mut self,
        id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
        proceed: impl FnOnce(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let Some(table) = self.unsaved_table(id, cx) else {
            return proceed(self, window, cx);
        };

        if table.read(cx).is_saving(cx) {
            window.push_notification(
                gpui_kit::component::notification::Notification::info(
                    "Wait for the current save to finish before leaving this table.",
                ),
                cx,
            );
            return;
        }

        let title = format!("Save changes to “{}”?", table.read(cx).title());
        let proceed: Rc<RefCell<Option<Proceed>>> = Rc::new(RefCell::new(Some(Box::new(proceed))));
        let this = cx.entity().downgrade();

        window.open_dialog(cx, move |dialog, _, _| {
            let discard = {
                let (this, table, proceed) = (this.clone(), table.clone(), Rc::clone(&proceed));
                move |_: &_, window: &mut Window, cx: &mut App| {
                    if !table.update(cx, |table, cx| table.discard_edits(cx)) {
                        return;
                    }
                    window.close_dialog(cx);

                    if let Some(proceed) = proceed.borrow_mut().take() {
                        this.update(cx, |this, cx| proceed(this, window, cx)).ok();
                    }
                }
            };
            let save = {
                let (this, table, proceed) = (this.clone(), table.clone(), Rc::clone(&proceed));
                move |_: &_, window: &mut Window, cx: &mut App| {
                    window.close_dialog(cx);
                    let saved = table.update(cx, |table, cx| table.save_edits(window, cx));
                    let proceed = proceed.borrow_mut().take();

                    this.update(cx, |_, cx| {
                        cx.spawn_in(window, async move |this, cx| {
                            // A failed save keeps the edits; its error is shown.
                            let (true, Some(proceed)) = (saved.await, proceed) else {
                                return;
                            };

                            this.update_in(cx, |this, window, cx| proceed(this, window, cx))
                                .ok();
                        })
                        .detach();
                    })
                    .ok();
                }
            };

            dialog
                .title(title.clone())
                .w(px(DIALOG_WIDTH))
                .child("These row changes have not been saved to the database yet.")
                .footer(
                    h_flex()
                        .gap_2()
                        .justify_end()
                        .child(
                            DialogClose::new()
                                .child(Button::new("cancel-leave").outline().label("Cancel")),
                        )
                        .child(
                            Button::new("discard-leave")
                                .outline()
                                .label("Discard Changes")
                                .on_click(discard),
                        )
                        .child(
                            Button::new("save-leave")
                                .primary()
                                .label("Save Changes")
                                .on_click(save),
                        ),
                )
        });
    }
}

impl WorkspaceShell {
    /// Visits every tab, including background panes, before replacing a workspace.
    pub(crate) fn leave_workspace(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        proceed: impl FnOnce(&mut Window, &mut gpui_kit::App) + 'static,
    ) {
        if self
            .documents
            .values()
            .chain(self.scratches.values())
            .any(|document| document.read(cx).needs_confirmation_to_close(cx))
        {
            window.push_notification(gpui_kit::component::notification::Notification::warning("Commit or roll back the open transaction, or wait for the statement, before leaving the workspace."), cx);
            return;
        }
        let pending = self
            .slots
            .keys()
            .copied()
            .find(|id| self.unsaved_table(*id, cx).is_some());
        let Some(id) = pending else {
            let layout = self.persist_ui_state(cx);
            let drafts: Vec<_> = self
                .documents
                .values()
                .map(|document| document.update(cx, |document, cx| document.persist_draft(cx)))
                .collect();
            cx.spawn_in(window, async move |_, cx| {
                for draft in drafts {
                    if let Err(error) = draft.await {
                        cx.update(|window, cx| {
                            window.push_notification(
                                gpui_kit::component::notification::Notification::error(format!(
                                    "Could not preserve document edits: {error}"
                                )),
                                cx,
                            )
                        })
                        .ok();
                        return;
                    }
                }
                if let Err(error) = layout.await {
                    cx.update(|window, cx| {
                        window.push_notification(
                            gpui_kit::component::notification::Notification::error(format!(
                                "Could not preserve workspace state: {error}"
                            )),
                            cx,
                        )
                    })
                    .ok();
                    return;
                }
                cx.update(|window, cx| proceed(window, cx)).ok();
            })
            .detach();
            return;
        };
        self.leave_tab(id, window, cx, move |this, window, cx| {
            this.leave_workspace(window, cx, proceed);
        });
    }
}
