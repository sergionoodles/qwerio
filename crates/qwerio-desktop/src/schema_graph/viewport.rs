//! Zoom and pan of the graph viewport. Offsets follow GPUI's scroll
//! convention: zero at the top-left, negative as the content moves up and
//! left.

use gpui_kit::{Point, Size, point};

pub const MIN_ZOOM: f32 = 0.25;
pub const MAX_ZOOM: f32 = 2.5;

/// Zoom factor of one key press.
pub const ZOOM_STEP: f32 = 1.2;

/// Wheel travel, in pixels, that zooms by one step.
const WHEEL_PIXELS_PER_STEP: f32 = 60.;

pub fn clamp_zoom(zoom: f32) -> f32 {
    zoom.clamp(MIN_ZOOM, MAX_ZOOM)
}

/// Zoom factor for a vertical wheel movement; scrolling up zooms in.
pub fn wheel_zoom_factor(pixels: f32) -> f32 {
    ZOOM_STEP.powf(pixels / WHEEL_PIXELS_PER_STEP)
}

/// The offset that keeps the content under `anchor`, a point in viewport
/// coordinates, in place while the zoom changes.
pub fn zoom_about(offset: Point<f32>, anchor: Point<f32>, from: f32, to: f32) -> Point<f32> {
    let scale = to / from;

    point(
        anchor.x - (anchor.x - offset.x) * scale,
        anchor.y - (anchor.y - offset.y) * scale,
    )
}

/// Keeps `offset` within the scrollable range of `content` in `viewport`.
pub fn clamp_offset(offset: Point<f32>, content: Size<f32>, viewport: Size<f32>) -> Point<f32> {
    let limit = |content: f32, viewport: f32| (content - viewport).max(0.);

    point(
        offset.x.clamp(-limit(content.width, viewport.width), 0.),
        offset.y.clamp(-limit(content.height, viewport.height), 0.),
    )
}

#[cfg(test)]
mod tests {
    use gpui_kit::size;

    use super::*;

    #[test]
    fn zooming_keeps_the_anchored_content_in_place() {
        let offset = point(-100., -40.);
        let anchor = point(200., 150.);
        let content_under_anchor = |offset: Point<f32>, zoom: f32| {
            point((anchor.x - offset.x) / zoom, (anchor.y - offset.y) / zoom)
        };

        let zoomed = zoom_about(offset, anchor, 1., 2.);

        assert_eq!(
            content_under_anchor(zoomed, 2.),
            content_under_anchor(offset, 1.)
        );
    }

    #[test]
    fn offsets_stay_within_the_content() {
        let content = size(1000., 300.);
        let viewport = size(400., 500.);

        assert_eq!(
            clamp_offset(point(-900., -50.), content, viewport),
            point(-600., 0.),
            "content shorter than the viewport does not scroll"
        );
        assert_eq!(
            clamp_offset(point(30., 0.), content, viewport),
            point(0., 0.)
        );
    }

    #[test]
    fn zoom_is_limited_and_the_wheel_zooms_in_steps() {
        assert_eq!(clamp_zoom(10.), MAX_ZOOM);
        assert_eq!(clamp_zoom(0.01), MIN_ZOOM);
        assert_eq!(wheel_zoom_factor(WHEEL_PIXELS_PER_STEP), ZOOM_STEP);
        assert!(wheel_zoom_factor(-WHEEL_PIXELS_PER_STEP) < 1.);
    }
}
