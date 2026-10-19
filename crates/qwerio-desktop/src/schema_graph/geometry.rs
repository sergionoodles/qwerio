//! Edge geometry: cubic segments chained into routes, plus the distance from a
//! pointer to a route for hover hit testing.

use gpui_kit::{Point, point};

/// Straight samples per cubic segment when measuring pointer distance.
const SAMPLES_PER_SEGMENT: usize = 16;

/// One cubic Bézier piece of an edge.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Segment {
    pub from: Point<f32>,
    pub control_a: Point<f32>,
    pub control_b: Point<f32>,
    pub to: Point<f32>,
}

impl Segment {
    /// A curve that leaves and arrives horizontally, `bend` pixels into the
    /// gap on each side, so edges meet node sides square-on.
    pub fn horizontal_curve(from: Point<f32>, to: Point<f32>, bend: f32) -> Self {
        let direction = if to.x >= from.x { 1. } else { -1. };

        Self {
            from,
            control_a: point(from.x + bend * direction, from.y),
            control_b: point(to.x - bend * direction, to.y),
            to,
        }
    }

    pub fn line(from: Point<f32>, to: Point<f32>) -> Self {
        Self {
            from,
            control_a: from,
            control_b: to,
            to,
        }
    }

    pub fn reversed(self) -> Self {
        Self {
            from: self.to,
            control_a: self.control_b,
            control_b: self.control_a,
            to: self.from,
        }
    }

    pub fn at(&self, t: f32) -> Point<f32> {
        let inverse = 1. - t;
        let weights = [
            inverse * inverse * inverse,
            3. * inverse * inverse * t,
            3. * inverse * t * t,
            t * t * t,
        ];
        let points = [self.from, self.control_a, self.control_b, self.to];

        points
            .iter()
            .zip(weights)
            .fold(point(0., 0.), |sum, (control, weight)| {
                point(sum.x + control.x * weight, sum.y + control.y * weight)
            })
    }

    fn distance_to(&self, target: Point<f32>) -> f32 {
        (0..SAMPLES_PER_SEGMENT)
            .map(|step| {
                let start = self.at(step as f32 / SAMPLES_PER_SEGMENT as f32);
                let end = self.at((step + 1) as f32 / SAMPLES_PER_SEGMENT as f32);
                distance_to_line(target, start, end)
            })
            .fold(f32::INFINITY, f32::min)
    }
}

/// The drawn path of one edge, from the referencing object to the one it
/// references.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EdgeRoute {
    pub segments: Vec<Segment>,
}

impl EdgeRoute {
    pub fn new(segments: Vec<Segment>) -> Self {
        Self { segments }
    }

    pub fn reversed(&self) -> Self {
        Self {
            segments: self
                .segments
                .iter()
                .rev()
                .map(|segment| segment.reversed())
                .collect(),
        }
    }

    pub fn start(&self) -> Option<Point<f32>> {
        self.segments.first().map(|segment| segment.from)
    }

    pub fn end(&self) -> Option<Point<f32>> {
        self.segments.last().map(|segment| segment.to)
    }

    /// The unit direction of travel where the route arrives at its end.
    pub fn end_direction(&self) -> Option<Point<f32>> {
        let last = self.segments.last()?;
        let tail = [last.control_b, last.control_a, last.from]
            .into_iter()
            .find(|candidate| *candidate != last.to)?;

        Some(normalized(point(last.to.x - tail.x, last.to.y - tail.y)))
    }

    /// A point halfway along the route by segment count.
    #[cfg(test)]
    pub fn midpoint(&self) -> Option<Point<f32>> {
        let count = self.segments.len();
        let middle = self.segments.get(count / 2)?;

        if count.is_multiple_of(2) {
            Some(middle.from)
        } else {
            Some(middle.at(0.5))
        }
    }

    pub fn distance_to(&self, target: Point<f32>) -> f32 {
        self.segments
            .iter()
            .map(|segment| segment.distance_to(target))
            .fold(f32::INFINITY, f32::min)
    }
}

fn normalized(vector: Point<f32>) -> Point<f32> {
    let length = vector.x.hypot(vector.y);

    if length == 0. {
        return vector;
    }

    point(vector.x / length, vector.y / length)
}

fn distance_to_line(target: Point<f32>, start: Point<f32>, end: Point<f32>) -> f32 {
    let along = point(end.x - start.x, end.y - start.y);
    let length_squared = along.x * along.x + along.y * along.y;

    let t = if length_squared == 0. {
        0.
    } else {
        (((target.x - start.x) * along.x + (target.y - start.y) * along.y) / length_squared)
            .clamp(0., 1.)
    };

    let closest = point(start.x + along.x * t, start.y + along.y * t);
    (target.x - closest.x).hypot(target.y - closest.y)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curves_leave_and_arrive_horizontally_in_either_direction() {
        let leftward = Segment::horizontal_curve(point(100., 0.), point(0., 40.), 20.);

        assert_eq!(leftward.control_a, point(80., 0.));
        assert_eq!(leftward.control_b, point(20., 40.));
        assert_eq!(leftward.at(0.), point(100., 0.));
        assert_eq!(leftward.at(1.), point(0., 40.));
    }

    #[test]
    fn distance_measures_the_closest_point_on_the_route() {
        let route = EdgeRoute::new(vec![Segment::line(point(0., 0.), point(100., 0.))]);

        assert_eq!(route.distance_to(point(50., 6.)), 6.);
        assert_eq!(route.distance_to(point(-3., -4.)), 5.);
    }

    #[test]
    fn reversing_swaps_ends_and_arrival_direction() {
        let route = EdgeRoute::new(vec![
            Segment::line(point(0., 0.), point(10., 0.)),
            Segment::horizontal_curve(point(10., 0.), point(50., 30.), 10.),
        ]);
        let reversed = route.reversed();

        assert_eq!(reversed.start(), Some(point(50., 30.)));
        assert_eq!(reversed.end(), Some(point(0., 0.)));
        assert_eq!(route.end_direction(), Some(point(1., 0.)));
        assert_eq!(reversed.end_direction(), Some(point(-1., 0.)));
    }
}
