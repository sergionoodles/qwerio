//! The relationship graph of a container's tables and views: which objects
//! reference which, as a compact diagram without columns.

mod geometry;
mod layered;
mod layout;
pub mod viewport;

pub use geometry::EdgeRoute;
pub use layout::{EdgeEnds, GraphLayout, layout_graph};

use qwerio_core::catalog::ForeignKeyInfo;
use qwerio_core::resource::ObjectRef;

pub const NODE_HEIGHT: f32 = 28.;

/// One line of the graph: every foreign key from one object to another.
#[derive(Clone, Debug, PartialEq)]
pub struct GraphEdge {
    pub ends: EdgeEnds,
    pub keys: Vec<ForeignKeyInfo>,
}

/// Groups foreign keys into one edge per (referencing, referenced) pair of
/// `nodes`, in node order. Keys to objects outside `nodes` are left out.
pub fn graph_edges(nodes: &[ObjectRef], relationships: &[ForeignKeyInfo]) -> Vec<GraphEdge> {
    let index_of = |object: &ObjectRef| {
        nodes
            .iter()
            .position(|node| node.same_location_and_name(object))
    };
    let mut edges: Vec<GraphEdge> = Vec::new();

    for key in relationships {
        let (Some(referencing), Some(referenced)) =
            (index_of(&key.table), index_of(&key.referenced_table))
        else {
            continue;
        };
        let ends = EdgeEnds {
            referencing,
            referenced,
        };

        match edges.iter_mut().find(|edge| edge.ends == ends) {
            Some(edge) => edge.keys.push(key.clone()),
            None => edges.push(GraphEdge {
                ends,
                keys: vec![key.clone()],
            }),
        }
    }

    edges.sort_by_key(|edge| (edge.ends.referencing, edge.ends.referenced));
    edges
}

#[cfg(test)]
mod tests {
    use qwerio_core::ids::ConnectionId;
    use qwerio_core::resource::ObjectKind;

    use super::*;

    fn object(connection: ConnectionId, namespace: &str, name: &str) -> ObjectRef {
        ObjectRef {
            connection,
            database: None,
            namespace: Some(namespace.to_owned()),
            kind: ObjectKind::Table,
            name: name.to_owned(),
        }
    }

    fn key(name: &str, table: &ObjectRef, referenced: &ObjectRef) -> ForeignKeyInfo {
        ForeignKeyInfo {
            name: Some(name.to_owned()),
            table: table.clone(),
            columns: vec!["id".to_owned()],
            referenced_table: referenced.clone(),
            referenced_columns: vec!["id".to_owned()],
        }
    }

    #[test]
    fn keys_between_the_same_objects_share_one_edge() {
        let connection = ConnectionId::new();
        let people = object(connection, "public", "people");
        let tasks = object(connection, "public", "tasks");
        let relationships = [
            key("tasks_owner", &tasks, &people),
            key("tasks_reviewer", &tasks, &people),
            key("people_manager", &people, &people),
        ];

        let edges = graph_edges(&[people, tasks], &relationships);

        assert_eq!(edges.len(), 2);
        assert_eq!(
            edges[0].ends,
            EdgeEnds {
                referencing: 0,
                referenced: 0
            }
        );
        assert_eq!(edges[1].keys.len(), 2);
    }

    #[test]
    fn keys_to_objects_outside_the_graph_are_left_out() {
        let connection = ConnectionId::new();
        let orders = object(connection, "sales", "orders");
        let elsewhere = object(connection, "billing", "accounts");

        let edges = graph_edges(
            std::slice::from_ref(&orders),
            &[key("orders_account", &orders, &elsewhere)],
        );

        assert!(edges.is_empty());
    }
}
