//! The graph layout of the explorer: tables and views as nodes, foreign keys
//! as lines from the referencing object to the one it references. Hovering a
//! line describes its keys; hovering a node highlights its relationships.
//! Nodes are sized to their names, and the whole graph scales with the zoom.

use std::sync::Arc;

use gpui_kit::component::button::Button;
use gpui_kit::component::scroll::Scrollbar;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Bounds, Context, InteractiveElement as _, IntoElement, ParentElement as _,
    Pixels, Point, Role, ScrollHandle, SharedString, StatefulInteractiveElement as _, Styled as _,
    TestSupportExt as _, div, font, point, px, size,
};
use qwerio_core::catalog::ForeignKeyInfo;
use qwerio_core::ids::ConnectionId;
use qwerio_core::resource::{CatalogPath, ObjectRef};
use qwerio_workspace::explorer::{EntryKind, EntryTarget, Location};

use super::{ExplorerLayout, ExplorerView};
use crate::content::entry_icon;
use crate::schema_graph::{EdgeEnds, GraphEdge, GraphLayout, graph_edges, layout_graph};
use crate::workspace_model::CatalogLoad;

mod card;
mod edges;
mod navigation;

const FADED_NODE_OPACITY: f32 = 0.4;

/// Node text size at 100% zoom; nodes are sized to fit names at this size.
const NODE_TEXT_SIZE: f32 = 12.;
const NODE_ICON_SIZE: f32 = 12.;
const NODE_PADDING_X: f32 = 8.;
const NODE_ICON_GAP: f32 = 6.;

/// Everything in a node besides its name: padding, icon, gap, borders, and
/// a little slack so measured names are not cut by rounding.
const NODE_CHROME: f32 = 2. * NODE_PADDING_X + NODE_ICON_SIZE + NODE_ICON_GAP + 4.;

/// Longer names are truncated; their full name is in the relationship card
/// and the list layouts.
const MIN_NODE_WIDTH: f32 = 64.;
const MAX_NODE_WIDTH: f32 = 168.;

/// Advance assumed for a character the font cannot measure, in ems.
const FALLBACK_ADVANCE: f32 = 0.6;

const HEADING_TEXT_SIZE: f32 = 11.;

#[derive(Default)]
pub(super) enum GraphStatus {
    /// Another layout is showing, or the container has no tables or views.
    #[default]
    Hidden,
    Loading,
    Failed(SharedString),
    Ready(GraphState),
}

pub(super) struct GraphState {
    /// The explorer entry (an index into the filtered entries) of each node.
    entries: Vec<usize>,
    /// Entry keys of the nodes, to skip rebuilding from unchanged input.
    entry_keys: Vec<String>,
    relationships: Arc<[ForeignKeyInfo]>,
    edges: Vec<GraphEdge>,
    layout: Arc<GraphLayout>,
    hover: GraphHover,
}

/// Zoom and scroll position. They outlive graph rebuilds, so refreshing or
/// filtering keeps the view where the user left it.
pub(super) struct GraphViewport {
    zoom: f32,
    scroll: ScrollHandle,
    drag: Option<Drag>,
}

impl Default for GraphViewport {
    fn default() -> Self {
        Self {
            zoom: 1.,
            scroll: ScrollHandle::new(),
            drag: None,
        }
    }
}

/// Where a drag on empty space started: the pointer, in window coordinates,
/// and the scroll offset at that moment.
#[derive(Clone, Copy, Debug)]
struct Drag {
    pointer: Point<Pixels>,
    offset: Point<Pixels>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct GraphHover {
    edge: Option<usize>,
    node: Option<usize>,
    /// Pointer position over the zoomed graph, where the relationship card
    /// opens.
    pointer: Point<Pixels>,
}

/// Edges and nodes to draw in focus while something is hovered.
struct Focus {
    edges: Vec<bool>,
    nodes: Vec<bool>,
    is_active: bool,
}

impl Focus {
    fn of(state: &GraphState) -> Self {
        let mut focus = Self {
            edges: vec![false; state.edges.len()],
            nodes: vec![false; state.entries.len()],
            is_active: false,
        };

        let touches = |ends: &EdgeEnds| match (state.hover.edge, state.hover.node) {
            (Some(_), _) => false,
            (None, Some(node)) => ends.referencing == node || ends.referenced == node,
            (None, None) => false,
        };

        for (index, edge) in state.edges.iter().enumerate() {
            if state.hover.edge == Some(index) || touches(&edge.ends) {
                focus.edges[index] = true;
                focus.nodes[edge.ends.referencing] = true;
                focus.nodes[edge.ends.referenced] = true;
            }
        }

        if let Some(node) = state.hover.node {
            focus.nodes[node] = true;
        }

        focus.is_active = state.hover.edge.is_some() || state.hover.node.is_some();
        focus
    }
}

#[derive(Clone, Copy)]
struct NodeLook {
    is_selected: bool,
    /// Part of the hovered relationship or node.
    is_focused: bool,
    /// Something else is hovered.
    is_faded: bool,
}

impl ExplorerView {
    /// Whether this location lists tables or views to draw.
    pub fn can_show_graph(&self) -> bool {
        matches!(self.location, Location::Container { .. })
            && self
                .all_entries
                .iter()
                .any(|entry| matches!(entry.kind, EntryKind::Object(_)))
    }

    /// The layout actually shown: the graph falls back to the list where
    /// there is nothing to draw.
    pub fn effective_layout(&self) -> ExplorerLayout {
        if self.layout == ExplorerLayout::Graph && !self.can_show_graph() {
            return ExplorerLayout::Details;
        }

        self.layout
    }

    fn graph_container(&self) -> Option<(ConnectionId, CatalogPath)> {
        if self.effective_layout() != ExplorerLayout::Graph {
            return None;
        }

        match &self.location {
            Location::Container { connection, path } => Some((*connection, path.clone())),
            _ => None,
        }
    }

    pub(super) fn load_relationships(&mut self, force: bool, cx: &mut Context<Self>) {
        if let Some((connection, path)) = self.graph_container() {
            self.model.update(cx, |model, cx| {
                model.ensure_relationships(connection, path, force, cx)
            });
        }
    }

    pub(super) fn rebuild_graph(&mut self, cx: &mut Context<Self>) {
        let Some((connection, path)) = self.graph_container() else {
            self.graph = GraphStatus::Hidden;
            return;
        };

        let relationships = match self.model.read(cx).relationships(connection, &path) {
            CatalogLoad::Loading => {
                self.graph = GraphStatus::Loading;
                return;
            }
            CatalogLoad::Failed(error) => {
                self.graph = GraphStatus::Failed(error);
                return;
            }
            CatalogLoad::Loaded(relationships) => relationships,
        };

        let (entries, objects): (Vec<usize>, Vec<ObjectRef>) = self
            .entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| match &entry.target {
                EntryTarget::Open(Location::Object(object)) => Some((index, object.clone())),
                _ => None,
            })
            .unzip();
        let entry_keys: Vec<String> = entries
            .iter()
            .map(|index| self.entries[*index].key.clone())
            .collect();

        if let GraphStatus::Ready(state) = &self.graph
            && state.entries == entries
            && state.entry_keys == entry_keys
            && Arc::ptr_eq(&state.relationships, &relationships)
        {
            return;
        }

        let names: Vec<&str> = entries
            .iter()
            .map(|index| self.entries[*index].name.as_str())
            .collect();
        let widths = node_widths(&names, cx);
        let edges = graph_edges(&objects, &relationships);
        let ends: Vec<EdgeEnds> = edges.iter().map(|edge| edge.ends).collect();
        let layout = Arc::new(layout_graph(&widths, &ends));

        self.graph = GraphStatus::Ready(GraphState {
            entries,
            entry_keys,
            relationships,
            edges,
            layout,
            hover: GraphHover::default(),
        });
    }

    pub(super) fn graph_status_text(&self) -> Option<SharedString> {
        if self.effective_layout() != ExplorerLayout::Graph {
            return None;
        }

        let GraphStatus::Ready(state) = &self.graph else {
            return None;
        };

        let keys: usize = state.edges.iter().map(|edge| edge.keys.len()).sum();
        let noun = if keys == 1 {
            "relationship"
        } else {
            "relationships"
        };

        Some(format!("{} items · {keys} {noun}", state.entries.len()).into())
    }

    fn hover_edge(&mut self, edge: Option<usize>, pointer: Point<Pixels>, cx: &mut Context<Self>) {
        let GraphStatus::Ready(state) = &mut self.graph else {
            return;
        };
        let edge = if self.graph_viewport.drag.is_some() {
            None
        } else {
            edge
        };

        let hover = GraphHover {
            edge,
            pointer: if edge.is_some() {
                pointer
            } else {
                state.hover.pointer
            },
            ..state.hover
        };

        if hover != state.hover {
            state.hover = hover;
            cx.notify();
        }
    }

    fn hover_node(&mut self, node: usize, is_hovered: bool, cx: &mut Context<Self>) {
        let GraphStatus::Ready(state) = &mut self.graph else {
            return;
        };

        let hovered = match (is_hovered, state.hover.node) {
            (true, _) => Some(node),
            (false, Some(current)) if current == node => None,
            (false, current) => current,
        };

        if hovered != state.hover.node {
            state.hover.node = hovered;
            cx.notify();
        }
    }

    pub(super) fn render_graph(
        &self,
        selected: Option<usize>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match &self.graph {
            GraphStatus::Hidden | GraphStatus::Loading => v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .child(Spinner::new())
                .into_any_element(),
            GraphStatus::Failed(error) => v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_2()
                .p_6()
                .child(div().font_medium().child("Could not load relationships"))
                .child(
                    div()
                        .max_w_96()
                        .text_sm()
                        .text_center()
                        .text_color(cx.theme().muted_foreground)
                        .child(error.clone()),
                )
                .child(
                    Button::new("retry-relationships")
                        .outline()
                        .label("Retry")
                        .on_click(cx.listener(|this, _, _, cx| this.load_relationships(true, cx))),
                )
                .into_any_element(),
            GraphStatus::Ready(state) => self.render_ready_graph(state, selected, cx),
        }
    }

    fn render_ready_graph(
        &self,
        state: &GraphState,
        selected: Option<usize>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let focus = Focus::of(state);
        let zoom = self.graph_viewport.zoom;
        let scroll = self.graph_viewport.scroll.clone();

        let nodes: Vec<AnyElement> = state
            .entries
            .iter()
            .enumerate()
            .map(|(node, entry_index)| {
                let look = NodeLook {
                    is_selected: selected == Some(*entry_index),
                    is_focused: focus.is_active && focus.nodes[node],
                    is_faded: focus.is_active && !focus.nodes[node],
                };
                let bounds = scale_bounds(state.layout.nodes[node], zoom);
                self.render_node(node, *entry_index, look, bounds, cx)
            })
            .collect();

        let heading = state.layout.unrelated_heading.map(|origin| {
            let text = if state.edges.is_empty() {
                "No relationships between these tables and views"
            } else {
                "Not related to other tables or views"
            };

            div()
                .absolute()
                .left(px(origin.x * zoom))
                .top(px(origin.y * zoom))
                .text_size(px(HEADING_TEXT_SIZE * zoom))
                .font_medium()
                .text_color(cx.theme().muted_foreground)
                .child(text)
        });

        let card = state
            .hover
            .edge
            .and_then(|edge| self.render_relationship_card(state, edge, cx));

        div()
            .relative()
            .size_full()
            .child(
                div()
                    .id("explorer-graph")
                    .size_full()
                    .overflow_scroll()
                    .track_scroll(&scroll)
                    .child(
                        div()
                            .id("explorer-graph-content")
                            .relative()
                            .flex_none()
                            .w(px(state.layout.size.width * zoom))
                            .h(px(state.layout.size.height * zoom))
                            .child(self.render_edges(state, &focus, cx))
                            .children(heading)
                            .children(nodes)
                            .children(card)
                            .test_support(),
                    ),
            )
            .child(div().absolute().inset_0().child(Scrollbar::new(&scroll)))
            .child(self.render_zoom_controls(cx))
            .into_any_element()
    }

    fn render_node(
        &self,
        node: usize,
        entry_index: usize,
        look: NodeLook,
        bounds: Bounds<f32>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let zoom = self.graph_viewport.zoom;
        let NodeLook {
            is_selected,
            is_focused,
            is_faded,
        } = look;
        let entry = &self.entries[entry_index];
        let theme = cx.theme();
        let border = if is_selected || is_focused {
            theme.primary
        } else {
            theme.border
        };

        h_flex()
            .id(SharedString::from(entry.key.clone()))
            .absolute()
            .left(px(bounds.origin.x))
            .top(px(bounds.origin.y))
            .w(px(bounds.size.width))
            .h(px(bounds.size.height))
            .px(px(NODE_PADDING_X * zoom))
            .gap(px(NODE_ICON_GAP * zoom))
            .rounded(theme.radius * zoom)
            .border_1()
            .border_color(border)
            .bg(theme.background)
            .shadow_xs()
            .text_size(px(NODE_TEXT_SIZE * zoom))
            .when(is_selected, |node| node.bg(theme.list_active))
            .when(!is_selected, |node| {
                node.hover(|style| style.bg(theme.list_hover))
            })
            .when(is_faded, |node| node.opacity(FADED_NODE_OPACITY))
            .child(
                entry_icon(entry.kind, cx)
                    .flex_none()
                    .size(px(NODE_ICON_SIZE * zoom)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(entry.name.clone()),
            )
            // Pressing a node selects it rather than starting a pan.
            .block_mouse_except_scroll()
            .on_hover(cx.listener(move |this, is_hovered: &bool, _, cx| {
                this.hover_node(node, *is_hovered, cx)
            }))
            .on_any_mouse_down(cx.listener(move |this, _, _, _| {
                this.context_entry = this.entries.get(entry_index).cloned();
            }))
            .on_click(cx.listener(move |this, event, window, cx| {
                this.on_row_click(entry_index, event, window, cx)
            }))
            .role(Role::ListItem)
            .aria_label(entry.name.clone())
            .aria_selected(is_selected)
            .test_support()
            .into_any_element()
    }
}

/// Node widths that fit each name at the base text size, within limits.
pub(crate) fn node_widths(names: &[&str], cx: &App) -> Vec<f32> {
    let text_system = cx.text_system();
    let font_id = text_system.resolve_font(&font(cx.theme().font_family.clone()));
    let font_size = px(NODE_TEXT_SIZE);

    names
        .iter()
        .map(|name| {
            let text_width: f32 = name
                .chars()
                .map(|character| {
                    text_system
                        .advance(font_id, font_size, character)
                        .map_or(NODE_TEXT_SIZE * FALLBACK_ADVANCE, |advance| {
                            f32::from(advance.width)
                        })
                })
                .sum();

            (NODE_CHROME + text_width).clamp(MIN_NODE_WIDTH, MAX_NODE_WIDTH)
        })
        .collect()
}

fn scale_bounds(bounds: Bounds<f32>, zoom: f32) -> Bounds<f32> {
    Bounds::new(
        point(bounds.origin.x * zoom, bounds.origin.y * zoom),
        size(bounds.size.width * zoom, bounds.size.height * zoom),
    )
}
