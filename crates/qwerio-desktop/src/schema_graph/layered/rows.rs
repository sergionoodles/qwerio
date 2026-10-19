//! Vertical placement: row centers that pull each vertex toward its
//! neighbors while keeping column order and spacing.

use super::{LOOP_HEIGHT, LOOP_STEP, Link};
use crate::schema_graph::NODE_HEIGHT;

/// Vertical space between two nodes in a column.
const NODE_GAP: f32 = 12.;

/// Vertical space between edges passing through a column, or between such an
/// edge and a node.
const PASSING_GAP: f32 = 8.;

/// Alternating passes that pull vertices toward their neighbors' rows.
const ROW_PASSES: usize = 8;

/// Vertical room a vertex needs around its row.
#[derive(Clone, Copy)]
pub(super) struct Extent {
    above: f32,
    pub(super) below: f32,
    is_node: bool,
}

impl Extent {
    pub(super) const PASSING: Self = Self {
        above: 0.,
        below: 0.,
        is_node: false,
    };

    pub(super) fn node(loop_count: usize) -> Self {
        let loops = if loop_count == 0 {
            0.
        } else {
            LOOP_HEIGHT + LOOP_STEP * (loop_count - 1) as f32
        };

        Self {
            above: NODE_HEIGHT / 2. + loops,
            below: NODE_HEIGHT / 2.,
            is_node: true,
        }
    }
}

fn separation(upper: Extent, lower: Extent) -> f32 {
    let gap = if upper.is_node && lower.is_node {
        NODE_GAP
    } else {
        PASSING_GAP
    };

    upper.below + gap + lower.above
}

/// Row centers. Columns start stacked, then repeatedly move each vertex
/// toward the mean row of its neighbors while keeping order and spacing.
pub(super) fn assign_rows(columns: &[Vec<usize>], links: &[Link], extents: &[Extent]) -> Vec<f32> {
    let vertex_count = extents.len();
    let mut left_neighbors = vec![Vec::new(); vertex_count];
    let mut right_neighbors = vec![Vec::new(); vertex_count];
    for link in links {
        left_neighbors[link.right].push(link.left);
        right_neighbors[link.left].push(link.right);
    }

    let mut rows = vec![0.; vertex_count];
    for column in columns {
        let desired = vec![0.; column.len()];
        place_column(column, &desired, extents, &mut rows);
    }

    for pass in 0..ROW_PASSES {
        let is_rightward = pass % 2 == 0;
        let order: Vec<usize> = if is_rightward {
            (1..columns.len()).collect()
        } else {
            (0..columns.len().saturating_sub(1)).rev().collect()
        };
        let neighbors = if is_rightward {
            &left_neighbors
        } else {
            &right_neighbors
        };

        for column in order.into_iter().map(|index| &columns[index]) {
            let desired: Vec<f32> = column
                .iter()
                .map(|vertex| {
                    let around = &neighbors[*vertex];
                    if around.is_empty() {
                        rows[*vertex]
                    } else {
                        around.iter().map(|neighbor| rows[*neighbor]).sum::<f32>()
                            / around.len() as f32
                    }
                })
                .collect();
            place_column(column, &desired, extents, &mut rows);
        }
    }

    let top = (0..vertex_count)
        .map(|vertex| rows[vertex] - extents[vertex].above)
        .fold(f32::INFINITY, f32::min);
    let top = if top.is_finite() { top } else { 0. };

    rows.iter().map(|row| row - top).collect()
}

/// Places a column as close to `desired` as spacing allows: the average of
/// a downward and an upward packing, both of which keep the spacing.
fn place_column(column: &[usize], desired: &[f32], extents: &[Extent], rows: &mut [f32]) {
    let count = column.len();
    let mut downward = desired.to_vec();
    let mut upward = desired.to_vec();

    for position in 1..count {
        let gap = separation(extents[column[position - 1]], extents[column[position]]);
        downward[position] = downward[position].max(downward[position - 1] + gap);
    }

    for position in (0..count.saturating_sub(1)).rev() {
        let gap = separation(extents[column[position]], extents[column[position + 1]]);
        upward[position] = upward[position].min(upward[position + 1] - gap);
    }

    for (position, vertex) in column.iter().enumerate() {
        rows[*vertex] = (downward[position] + upward[position]) / 2.;
    }
}
