//! Rendering of the schema form: its fields and the statement preview.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, IconName, Sizable as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, Role, SharedString, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _,
    Window, div, px,
};

use super::column_row::{ColumnRow, type_input};
use super::{Fields, SchemaForm};
use crate::connection_dialog::field;

const TYPE_FIELD_WIDTH: f32 = 230.;

impl SchemaForm {
    fn render_table_columns(&self, columns: &[ColumnRow], cx: &mut Context<Self>) -> AnyElement {
        let can_remove = columns.len() > 1;
        let heading = |text: &'static str| {
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(text)
        };

        let rows = columns.iter().map(|row| {
            let id = row.id;

            h_flex()
                .gap_2()
                .child(
                    div().flex_1().child(
                        Input::new(&row.name)
                            .id(("column-name", id))
                            .aria_label("Column name"),
                    ),
                )
                .child(div().w(px(TYPE_FIELD_WIDTH)).child(type_input(
                    ("column-type", id),
                    &row.data_type,
                    &self.column_types,
                )))
                .child(
                    Checkbox::new(("column-nullable", id))
                        .label("Null")
                        .checked(row.is_nullable)
                        .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                            this.update_row(id, cx, |row| row.set_nullable(*checked))
                        })),
                )
                .child(
                    Checkbox::new(("column-key", id))
                        .label("Key")
                        .checked(row.is_primary_key)
                        .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                            this.update_row(id, cx, |row| row.set_primary_key(*checked))
                        })),
                )
                .child(
                    Button::new(("remove-column", id))
                        .ghost()
                        .xsmall()
                        .icon(IconName::Close)
                        .tooltip("Remove column")
                        .disabled(!can_remove)
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.remove_column_row(id, cx)),
                        ),
                )
        });

        v_flex()
            .gap_2()
            .child(
                h_flex()
                    .gap_2()
                    .child(div().flex_1().child(heading("Columns")))
                    .child(div().w(px(TYPE_FIELD_WIDTH)).child(heading("Type"))),
            )
            .children(rows)
            .child(
                div().child(
                    Button::new("add-column-row")
                        .ghost()
                        .small()
                        .icon(IconName::Plus)
                        .label("Add Column")
                        .on_click(
                            cx.listener(|this, _, window, cx| this.add_column_row(window, cx)),
                        ),
                ),
            )
            .into_any_element()
    }

    fn render_column_fields(
        &self,
        column: &ColumnRow,
        default: &Entity<InputState>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = column.id;

        v_flex()
            .gap_3()
            .child(field(
                "Name",
                Input::new(&column.name)
                    .id("column-name")
                    .aria_label("Name"),
                cx,
            ))
            .child(field(
                "Type",
                type_input("column-type", &column.data_type, &self.column_types),
                cx,
            ))
            .child(field(
                "Default",
                Input::new(default)
                    .id("column-default")
                    .aria_label("Default"),
                cx,
            ))
            .child(
                Checkbox::new("column-nullable")
                    .label("Allow NULL")
                    .checked(column.is_nullable)
                    .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                        this.update_row(id, cx, |row| row.set_nullable(*checked))
                    })),
            )
            .into_any_element()
    }

    /// The statement that runs on Create, or what is still missing.
    fn render_preview(&self, cx: &App) -> impl IntoElement {
        let (text, is_valid) = match self.change(cx).render() {
            Ok(sql) => (SharedString::from(sql), true),
            Err(error) => (SharedString::from(error.message), false),
        };

        v_flex()
            .gap_1()
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("SQL"),
            )
            .child(
                div()
                    .id("schema-preview")
                    .p_2()
                    .rounded_md()
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().muted)
                    .text_sm()
                    .when(is_valid, |this| {
                        this.font_family(cx.theme().mono_font_family.clone())
                    })
                    .when(!is_valid, |this| {
                        this.text_color(cx.theme().muted_foreground)
                    })
                    .aria_label(text.clone())
                    .child(text)
                    .test_support(),
            )
    }
}

impl Render for SchemaForm {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match &self.fields {
            Fields::Database { name } => field(
                "Name",
                Input::new(name).id("schema-name").aria_label("Name"),
                cx,
            )
            .into_any_element(),
            Fields::Table { name, columns, .. } => v_flex()
                .gap_3()
                .child(field(
                    "Name",
                    Input::new(name).id("schema-name").aria_label("Name"),
                    cx,
                ))
                .child(self.render_table_columns(columns, cx))
                .into_any_element(),
            Fields::Column {
                column, default, ..
            } => self.render_column_fields(column, default, cx),
        };

        v_flex()
            .gap_3()
            .child(body)
            .child(self.render_preview(cx))
            .when_some(self.error.clone(), |this, error| {
                this.child(
                    div()
                        .id("schema-error")
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .role(Role::Status)
                        .aria_label(error.clone())
                        .child(error)
                        .test_support(),
                )
            })
    }
}
