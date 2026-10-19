//! The card that describes the hovered relationship: which object
//! references which, and the column pairs of each foreign key.

use gpui_kit::component::{ActiveTheme as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, Role,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, div, px,
};

use super::navigation::zoomed;
use super::{ExplorerView, GraphState};
use crate::schema_graph::GraphEdge;

const CARD_WIDTH: f32 = 280.;
/// Distance between the pointer and the relationship card.
const CARD_OFFSET: f32 = 14.;
/// Below this distance from the bottom, the card opens above the pointer.
const CARD_FLIP_MARGIN: f32 = 160.;

impl ExplorerView {
    pub(super) fn render_relationship_card(
        &self,
        state: &GraphState,
        edge: usize,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let GraphEdge { ends, keys } = state.edges.get(edge)?;
        let name_of = |node: usize| {
            state
                .entries
                .get(node)
                .and_then(|index| self.entries.get(*index))
                .map(|entry| entry.name.clone())
                .unwrap_or_default()
        };
        let referencing = name_of(ends.referencing);
        let referenced = name_of(ends.referenced);
        let summary = if ends.referencing == ends.referenced {
            format!("{referencing} references itself")
        } else {
            format!("{referencing} references {referenced}")
        };

        let theme = cx.theme();
        let pointer = state.hover.pointer;
        let zoom = self.graph_viewport.zoom;
        let graph_size = zoomed(state.layout.size, zoom);
        let opens_left = f32::from(pointer.x) + CARD_OFFSET + CARD_WIDTH > graph_size.width;
        let opens_up = f32::from(pointer.y) + CARD_FLIP_MARGIN > graph_size.height;

        let key_rows = keys.iter().map(|key| {
            let pairs = key
                .columns
                .iter()
                .zip(&key.referenced_columns)
                .map(|(from, to)| {
                    h_flex()
                        .gap_1()
                        .font_family(theme.mono_font_family.clone())
                        .child(from.clone())
                        .child(div().text_color(theme.muted_foreground).child("→"))
                        .child(to.clone())
                });

            v_flex()
                .gap_0p5()
                .child(
                    div().text_color(theme.muted_foreground).child(
                        key.name
                            .clone()
                            .unwrap_or_else(|| "Unnamed foreign key".to_owned()),
                    ),
                )
                .children(pairs)
        });

        let card = v_flex()
            .id("relationship-details")
            .absolute()
            .w(px(CARD_WIDTH))
            .p_3()
            .gap_2()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .text_color(theme.popover_foreground)
            .shadow_lg()
            .text_xs()
            .map(|card| {
                if opens_left {
                    card.right(px(graph_size.width) - pointer.x + px(CARD_OFFSET))
                } else {
                    card.left(pointer.x + px(CARD_OFFSET))
                }
            })
            .map(|card| {
                if opens_up {
                    card.bottom(px(graph_size.height) - pointer.y + px(CARD_OFFSET))
                } else {
                    card.top(pointer.y + px(CARD_OFFSET))
                }
            })
            .child(div().text_sm().font_medium().child(summary.clone()))
            .children(key_rows)
            .role(Role::Tooltip)
            .aria_label(summary)
            .test_support();

        Some(card.into_any_element())
    }
}
