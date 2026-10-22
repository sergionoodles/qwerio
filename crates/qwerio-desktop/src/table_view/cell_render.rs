//! Rendering of inline inputs, checkboxes, and the calendar popup.
use super::TableGridDelegate;
use super::cell_editor::{DEFAULT_LABEL, GridContext, commit_before_moving, focus_grid};
use crate::actions::{AddRow, DeleteRow, DuplicateRow, EditCell};
use gpui_kit::base::actions::{SelectDown, SelectNextColumn, SelectPrevColumn, SelectUp};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::calendar::{Calendar, CalendarState};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Escape, Input};
use gpui_kit::component::menu::ContextMenuExt as _;
use gpui_kit::component::table::{TableDelegate as _, TableEvent};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, Sizable as _, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Entity, InteractiveElement as _, IntoElement, MouseButton, ParentElement as _,
    Role, SharedString, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _,
    anchored, deferred, div, px,
};
use qwerio_core::cell_input::boolean_state;
use qwerio_core::display::NULL_LABEL;

impl TableGridDelegate {
    pub(super) fn render_editor(
        &self,
        row_ix: usize,
        col_ix: usize,
        cx: &mut GridContext,
    ) -> AnyElement {
        let Some(edit) = self.editing.as_ref() else {
            return div().into_any_element();
        };

        let border = if edit.error.is_some() {
            cx.theme().danger
        } else {
            cx.theme().ring
        };

        h_flex()
            .id(SharedString::from(format!("cell-editor-{row_ix}-{col_ix}")))
            .relative()
            .size_full()
            .gap_0p5()
            .px_0p5()
            .rounded_sm()
            .border_1()
            .border_color(border)
            .bg(cx.theme().background)
            // Clicks inside the editor must not reselect the cell, which
            // would escalate to row selection or start another edit.
            .on_click(|_, _, cx| cx.stop_propagation())
            // Row shortcuts belong to the grid, not to the text being typed.
            .on_action(|_: &EditCell, _, cx: &mut App| cx.stop_propagation())
            .on_action(|_: &AddRow, _, cx: &mut App| cx.stop_propagation())
            .on_action(|_: &DuplicateRow, _, cx: &mut App| cx.stop_propagation())
            .on_action(|_: &DeleteRow, _, cx: &mut App| cx.stop_propagation())
            .on_action(cx.listener(|table, _: &Escape, window, cx| {
                table.delegate_mut().close_edit(cx);
                focus_grid(table, window, cx);
                cx.stop_propagation();
            }))
            .capture_action(cx.listener(|table, _: &SelectUp, window, cx| {
                commit_before_moving(table, window, cx)
            }))
            .capture_action(cx.listener(|table, _: &SelectDown, window, cx| {
                commit_before_moving(table, window, cx)
            }))
            .capture_action(cx.listener(|table, _: &SelectNextColumn, window, cx| {
                commit_before_moving(table, window, cx)
            }))
            .capture_action(cx.listener(|table, _: &SelectPrevColumn, window, cx| {
                commit_before_moving(table, window, cx)
            }))
            .when_some(edit.error.clone(), |this, error| {
                this.tooltip(move |window, cx| {
                    gpui_kit::component::tooltip::Tooltip::new(error.clone()).build(window, cx)
                })
            })
            .child(
                Input::new(&edit.input)
                    .id("cell-editor")
                    .aria_label("Cell value")
                    .xsmall()
                    .appearance(false)
                    .flex_1(),
            )
            .when(edit.calendar.is_some(), |this| {
                this.child(
                    Button::new("cell-editor-calendar")
                        .ghost()
                        .xsmall()
                        .icon(Icon::new(gpui_kit::assets::IconName::Calendar))
                        .accessibility_label("Pick a date")
                        .tooltip("Pick a date")
                        .toggled(edit.is_calendar_open)
                        .on_click(
                            cx.listener(|table, _, _, cx| table.delegate_mut().toggle_calendar(cx)),
                        ),
                )
            })
            .when(edit.draft.is_nullable, |this| {
                let (row, column) = (edit.row, edit.column);
                this.child(
                    Button::new("cell-editor-null")
                        .ghost()
                        .xsmall()
                        .label(NULL_LABEL)
                        .tooltip("Set to NULL")
                        .on_click(cx.listener(move |table, _, window, cx| {
                            table.delegate_mut().set_null(row, column, cx);
                            focus_grid(table, window, cx);
                        })),
                )
            })
            .when_some(
                edit.calendar.clone().filter(|_| edit.is_calendar_open),
                |this, calendar| this.child(render_calendar_popup(calendar, cx)),
            )
            .test_support()
            .into_any_element()
    }

    /// A boolean cell as a checkbox, with NULL and DEFAULT spelled out.
    pub(super) fn render_checkbox(
        &self,
        row_ix: usize,
        col_ix: usize,
        column: usize,
        cx: &mut GridContext,
    ) -> AnyElement {
        let value = self.cell_value(row_ix, column).cloned();
        let checked = value.as_ref().and_then(boolean_state).unwrap_or(false);
        let note = match &value {
            None => Some(DEFAULT_LABEL),
            Some(value) if value.is_null() => Some(NULL_LABEL),
            Some(_) => None,
        };

        h_flex()
            .size_full()
            .gap_1()
            .child(
                Checkbox::new(SharedString::from(format!("cell-check-{row_ix}-{col_ix}")))
                    .disabled(self.edit_session.is_saving())
                    .checked(checked)
                    .accessibility_label(note.unwrap_or("Value"))
                    .on_click(cx.listener(move |table, checked: &bool, _, cx| {
                        table
                            .delegate_mut()
                            .set_boolean(row_ix, column, *checked, cx)
                    })),
            )
            .when_some(note, |this, note| {
                this.child(
                    div()
                        .text_color(cx.theme().muted_foreground)
                        .italic()
                        .child(note),
                )
            })
            .into_any_element()
    }
}

/// Gives a cell the row menu, with the cell's own items. The table opens
/// that menu only for right clicks outside cells: on a cell it stops the
/// click, so the cell needs a menu of its own.
pub(super) fn with_row_menu(
    content: AnyElement,
    row_ix: usize,
    col_ix: usize,
    column: usize,
    cx: &mut GridContext,
) -> AnyElement {
    let table = cx.entity();

    div()
        .id(SharedString::from(format!("cell-menu-{row_ix}-{col_ix}")))
        .size_full()
        // Runs before the menu, which keeps the table from reporting the
        // right click itself; an edit in another cell still commits.
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |_, _, _, cx| cx.emit(TableEvent::RightClickedCell(row_ix, col_ix))),
        )
        .child(content)
        .context_menu(move |menu, window, cx| {
            table.update(cx, |table, cx| {
                let delegate = table.delegate_mut();
                delegate.right_clicked = Some((row_ix, column));
                delegate.context_menu(row_ix, menu, window, cx)
            })
        })
        .into_any_element()
}

/// A new-row cell left to the column's default.
pub(super) fn render_default_cell(row_ix: usize, col_ix: usize, cx: &App) -> AnyElement {
    div()
        .id(SharedString::from(format!("cell-{row_ix}-{col_ix}")))
        .w_full()
        .truncate()
        .role(Role::Cell)
        .aria_label(DEFAULT_LABEL)
        .text_color(cx.theme().muted_foreground)
        .italic()
        .child(DEFAULT_LABEL)
        .test_support()
        .into_any_element()
}

fn render_calendar_popup(
    calendar: Entity<CalendarState>,
    cx: &mut GridContext,
) -> impl IntoElement {
    div().absolute().top_full().left_0().child(
        deferred(
            anchored().snap_to_window_with_margin(px(8.)).child(
                div()
                    .id("cell-editor-calendar-popup")
                    .occlude()
                    .mt_1()
                    .p_2()
                    .rounded_md()
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().popover)
                    .shadow_md()
                    .role(Role::Dialog)
                    .aria_label("Pick a date")
                    .on_mouse_down_out(cx.listener(|table, _, _, cx| {
                        if let Some(edit) = table.delegate_mut().editing.as_mut() {
                            edit.is_calendar_open = false;
                            cx.notify();
                        }
                    }))
                    .child(Calendar::new(&calendar))
                    .test_support(),
            ),
        )
        .with_priority(gpui_kit::base::POPUP_PRIORITY),
    )
}
