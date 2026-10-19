//! Foreign key lines: painted on a canvas beneath the nodes, with a dot at
//! the referencing end and an arrowhead at the referenced end.

use std::sync::Arc;

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{
    Bounds, Context, DispatchPhase, Hitbox, HitboxBehavior, Hsla, IntoElement, MouseMoveEvent,
    PathBuilder, Pixels, Point, Styled as _, WeakEntity, Window, canvas, fill, point, px, size,
};

use super::navigation::track_viewport_input;
use super::{ExplorerView, Focus, GraphState};
use crate::schema_graph::{EdgeRoute, GraphLayout};

const EDGE_WIDTH: f32 = 1.25;
const FOCUSED_EDGE_WIDTH: f32 = 2.;
const EDGE_OPACITY: f32 = 0.5;
const FADED_EDGE_OPACITY: f32 = 0.12;

/// Arrowheads mark the referenced end of an edge.
const ARROW_LENGTH: f32 = 8.;
const ARROW_HALF_WIDTH: f32 = 4.;

/// A dot marks the referencing end.
const DOT_SIZE: f32 = 6.;

/// Lines and markers scale with the zoom, down to this fraction.
const MIN_LINE_SCALE: f32 = 0.6;

/// Where the canvas sits in the window, and the zoom of layout units.
#[derive(Clone, Copy)]
struct Surface {
    origin: Point<Pixels>,
    zoom: f32,
}

impl Surface {
    fn to_window(self, target: Point<f32>) -> Point<Pixels> {
        point(
            self.origin.x + px(target.x * self.zoom),
            self.origin.y + px(target.y * self.zoom),
        )
    }

    fn to_layout(self, position: Point<Pixels>) -> Point<f32> {
        let local = position - self.origin;
        point(
            f32::from(local.x) / self.zoom,
            f32::from(local.y) / self.zoom,
        )
    }
}

#[derive(Clone, Copy)]
struct EdgePaint {
    color: Hsla,
    width: f32,
}

impl ExplorerView {
    pub(super) fn render_edges(
        &self,
        state: &GraphState,
        focus: &Focus,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let resting = EdgePaint {
            color: theme.muted_foreground.opacity(EDGE_OPACITY),
            width: EDGE_WIDTH,
        };
        let faded = EdgePaint {
            color: theme.muted_foreground.opacity(FADED_EDGE_OPACITY),
            width: EDGE_WIDTH,
        };
        let focused = EdgePaint {
            color: theme.primary,
            width: FOCUSED_EDGE_WIDTH,
        };

        // Focused edges paint last, so they stay on top where lines cross.
        let mut paints: Vec<(usize, EdgePaint)> = (0..state.edges.len())
            .map(|edge| match (focus.is_active, focus.edges[edge]) {
                (true, true) => (edge, focused),
                (true, false) => (edge, faded),
                (false, _) => (edge, resting),
            })
            .collect();
        paints.sort_by_key(|(edge, _)| focus.edges[*edge]);

        let layout = Arc::clone(&state.layout);
        let view = cx.entity().downgrade();
        let zoom = self.graph_viewport.zoom;
        let focus_handle = self.focus_handle.clone();
        let is_dragging = self.is_dragging_graph();

        canvas(
            |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
            move |bounds, hitbox, window, _| {
                let surface = Surface {
                    origin: bounds.origin,
                    zoom,
                };

                for (edge, paint) in &paints {
                    paint_route(&layout.routes[*edge], surface, *paint, window);
                }

                track_viewport_input(view.clone(), &hitbox, focus_handle, is_dragging, window);
                track_edge_hover(view, layout, surface, hitbox, window);
            },
        )
        .absolute()
        .size_full()
    }
}

/// Hovers the edge under the pointer, or none when the pointer leaves the
/// graph or rests on a node, which sits above the edges.
fn track_edge_hover(
    view: WeakEntity<ExplorerView>,
    layout: Arc<GraphLayout>,
    surface: Surface,
    hitbox: Hitbox,
    window: &mut Window,
) {
    window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
        if phase != DispatchPhase::Bubble {
            return;
        }

        let pointer = event.position - surface.origin;
        let edge = if hitbox.is_hovered(window) {
            layout.edge_at(surface.to_layout(event.position))
        } else {
            None
        };

        view.update(cx, |this, cx| this.hover_edge(edge, pointer, cx))
            .ok();
    });
}

fn paint_route(route: &EdgeRoute, surface: Surface, paint: EdgePaint, window: &mut Window) {
    let to_window = |target: Point<f32>| surface.to_window(target);
    // Lines thin out when zoomed out, but never below a hairline.
    let scale = surface.zoom.max(MIN_LINE_SCALE);
    let arrow_length = ARROW_LENGTH * scale;
    let arrow_half_width = ARROW_HALF_WIDTH * scale;
    let dot_size = DOT_SIZE * scale;
    let (Some(start), Some(end), Some(direction)) =
        (route.start(), route.end(), route.end_direction())
    else {
        return;
    };

    let mut line = PathBuilder::stroke(px(paint.width * scale));
    line.move_to(to_window(start));
    for segment in &route.segments {
        line.cubic_bezier_to(
            to_window(segment.to),
            to_window(segment.control_a),
            to_window(segment.control_b),
        );
    }

    let base = point(
        end.x - direction.x * arrow_length / surface.zoom,
        end.y - direction.y * arrow_length / surface.zoom,
    );
    let across = point(
        -direction.y * arrow_half_width / surface.zoom,
        direction.x * arrow_half_width / surface.zoom,
    );
    let mut arrow = PathBuilder::fill();
    arrow.move_to(to_window(end));
    arrow.line_to(to_window(point(base.x + across.x, base.y + across.y)));
    arrow.line_to(to_window(point(base.x - across.x, base.y - across.y)));
    arrow.close();

    // Lyon rejects only malformed paths, and both start with `move_to`.
    if let Ok(path) = line.build() {
        window.paint_path(path, paint.color);
    }
    if let Ok(path) = arrow.build() {
        window.paint_path(path, paint.color);
    }

    let dot = Bounds::centered_at(to_window(start), size(px(dot_size), px(dot_size)));
    window.paint_quad(fill(dot, paint.color).corner_radii(px(dot_size / 2.)));
}
