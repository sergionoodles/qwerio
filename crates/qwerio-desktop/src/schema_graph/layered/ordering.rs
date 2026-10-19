//! Node order within each column: barycenter sweeps that keep the ordering
//! with the fewest edge crossings.

use super::Link;

/// Barycenter sweeps when ordering columns; the best ordering seen wins.
const ORDER_SWEEPS: usize = 12;

pub(super) fn initial_columns(vertex_columns: &[usize]) -> Vec<Vec<usize>> {
    let count = vertex_columns.iter().max().map_or(0, |last| last + 1);
    let mut columns = vec![Vec::new(); count];

    for (vertex, column) in vertex_columns.iter().enumerate() {
        columns[*column].push(vertex);
    }

    columns
}

/// Barycenter ordering, sweeping right then left, keeping the ordering with
/// the fewest crossings.
pub(super) fn order_columns(columns: &mut [Vec<usize>], links: &[Link], vertex_columns: &[usize]) {
    let vertex_count = vertex_columns.len();
    let mut left_neighbors = vec![Vec::new(); vertex_count];
    let mut right_neighbors = vec![Vec::new(); vertex_count];
    for link in links {
        left_neighbors[link.right].push(link.left);
        right_neighbors[link.left].push(link.right);
    }

    let mut best = columns.to_vec();
    let mut best_crossings = count_crossings(columns, links, vertex_columns);

    for sweep in 0..ORDER_SWEEPS {
        if best_crossings == 0 {
            break;
        }

        if sweep % 2 == 0 {
            for column in 1..columns.len() {
                let fixed = positions_of(columns, vertex_count);
                sort_by_barycenter(&mut columns[column], &left_neighbors, &fixed);
            }
        } else {
            for column in (0..columns.len().saturating_sub(1)).rev() {
                let fixed = positions_of(columns, vertex_count);
                sort_by_barycenter(&mut columns[column], &right_neighbors, &fixed);
            }
        }

        let crossings = count_crossings(columns, links, vertex_columns);
        if crossings < best_crossings {
            best_crossings = crossings;
            best = columns.to_vec();
        }
    }

    columns.clone_from_slice(&best);
}

fn positions_of(columns: &[Vec<usize>], vertex_count: usize) -> Vec<f32> {
    let mut positions = vec![0.; vertex_count];

    for column in columns {
        for (position, vertex) in column.iter().enumerate() {
            positions[*vertex] = position as f32;
        }
    }

    positions
}

/// Vertices without neighbors on the fixed side keep their place.
fn sort_by_barycenter(column: &mut [usize], neighbors: &[Vec<usize>], fixed: &[f32]) {
    let mut keyed: Vec<(f32, usize)> = column
        .iter()
        .enumerate()
        .map(|(position, vertex)| {
            let around = &neighbors[*vertex];
            let key = if around.is_empty() {
                position as f32
            } else {
                around.iter().map(|neighbor| fixed[*neighbor]).sum::<f32>() / around.len() as f32
            };
            (key, *vertex)
        })
        .collect();

    keyed.sort_by(|left, right| left.0.total_cmp(&right.0));

    for (slot, (_, vertex)) in column.iter_mut().zip(keyed) {
        *slot = vertex;
    }
}

fn count_crossings(columns: &[Vec<usize>], links: &[Link], vertex_columns: &[usize]) -> usize {
    let positions = positions_of(columns, vertex_columns.len());
    let mut crossings = 0;

    for (index, first) in links.iter().enumerate() {
        for second in &links[index + 1..] {
            let is_same_gap = vertex_columns[first.left] == vertex_columns[second.left];
            let left_order = positions[first.left] - positions[second.left];
            let right_order = positions[first.right] - positions[second.right];

            if is_same_gap && left_order * right_order < 0. {
                crossings += 1;
            }
        }
    }

    crossings
}
