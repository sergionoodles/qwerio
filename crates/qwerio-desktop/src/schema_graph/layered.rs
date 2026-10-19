//! A layered layout of one connected group of objects. Referenced objects sit
//! in columns to the left of the objects that reference them. Edges that skip
//! columns pass through reserved gaps instead of crossing nodes, and node
//! order within each column minimizes crossings.

use gpui_kit::{Bounds, Size, point, size};

use super::NODE_HEIGHT;
use super::geometry::{EdgeRoute, Segment};
use ordering::{initial_columns, order_columns};
use rows::{Extent, assign_rows};

mod ordering;
mod rows;

/// Horizontal space between columns, where edges curve.
const COLUMN_GAP: f32 = 72.;

/// Space between neighboring edge ends on one side of a node.
const PORT_SPACING: f32 = 6.;

/// Keeps edge ends off a node's rounded corners.
const PORT_MARGIN: f32 = 6.;

/// Height of a self-referencing loop drawn above its node, and the extra
/// height of each further loop on the same node.
const LOOP_HEIGHT: f32 = 22.;
const LOOP_STEP: f32 = 8.;

/// A foreign key between two objects of the component, by global index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ComponentEdge {
    pub index: usize,
    pub referencing: usize,
    pub referenced: usize,
}

/// Positions relative to the component's top-left corner.
#[derive(Debug, PartialEq)]
pub struct ComponentLayout {
    pub size: Size<f32>,
    /// Bounds of each node, by global node index.
    pub nodes: Vec<(usize, Bounds<f32>)>,
    /// The route of each edge, by global edge index.
    pub routes: Vec<(usize, EdgeRoute)>,
}

/// An edge oriented left to right. Natural orientation puts the referenced
/// object on the left; edges closing a cycle are flipped.
struct OrientedEdge {
    index: usize,
    left: usize,
    right: usize,
    is_flipped: bool,
}

/// A link between vertices in adjacent columns. Vertices below the node count
/// are nodes; the rest are points where a long edge passes a column.
struct Link {
    left: usize,
    right: usize,
}

struct Chain {
    index: usize,
    links: Vec<usize>,
    is_flipped: bool,
}

/// `nodes` are global node indices in display order, `widths` their widths in
/// the same order; every edge connects two of them.
pub fn layout_component(
    nodes: &[usize],
    widths: &[f32],
    edges: &[ComponentEdge],
) -> ComponentLayout {
    let local = |global: usize| nodes.iter().position(|node| *node == global);
    let mut local_edges = Vec::new();
    let mut self_loops = vec![Vec::new(); nodes.len()];

    for edge in edges {
        let (Some(referencing), Some(referenced)) =
            (local(edge.referencing), local(edge.referenced))
        else {
            continue;
        };

        if referencing == referenced {
            self_loops[referencing].push(edge.index);
        } else {
            local_edges.push((edge.index, referencing, referenced));
        }
    }

    let oriented = orient_edges(nodes.len(), &local_edges);
    let node_columns = assign_columns(nodes.len(), &oriented);
    let Expanded {
        vertex_columns,
        links,
        chains,
    } = expand_long_edges(&node_columns, &oriented);

    let mut columns = initial_columns(&vertex_columns);
    order_columns(&mut columns, &links, &vertex_columns);

    let extents: Vec<Extent> = (0..vertex_columns.len())
        .map(|vertex| match self_loops.get(vertex) {
            Some(loops) => Extent::node(loops.len()),
            None => Extent::PASSING,
        })
        .collect();
    let rows = assign_rows(&columns, &links, &extents);

    let columns_x = ColumnsX::new(&vertex_columns, widths);
    let ports = assign_ports(&links, &rows, nodes.len());

    let routes = chains
        .iter()
        .map(|chain| {
            let route = route_chain(chain, &links, &ports, &vertex_columns, &columns_x, widths);
            // Routes run from the referencing object to the referenced one.
            let route = if chain.is_flipped {
                route
            } else {
                route.reversed()
            };
            (chain.index, route)
        })
        .chain(self_loops.iter().enumerate().flat_map(|(node, loops)| {
            let bounds = node_bounds(node, &node_columns, &rows, widths, &columns_x);
            loops
                .iter()
                .enumerate()
                .map(move |(nesting, index)| (*index, self_loop(bounds, nesting)))
        }))
        .collect();

    let width = columns_x.total_width();
    let height = (0..vertex_columns.len())
        .map(|vertex| rows[vertex] + extents[vertex].below)
        .fold(0., f32::max);

    ComponentLayout {
        size: size(width, height),
        nodes: nodes
            .iter()
            .enumerate()
            .map(|(local, global)| {
                let bounds = node_bounds(local, &node_columns, &rows, widths, &columns_x);
                (*global, bounds)
            })
            .collect(),
        routes,
    }
}

/// Where each column starts and how wide it is: as wide as its widest node.
struct ColumnsX {
    starts: Vec<f32>,
    widths: Vec<f32>,
}

impl ColumnsX {
    fn new(vertex_columns: &[usize], node_widths: &[f32]) -> Self {
        let count = vertex_columns.iter().max().map_or(0, |last| last + 1);
        let mut widths = vec![0_f32; count];

        for (vertex, width) in node_widths.iter().enumerate() {
            let column = vertex_columns[vertex];
            widths[column] = widths[column].max(*width);
        }

        let starts = widths
            .iter()
            .scan(0., |next, width| {
                let start = *next;
                *next += width + COLUMN_GAP;
                Some(start)
            })
            .collect();

        Self { starts, widths }
    }

    fn total_width(&self) -> f32 {
        match (self.starts.last(), self.widths.last()) {
            (Some(start), Some(width)) => start + width,
            _ => 0.,
        }
    }
}

fn node_bounds(
    node: usize,
    node_columns: &[usize],
    rows: &[f32],
    widths: &[f32],
    columns_x: &ColumnsX,
) -> Bounds<f32> {
    Bounds::new(
        point(
            columns_x.starts[node_columns[node]],
            rows[node] - NODE_HEIGHT / 2.,
        ),
        size(widths[node], NODE_HEIGHT),
    )
}

/// Flips the edges that close cycles, found by depth-first search in node
/// order, so the remaining graph has a left-to-right order.
fn orient_edges(node_count: usize, edges: &[(usize, usize, usize)]) -> Vec<OrientedEdge> {
    #[derive(Clone, Copy, PartialEq)]
    enum Visit {
        New,
        Active,
        Done,
    }

    fn visit(
        node: usize,
        outgoing: &[Vec<usize>],
        edges: &[(usize, usize, usize)],
        states: &mut [Visit],
        is_flipped: &mut [bool],
    ) {
        states[node] = Visit::Active;

        for &position in &outgoing[node] {
            let (_, referencing, _) = edges[position];

            match states[referencing] {
                Visit::Active => is_flipped[position] = true,
                Visit::New => visit(referencing, outgoing, edges, states, is_flipped),
                Visit::Done => {}
            }
        }

        states[node] = Visit::Done;
    }

    let mut outgoing = vec![Vec::new(); node_count];
    for (position, (_, _, referenced)) in edges.iter().enumerate() {
        outgoing[*referenced].push(position);
    }

    let mut states = vec![Visit::New; node_count];
    let mut is_flipped = vec![false; edges.len()];

    for node in 0..node_count {
        if states[node] == Visit::New {
            visit(node, &outgoing, edges, &mut states, &mut is_flipped);
        }
    }

    edges
        .iter()
        .zip(is_flipped)
        .map(|(&(index, referencing, referenced), is_flipped)| {
            let (left, right) = if is_flipped {
                (referencing, referenced)
            } else {
                (referenced, referencing)
            };

            OrientedEdge {
                index,
                left,
                right,
                is_flipped,
            }
        })
        .collect()
}

/// Longest-path columns, then each object that references nothing moves
/// right to sit just before its nearest dependent, shortening its edges.
fn assign_columns(node_count: usize, edges: &[OrientedEdge]) -> Vec<usize> {
    let mut incoming = vec![0_usize; node_count];
    for edge in edges {
        incoming[edge.right] += 1;
    }

    let mut columns = vec![0_usize; node_count];
    let mut remaining = incoming.clone();
    let mut ready: Vec<usize> = (0..node_count)
        .filter(|node| incoming[*node] == 0)
        .collect();

    while let Some(node) = ready.pop() {
        for edge in edges.iter().filter(|edge| edge.left == node) {
            columns[edge.right] = columns[edge.right].max(columns[node] + 1);
            remaining[edge.right] -= 1;

            if remaining[edge.right] == 0 {
                ready.push(edge.right);
            }
        }
    }

    for node in (0..node_count).filter(|node| incoming[*node] == 0) {
        let nearest = edges
            .iter()
            .filter(|edge| edge.left == node)
            .map(|edge| columns[edge.right])
            .min();

        if let Some(nearest) = nearest {
            columns[node] = nearest - 1;
        }
    }

    columns
}

struct Expanded {
    vertex_columns: Vec<usize>,
    links: Vec<Link>,
    chains: Vec<Chain>,
}

/// Splits edges that span several columns into links between adjacent
/// columns, adding a passing vertex in every column they cross.
fn expand_long_edges(node_columns: &[usize], edges: &[OrientedEdge]) -> Expanded {
    let mut vertex_columns = node_columns.to_vec();
    let mut links = Vec::new();
    let mut chains = Vec::new();

    for edge in edges {
        let mut chain = Vec::new();
        let mut previous = edge.left;

        for column in node_columns[edge.left] + 1..node_columns[edge.right] {
            let passing = vertex_columns.len();
            vertex_columns.push(column);
            chain.push(links.len());
            links.push(Link {
                left: previous,
                right: passing,
            });
            previous = passing;
        }

        chain.push(links.len());
        links.push(Link {
            left: previous,
            right: edge.right,
        });
        chains.push(Chain {
            index: edge.index,
            links: chain,
            is_flipped: edge.is_flipped,
        });
    }

    Expanded {
        vertex_columns,
        links,
        chains,
    }
}

/// Where each link meets its two vertices. Edges sharing a node side fan out
/// in the order of the rows they head toward, so they do not cross there.
struct Ports {
    left_ends: Vec<f32>,
    right_ends: Vec<f32>,
}

fn assign_ports(links: &[Link], rows: &[f32], node_count: usize) -> Ports {
    let mut left_ends: Vec<f32> = links.iter().map(|link| rows[link.left]).collect();
    let mut right_ends: Vec<f32> = links.iter().map(|link| rows[link.right]).collect();

    for node in 0..node_count {
        let mut leaving: Vec<usize> = (0..links.len())
            .filter(|index| links[*index].left == node)
            .collect();
        leaving.sort_by(|a, b| rows[links[*a].right].total_cmp(&rows[links[*b].right]));

        for (slot, row) in spread(rows[node], leaving.len()).into_iter().enumerate() {
            left_ends[leaving[slot]] = row;
        }

        let mut arriving: Vec<usize> = (0..links.len())
            .filter(|index| links[*index].right == node)
            .collect();
        arriving.sort_by(|a, b| rows[links[*a].left].total_cmp(&rows[links[*b].left]));

        for (slot, row) in spread(rows[node], arriving.len()).into_iter().enumerate() {
            right_ends[arriving[slot]] = row;
        }
    }

    Ports {
        left_ends,
        right_ends,
    }
}

fn spread(center: f32, count: usize) -> Vec<f32> {
    if count <= 1 {
        return vec![center; count];
    }

    let available = NODE_HEIGHT - 2. * PORT_MARGIN;
    let spacing = PORT_SPACING.min(available / (count - 1) as f32);
    let first = center - spacing * (count - 1) as f32 / 2.;

    (0..count)
        .map(|slot| first + spacing * slot as f32)
        .collect()
}

/// A left-to-right route: curves across column gaps and straight runs
/// through the columns a long edge passes.
///
/// Nodes come first among vertices, so the first link starts at its node's
/// own right side; later links start where the passing run leaves a column.
fn route_chain(
    chain: &Chain,
    links: &[Link],
    ports: &Ports,
    vertex_columns: &[usize],
    columns_x: &ColumnsX,
    node_widths: &[f32],
) -> EdgeRoute {
    let mut segments = Vec::new();

    for (position, link_index) in chain.links.iter().enumerate() {
        let link = &links[*link_index];
        let left_column = vertex_columns[link.left];
        let left_start = columns_x.starts[left_column];
        let start_x = if position == 0 {
            left_start + node_widths[link.left]
        } else {
            left_start + columns_x.widths[left_column]
        };
        let start = point(start_x, ports.left_ends[*link_index]);
        let end = point(
            columns_x.starts[vertex_columns[link.right]],
            ports.right_ends[*link_index],
        );

        if position > 0 {
            segments.push(Segment::line(point(left_start, start.y), start));
        }

        segments.push(Segment::horizontal_curve(start, end, COLUMN_GAP / 2.));
    }

    EdgeRoute::new(segments)
}

/// A loop over the top of a node, arriving from above.
fn self_loop(bounds: Bounds<f32>, nesting: usize) -> EdgeRoute {
    let height = LOOP_HEIGHT + LOOP_STEP * nesting as f32;
    let origin = bounds.origin;
    let start = point(origin.x + bounds.size.width * 0.62, origin.y);
    let end = point(origin.x + bounds.size.width * 0.38, origin.y);
    let lift = height * 4. / 3.;

    EdgeRoute::new(vec![Segment {
        from: start,
        control_a: point(start.x, start.y - lift),
        control_b: point(end.x, end.y - lift),
        to: end,
    }])
}

#[cfg(test)]
mod tests {
    use gpui_kit::Point;

    use super::*;

    fn edge(index: usize, referencing: usize, referenced: usize) -> ComponentEdge {
        ComponentEdge {
            index,
            referencing,
            referenced,
        }
    }

    const WIDTH: f32 = 120.;

    fn layout(node_count: usize, edges: &[ComponentEdge]) -> ComponentLayout {
        let nodes: Vec<usize> = (0..node_count).collect();
        layout_component(&nodes, &vec![WIDTH; node_count], edges)
    }

    fn node_bounds(layout: &ComponentLayout, node: usize) -> Bounds<f32> {
        layout
            .nodes
            .iter()
            .find(|(global, _)| *global == node)
            .map(|(_, bounds)| *bounds)
            .expect("node is placed")
    }

    fn node_origin(layout: &ComponentLayout, node: usize) -> Point<f32> {
        node_bounds(layout, node).origin
    }

    fn route(layout: &ComponentLayout, index: usize) -> &EdgeRoute {
        layout
            .routes
            .iter()
            .find(|(edge, _)| *edge == index)
            .map(|(_, route)| route)
            .expect("edge is routed")
    }

    #[test]
    fn referenced_objects_sit_left_of_their_dependents() {
        // lines -> orders -> customers
        let layout = layout(3, &[edge(0, 1, 0), edge(1, 2, 1)]);

        let customers = node_origin(&layout, 0);
        let orders = node_origin(&layout, 1);
        let lines = node_origin(&layout, 2);

        assert!(customers.x < orders.x);
        assert!(orders.x < lines.x);
        assert_eq!(customers.y, orders.y, "a chain is drawn straight");
    }

    #[test]
    fn routes_run_from_the_referencing_side_to_the_referenced_side() {
        let layout = layout(2, &[edge(7, 1, 0)]);
        let route = route(&layout, 7);

        let referenced = node_origin(&layout, 0);
        let referencing = node_origin(&layout, 1);

        assert_eq!(route.start().map(|start| start.x), Some(referencing.x));
        assert_eq!(route.end().map(|end| end.x), Some(referenced.x + WIDTH));
    }

    #[test]
    fn long_edges_pass_columns_without_crossing_nodes() {
        // 3 references 2 references 1 references 0, and 3 also references 0.
        let layout = layout(
            4,
            &[edge(0, 1, 0), edge(1, 2, 1), edge(2, 3, 2), edge(3, 3, 0)],
        );
        let long = route(&layout, 3);

        for node in [1, 2] {
            let origin = node_origin(&layout, node);
            let crosses_node = long.segments.iter().any(|segment| {
                (0..=8)
                    .map(|step| segment.at(step as f32 / 8.))
                    .any(|sample| {
                        sample.x > origin.x
                            && sample.x < origin.x + WIDTH
                            && sample.y > origin.y
                            && sample.y < origin.y + NODE_HEIGHT
                    })
            });
            assert!(!crosses_node, "the long edge crosses node {node}");
        }
    }

    #[test]
    fn cycles_and_self_references_are_laid_out() {
        let layout = layout(2, &[edge(0, 0, 1), edge(1, 1, 0), edge(2, 0, 0)]);

        assert_eq!(layout.nodes.len(), 2);
        assert_eq!(layout.routes.len(), 3);
        let loop_route = route(&layout, 2);
        let origin = node_origin(&layout, 0);
        assert!(origin.y > 0., "room is left above for the loop");
        assert_eq!(loop_route.end().map(|end| end.y), Some(origin.y));
    }

    #[test]
    fn ordering_removes_avoidable_crossings() {
        // Two parents, each with its own child; display order would cross.
        let layout = layout(4, &[edge(0, 3, 0), edge(1, 2, 1)]);

        let parent_a = node_origin(&layout, 0).y;
        let parent_b = node_origin(&layout, 1).y;
        let child_of_a = node_origin(&layout, 3).y;
        let child_of_b = node_origin(&layout, 2).y;

        assert_eq!(parent_a < parent_b, child_of_a < child_of_b);
    }

    #[test]
    fn columns_fit_their_widest_node_and_edges_leave_each_node_side() {
        // A wide and a narrow node both reference node 0.
        let widths = [80., 200., 60.];
        let layout = layout_component(&[0, 1, 2], &widths, &[edge(0, 1, 0), edge(1, 2, 0)]);

        let wide = node_bounds(&layout, 1);
        let narrow = node_bounds(&layout, 2);
        assert_eq!(wide.origin.x, narrow.origin.x, "one column");
        assert_eq!(wide.size.width, 200.);
        assert_eq!(layout.size.width, wide.origin.x + 200.);

        let narrow_route = route(&layout, 1);
        assert_eq!(
            narrow_route.start().map(|start| start.x),
            Some(narrow.origin.x),
            "dependents are entered from the left"
        );
        assert_eq!(
            narrow_route.end().map(|end| end.x),
            Some(node_bounds(&layout, 0).origin.x + 80.)
        );
    }
}
