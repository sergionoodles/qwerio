//! Moving around the graph: zoom with Ctrl and the wheel, Ctrl and +/-, or
//! the zoom buttons; pan by dragging empty space or with the scrollbars.

use gpui_kit::assets::IconName as LucideIcon;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, h_flex};
use gpui_kit::{
    Context, CursorStyle, DispatchPhase, FocusHandle, Hitbox, InteractiveElement as _, IntoElement,
    Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement as _,
    Pixels, Point, ScrollWheelEvent, Size, Styled as _, WeakEntity, Window, point, px, size,
};

use super::{Drag, GraphStatus};
use crate::explorer_view::ExplorerView;
use crate::schema_graph::viewport::{
    ZOOM_STEP, clamp_offset, clamp_zoom, wheel_zoom_factor, zoom_about,
};

/// Line height used to turn line-based wheel deltas into pixels.
const WHEEL_LINE_HEIGHT: f32 = 20.;

impl ExplorerView {
    pub(in crate::explorer_view) fn zoom_graph_in(&mut self, cx: &mut Context<Self>) {
        self.zoom_graph(ZOOM_STEP, None, cx);
    }

    pub(in crate::explorer_view) fn zoom_graph_out(&mut self, cx: &mut Context<Self>) {
        self.zoom_graph(1. / ZOOM_STEP, None, cx);
    }

    pub(in crate::explorer_view) fn reset_graph_zoom(&mut self, cx: &mut Context<Self>) {
        let factor = 1. / self.graph_viewport.zoom;
        self.zoom_graph(factor, None, cx);
    }

    /// Zooms by `factor` about `anchor`, a window position, or the middle of
    /// the viewport, keeping the content under it in place.
    fn zoom_graph(&mut self, factor: f32, anchor: Option<Point<Pixels>>, cx: &mut Context<Self>) {
        let GraphStatus::Ready(state) = &self.graph else {
            return;
        };

        let viewport = &mut self.graph_viewport;
        let from = viewport.zoom;
        let to = clamp_zoom(from * factor);
        if to == from {
            return;
        }

        let bounds = viewport.scroll.bounds();
        let anchor = anchor.map_or_else(
            || point(bounds.size.width / 2., bounds.size.height / 2.),
            |position| position - bounds.origin,
        );
        let content = state.layout.size;
        let offset = zoom_about(to_f32(viewport.scroll.offset()), to_f32(anchor), from, to);
        let offset = clamp_offset(offset, zoomed(content, to), size_to_f32(bounds.size));

        viewport.zoom = to;
        viewport
            .scroll
            .set_offset(point(px(offset.x), px(offset.y)));
        cx.notify();
    }

    fn start_graph_drag(&mut self, pointer: Point<Pixels>, cx: &mut Context<Self>) {
        let offset = self.graph_viewport.scroll.offset();
        self.graph_viewport.drag = Some(Drag { pointer, offset });
        self.hover_edge(None, pointer, cx);
        cx.notify();
    }

    /// Scrolls with the pointer while a drag is under way; false otherwise.
    fn drag_graph(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) -> bool {
        let Some(drag) = self.graph_viewport.drag else {
            return false;
        };

        if event.pressed_button != Some(MouseButton::Left) {
            self.end_graph_drag(cx);
            return false;
        }

        let GraphStatus::Ready(state) = &self.graph else {
            return false;
        };

        let viewport = &self.graph_viewport;
        let zoom = viewport.zoom;
        let moved = event.position - drag.pointer;
        let bounds = viewport.scroll.bounds();
        let offset = clamp_offset(
            to_f32(drag.offset + moved),
            zoomed(state.layout.size, zoom),
            size_to_f32(bounds.size),
        );

        viewport
            .scroll
            .set_offset(point(px(offset.x), px(offset.y)));
        cx.notify();
        true
    }

    fn end_graph_drag(&mut self, cx: &mut Context<Self>) {
        if self.graph_viewport.drag.take().is_some() {
            cx.notify();
        }
    }

    pub(super) fn is_dragging_graph(&self) -> bool {
        self.graph_viewport.drag.is_some()
    }

    pub(super) fn render_zoom_controls(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let percent = format!("{:.0}%", self.graph_viewport.zoom * 100.);

        h_flex()
            .id("graph-zoom")
            .absolute()
            .bottom_4()
            .right_4()
            .p_0p5()
            .gap_0p5()
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().popover)
            .shadow_sm()
            // Pressing the controls never starts a pan underneath.
            .block_mouse_except_scroll()
            .child(
                Button::new("graph-zoom-out")
                    .ghost()
                    .xsmall()
                    .icon(Icon::new(LucideIcon::ZoomOut))
                    .accessibility_label("Zoom Out")
                    .tooltip("Zoom Out")
                    .on_click(cx.listener(|this, _, _, cx| this.zoom_graph_out(cx))),
            )
            .child(
                Button::new("graph-zoom-reset")
                    .ghost()
                    .xsmall()
                    .label(percent)
                    .tooltip("Actual Size")
                    .on_click(cx.listener(|this, _, _, cx| this.reset_graph_zoom(cx))),
            )
            .child(
                Button::new("graph-zoom-in")
                    .ghost()
                    .xsmall()
                    .icon(Icon::new(LucideIcon::ZoomIn))
                    .accessibility_label("Zoom In")
                    .tooltip("Zoom In")
                    .on_click(cx.listener(|this, _, _, cx| this.zoom_graph_in(cx))),
            )
    }
}

/// Registers the pointer handling of the graph background: Ctrl and the
/// wheel zoom, and a press on empty space starts a pan. `hitbox` covers the
/// edge canvas, which nodes and controls block.
pub(super) fn track_viewport_input(
    view: WeakEntity<ExplorerView>,
    hitbox: &Hitbox,
    focus_handle: FocusHandle,
    is_dragging: bool,
    window: &mut Window,
) {
    if is_dragging {
        window.set_window_cursor_style(CursorStyle::ClosedHand);
    } else {
        window.set_cursor_style(CursorStyle::OpenHand, hitbox);
    }

    {
        let view = view.clone();
        window.on_mouse_event(move |event: &ScrollWheelEvent, phase, _, cx| {
            // Capture, so the scroll area does not also scroll.
            if phase != DispatchPhase::Capture || !is_zoom_modifier(&event.modifiers) {
                return;
            }

            let Some(is_over_graph) = view.upgrade().map(|view| {
                view.read(cx)
                    .graph_viewport
                    .scroll
                    .bounds()
                    .contains(&event.position)
            }) else {
                return;
            };
            if !is_over_graph {
                return;
            }

            cx.stop_propagation();
            let pixels = f32::from(event.delta.pixel_delta(px(WHEEL_LINE_HEIGHT)).y);
            view.update(cx, |this, cx| {
                this.zoom_graph(wheel_zoom_factor(pixels), Some(event.position), cx)
            })
            .ok();
        });
    }

    {
        let view = view.clone();
        let hitbox = hitbox.clone();
        window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble
                || event.button != MouseButton::Left
                || !hitbox.is_hovered(window)
            {
                return;
            }

            window.focus(&focus_handle, cx);
            view.update(cx, |this, cx| this.start_graph_drag(event.position, cx))
                .ok();
        });
    }

    {
        let view = view.clone();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
            if phase != DispatchPhase::Bubble {
                return;
            }

            view.update(cx, |this, cx| this.drag_graph(event, cx)).ok();
        });
    }

    window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
        if phase == DispatchPhase::Bubble && event.button == MouseButton::Left {
            view.update(cx, |this, cx| this.end_graph_drag(cx)).ok();
        }
    });
}

/// Ctrl on every platform, plus Command on macOS.
fn is_zoom_modifier(modifiers: &Modifiers) -> bool {
    modifiers.control || modifiers.secondary()
}

fn to_f32(position: Point<Pixels>) -> Point<f32> {
    point(f32::from(position.x), f32::from(position.y))
}

fn size_to_f32(extent: Size<Pixels>) -> Size<f32> {
    size(f32::from(extent.width), f32::from(extent.height))
}

pub(super) fn zoomed(content: Size<f32>, zoom: f32) -> Size<f32> {
    size(content.width * zoom, content.height * zoom)
}
