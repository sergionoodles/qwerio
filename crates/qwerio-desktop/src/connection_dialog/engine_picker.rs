//! Step one of a new connection: a grid of database engines.

use gpui_kit::component::{ActiveTheme as _, Icon, StyledExt as _, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ClickEvent, Context, ElementId, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, Pixels, Role, StatefulInteractiveElement as _, Styled as _,
    TestSupportExt as _, div, px,
};
use qwerio_core::connection::EngineKind;

use super::{ConnectionForm, ENGINES, render_step_heading};
use crate::assets::{POSTGRES_LOGO_PATH, SQLITE_LOGO_PATH};

/// Room for more engines without the cards growing too wide.
const GRID_COLUMNS: u16 = 3;
const CARD_HEIGHT: f32 = 136.;
const CARD_LOGO_TILE: f32 = 52.;
const CARD_LOGO: f32 = 30.;
const LOGO_TINT_OPACITY: f32 = 0.12;
const SELECTED_FILL_OPACITY: f32 = 0.06;
const DOUBLE_CLICK: usize = 2;

pub(super) fn engine_summary(engine: EngineKind) -> &'static str {
    match engine {
        EngineKind::Postgres | EngineKind::Mysql | EngineKind::Mariadb => "Server connection",
        EngineKind::Sqlite => "Local database file",
    }
}

fn engine_id(engine: EngineKind) -> ElementId {
    match engine {
        EngineKind::Postgres => "engine-postgres".into(),
        EngineKind::Sqlite => "engine-sqlite".into(),
        EngineKind::Mysql => "engine-mysql".into(),
        EngineKind::Mariadb => "engine-mariadb".into(),
    }
}

fn engine_accent(engine: EngineKind, cx: &App) -> Hsla {
    match engine {
        EngineKind::Postgres => cx.theme().blue,
        EngineKind::Sqlite => cx.theme().cyan,
        EngineKind::Mysql => cx.theme().blue,
        EngineKind::Mariadb => cx.theme().cyan,
    }
}

/// The engine's mark on a rounded tile tinted with its accent color.
pub(super) fn engine_logo(
    engine: EngineKind,
    tile: Pixels,
    logo: Pixels,
    cx: &App,
) -> impl IntoElement {
    let accent = engine_accent(engine, cx);
    let icon = match engine {
        EngineKind::Postgres => Icon::empty().path(POSTGRES_LOGO_PATH),
        EngineKind::Sqlite => Icon::empty().path(SQLITE_LOGO_PATH),
        EngineKind::Mysql | EngineKind::Mariadb => Icon::new(gpui_kit::assets::IconName::Database),
    };

    div()
        .flex_none()
        .size(tile)
        .rounded_lg()
        .bg(accent.opacity(LOGO_TINT_OPACITY))
        .flex()
        .items_center()
        .justify_center()
        .child(icon.size(logo).text_color(accent))
}

impl ConnectionForm {
    pub(super) fn render_engine_picker(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut cards = Vec::with_capacity(ENGINES.len());
        for engine in ENGINES {
            cards.push(self.render_engine_card(engine, cx));
        }

        v_flex()
            .gap_4()
            .child(render_step_heading(
                None,
                "Select the kind of database you want to connect to.",
                cx,
            ))
            .child(div().grid().grid_cols(GRID_COLUMNS).gap_3().children(cards))
    }

    fn render_engine_card(&self, engine: EngineKind, cx: &mut Context<Self>) -> AnyElement {
        let is_selected = self.engine == engine;
        let theme = cx.theme();
        let border = if is_selected {
            theme.primary
        } else {
            theme.border
        };
        let hover_border = theme.primary.opacity(0.6);
        let selected_fill = theme.primary.opacity(SELECTED_FILL_OPACITY);

        v_flex()
            .id(engine_id(engine))
            .role(Role::Button)
            .aria_label(engine.display_name())
            .h(px(CARD_HEIGHT))
            .items_center()
            .justify_center()
            .gap_3()
            .rounded_lg()
            .border_1()
            .border_color(border)
            .when(is_selected, |card| card.bg(selected_fill))
            .cursor_pointer()
            .hover(move |style| style.border_color(hover_border))
            .child(engine_logo(engine, px(CARD_LOGO_TILE), px(CARD_LOGO), cx))
            .child(
                v_flex()
                    .items_center()
                    .gap_0p5()
                    .child(div().text_sm().font_semibold().child(engine.display_name()))
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(engine_summary(engine)),
                    ),
            )
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                this.choose_engine(engine, window, cx);

                if event.click_count() >= DOUBLE_CLICK {
                    this.show_details(window, cx);
                }
            }))
            .test_support()
            .into_any_element()
    }
}
