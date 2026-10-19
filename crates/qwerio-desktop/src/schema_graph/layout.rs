//! The whole graph: connected groups laid out separately and packed into
//! rows, largest first, then objects without relationships wrapped in rows
//! below.

use gpui_kit::{Bounds, Point, Size, point, size};

use super::NODE_HEIGHT;
use super::geometry::{EdgeRoute, Segment};
use super::layered::{ComponentEdge, layout_component};

/// Space around the whole graph.
const PADDING: f32 = 24.;

/// Space between connected groups.
const GROUP_GAP: f32 = 40.;

/// Groups and unrelated objects wrap to a new row past this width, unless
/// one group is wider.
const ROW_WIDTH: f32 = 900.;

/// Space between the related groups and the unrelated objects.
const SECTION_GAP: f32 = 36.;

/// Room for the heading above the unrelated objects.
const HEADING_HEIGHT: f32 = 24.;

/// Space between unrelated objects.
const FLOW_GAP: f32 = 10.;

/// How close the pointer must be to an edge to hover it.
pub const HOVER_TOLERANCE: f32 = 6.;

/// A foreign key edge between two nodes, by node index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EdgeEnds {
    pub referencing: usize,
    pub referenced: usize,
}

#[derive(Debug, PartialEq)]
pub struct GraphLayout {
    pub size: Size<f32>,
    pub nodes: Vec<Bounds<f32>>,
    /// The route of each edge, from the referencing node to the referenced.
    pub routes: Vec<EdgeRoute>,
    /// Where to label the objects without relationships, if any.
    pub unrelated_heading: Option<Point<f32>>,
}

impl GraphLayout {
    /// The edge closest to `target`, within the hover tolerance.
    pub fn edge_at(&self, target: Point<f32>) -> Option<usize> {
        self.routes
            .iter()
            .enumerate()
            .map(|(index, route)| (index, route.distance_to(target)))
            .filter(|(_, distance)| *distance <= HOVER_TOLERANCE)
            .min_by(|left, right| left.1.total_cmp(&right.1))
            .map(|(index, _)| index)
    }
}

/// Lays out nodes of the given widths, in display order, and their edges.
pub fn layout_graph(widths: &[f32], edges: &[EdgeEnds]) -> GraphLayout {
    let node_count = widths.len();
    let mut nodes = vec![Bounds::default(); node_count];
    let mut routes = vec![EdgeRoute::default(); edges.len()];

    let groups = connected_groups(widths, edges);
    let widest = groups
        .iter()
        .map(|group| group.size.width)
        .fold(0., f32::max);
    let row_width = widest.max(ROW_WIDTH);

    let mut cursor = point(PADDING, PADDING);
    let mut row_height: f32 = 0.;
    let mut content_width: f32 = 0.;

    for group in &groups {
        if cursor.x > PADDING && cursor.x - PADDING + group.size.width > row_width {
            cursor = point(PADDING, cursor.y + row_height + GROUP_GAP);
            row_height = 0.;
        }

        for (node, bounds) in &group.nodes {
            nodes[*node] = Bounds::new(
                point(bounds.origin.x + cursor.x, bounds.origin.y + cursor.y),
                bounds.size,
            );
        }

        for (edge, route) in &group.routes {
            routes[*edge] = offset_route(route, cursor);
        }

        content_width = content_width.max(cursor.x - PADDING + group.size.width);
        row_height = row_height.max(group.size.height);
        cursor.x += group.size.width + GROUP_GAP;
    }

    let related_bottom = if groups.is_empty() {
        PADDING
    } else {
        cursor.y + row_height + SECTION_GAP
    };

    let unrelated: Vec<usize> = (0..node_count)
        .filter(|node| !edges.iter().any(|edge| touches(edge, *node)))
        .collect();

    let (unrelated_heading, bottom) = if unrelated.is_empty() {
        (None, related_bottom - SECTION_GAP + PADDING)
    } else {
        let flow_width = content_width.max(ROW_WIDTH);
        let mut flow = point(0., related_bottom + HEADING_HEIGHT);

        for node in &unrelated {
            let width = widths[*node];

            if flow.x > 0. && flow.x + width > flow_width {
                flow = point(0., flow.y + NODE_HEIGHT + FLOW_GAP);
            }

            nodes[*node] = Bounds::new(point(PADDING + flow.x, flow.y), size(width, NODE_HEIGHT));
            content_width = content_width.max(flow.x + width);
            flow.x += width + FLOW_GAP;
        }

        let bottom = flow.y + NODE_HEIGHT + PADDING;

        (Some(point(PADDING, related_bottom)), bottom)
    };

    GraphLayout {
        size: size(content_width + 2. * PADDING, bottom),
        nodes,
        routes,
        unrelated_heading,
    }
}

fn touches(edge: &EdgeEnds, node: usize) -> bool {
    edge.referencing == node || edge.referenced == node
}

struct Group {
    size: Size<f32>,
    nodes: Vec<(usize, Bounds<f32>)>,
    routes: Vec<(usize, EdgeRoute)>,
}

/// Groups of nodes joined by edges, largest first, then in display order.
fn connected_groups(widths: &[f32], edges: &[EdgeEnds]) -> Vec<Group> {
    let node_count = widths.len();
    let mut group_of: Vec<Option<usize>> = vec![None; node_count];
    let mut members: Vec<Vec<usize>> = Vec::new();

    for start in 0..node_count {
        if group_of[start].is_some() || !edges.iter().any(|edge| touches(edge, start)) {
            continue;
        }

        let group = members.len();
        let mut pending = vec![start];
        let mut found = Vec::new();
        group_of[start] = Some(group);

        while let Some(node) = pending.pop() {
            found.push(node);

            for edge in edges.iter().filter(|edge| touches(edge, node)) {
                for neighbor in [edge.referencing, edge.referenced] {
                    if group_of[neighbor].is_none() {
                        group_of[neighbor] = Some(group);
                        pending.push(neighbor);
                    }
                }
            }
        }

        found.sort_unstable();
        members.push(found);
    }

    let mut groups: Vec<(usize, Group)> = members
        .into_iter()
        .enumerate()
        .map(|(group, nodes)| {
            let group_edges: Vec<ComponentEdge> = edges
                .iter()
                .enumerate()
                .filter(|(_, edge)| group_of[edge.referencing] == Some(group))
                .map(|(index, edge)| ComponentEdge {
                    index,
                    referencing: edge.referencing,
                    referenced: edge.referenced,
                })
                .collect();
            let group_widths: Vec<f32> = nodes.iter().map(|node| widths[*node]).collect();
            let layout = layout_component(&nodes, &group_widths, &group_edges);

            (
                nodes.len(),
                Group {
                    size: layout.size,
                    nodes: layout.nodes,
                    routes: layout.routes,
                },
            )
        })
        .collect();

    // Stable, so equal-sized groups keep display order.
    groups.sort_by_key(|(size, _)| std::cmp::Reverse(*size));
    groups.into_iter().map(|(_, group)| group).collect()
}

fn offset_route(route: &EdgeRoute, offset: Point<f32>) -> EdgeRoute {
    let shift = |target: Point<f32>| point(target.x + offset.x, target.y + offset.y);

    EdgeRoute::new(
        route
            .segments
            .iter()
            .map(|segment| Segment {
                from: shift(segment.from),
                control_a: shift(segment.control_a),
                control_b: shift(segment.control_b),
                to: shift(segment.to),
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use gpui_kit::Bounds;

    use super::*;

    fn ends(referencing: usize, referenced: usize) -> EdgeEnds {
        EdgeEnds {
            referencing,
            referenced,
        }
    }

    const WIDTH: f32 = 120.;

    fn node_bounds(layout: &GraphLayout, node: usize) -> Bounds<f32> {
        layout.nodes[node]
    }

    #[test]
    fn nodes_never_overlap_and_fit_the_canvas() {
        // Two groups, a self-reference, and two unrelated objects.
        let edges = [ends(1, 0), ends(2, 1), ends(2, 0), ends(4, 3), ends(5, 5)];
        let layout = layout_graph(&[WIDTH; 8], &edges);

        for first in 0..8 {
            let bounds = node_bounds(&layout, first);
            assert!(bounds.origin.x >= 0. && bounds.origin.y >= 0.);
            assert!(bounds.bottom_right().x <= layout.size.width);
            assert!(bounds.bottom_right().y <= layout.size.height);

            for second in first + 1..8 {
                assert!(
                    !bounds.intersects(&node_bounds(&layout, second)),
                    "nodes {first} and {second} overlap"
                );
            }
        }
    }

    #[test]
    fn unrelated_objects_are_gathered_below_a_heading() {
        let layout = layout_graph(&[WIDTH; 3], &[ends(1, 0)]);
        let heading = layout.unrelated_heading.expect("a heading for node 2");

        assert!(heading.y > layout.nodes[0].bottom_right().y);
        assert!(layout.nodes[2].origin.y > heading.y);
    }

    #[test]
    fn unrelated_objects_wrap_in_rows_of_their_own_widths() {
        let mut widths = vec![60., 200.];
        widths.extend([WIDTH; 20]);
        let layout = layout_graph(&widths, &[]);

        assert!(layout.routes.is_empty());
        assert!(layout.unrelated_heading.is_some());
        assert_eq!(layout.nodes[0].origin.y, layout.nodes[1].origin.y);
        assert_eq!(
            layout.nodes[1].origin.x,
            layout.nodes[0].origin.x + 60. + FLOW_GAP
        );
        assert!(
            layout.nodes[21].origin.y > layout.nodes[0].origin.y,
            "wraps"
        );
        assert!(layout.size.width <= ROW_WIDTH + 2. * PADDING);
    }

    #[test]
    fn the_pointer_hovers_the_nearest_edge_within_tolerance() {
        let layout = layout_graph(&[WIDTH; 2], &[ends(1, 0)]);
        let middle = layout.routes[0].midpoint().expect("a routed edge");

        assert_eq!(layout.edge_at(middle), Some(0));
        assert_eq!(
            layout.edge_at(point(middle.x, middle.y + HOVER_TOLERANCE * 3.)),
            None
        );
    }
}
