//! One column being defined: its name, type, and constraints.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{IconName, Sizable as _};
use gpui_kit::{App, AppContext as _, Context, ElementId, Entity, SharedString, Window};
use qwerio_core::ddl::ColumnDefinition;

pub struct ColumnRow {
    /// Stable across removals, so element ids keep pointing at the same row.
    pub id: usize,
    pub name: Entity<InputState>,
    pub data_type: Entity<InputState>,
    pub is_nullable: bool,
    pub is_primary_key: bool,
}

impl ColumnRow {
    pub fn new<T: 'static>(
        id: usize,
        name: &str,
        data_type: &str,
        is_primary_key: bool,
        window: &mut Window,
        cx: &mut Context<T>,
    ) -> Self {
        let name = name.to_owned();
        let data_type = data_type.to_owned();

        Self {
            id,
            name: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("Column name")
                    .default_value(name)
            }),
            data_type: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("Type")
                    .default_value(data_type)
            }),
            is_nullable: !is_primary_key,
            is_primary_key,
        }
    }

    pub fn definition(&self, default: Option<String>, cx: &App) -> ColumnDefinition {
        ColumnDefinition {
            name: self.name.read(cx).value().trim().to_owned(),
            data_type: self.data_type.read(cx).value().trim().to_owned(),
            is_nullable: self.is_nullable,
            default,
            is_primary_key: self.is_primary_key,
        }
    }

    /// Key columns never allow NULL.
    pub fn set_primary_key(&mut self, is_primary_key: bool) {
        self.is_primary_key = is_primary_key;

        if is_primary_key {
            self.is_nullable = false;
        }
    }

    pub fn set_nullable(&mut self, is_nullable: bool) {
        self.is_nullable = is_nullable;

        if is_nullable {
            self.is_primary_key = false;
        }
    }
}

/// A type field with a menu of the engine's suggested types. Any other type
/// the engine accepts can be typed.
pub fn type_input(
    id: impl Into<ElementId>,
    state: &Entity<InputState>,
    types: &[SharedString],
) -> Input {
    let id = id.into();
    let menu_state = state.clone();
    let types = types.to_vec();

    let menu = Button::new(ElementId::from((id.clone(), "menu")))
        .ghost()
        .xsmall()
        .icon(IconName::ChevronDown)
        .tooltip("Suggested types")
        .dropdown_menu(move |menu, _, _| {
            types.iter().fold(menu, |menu, data_type| {
                let state = menu_state.clone();
                let data_type = data_type.clone();
                menu.item(
                    PopupMenuItem::new(data_type.clone()).on_click(move |_, window, cx| {
                        state.update(cx, |input, cx| {
                            input.set_value(data_type.clone(), window, cx)
                        });
                    }),
                )
            })
        });

    Input::new(state).id(id).aria_label("Type").suffix(menu)
}
