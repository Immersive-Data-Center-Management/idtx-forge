//! First-fit column-sweep auto layout for the graph canvas.

use std::collections::{HashMap, HashSet, VecDeque};

use egui::Vec2;
use num_traits::ToPrimitive;

use super::graph::{ColumnPin, Graph, NodeId};
use super::theme::{
    COLUMN_GAP, NODE_WIDTH, ROW_GAP, SOCKET_ROW_HEIGHT, node_height_for_socket_rows,
};

/// Fill missing positions in `existing` for every node in `graph` using
/// a longest-path column sweep.
///
/// Never overwrites a position already present in `existing` — user drags
/// and prior layouts survive. Cycle-tolerant. Nodes with [`ColumnPin::First`]
/// are forced to column 0 and [`ColumnPin::Last`] to the final column.
pub fn auto_layout<H: ::std::hash::BuildHasher>(
    graph: &Graph,
    existing: &mut HashMap<NodeId, Vec2, H>,
) {
    if graph.nodes.iter().all(|n| existing.contains_key(&n.id)) {
        return;
    }

    let id_to_index: HashMap<NodeId, usize> = graph
        .nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.id, i))
        .collect();

    let first_pin_idx: Option<usize> = graph.nodes.iter().position(|n| n.pin == ColumnPin::First);
    let last_pin_idx: Option<usize> = graph.nodes.iter().position(|n| n.pin == ColumnPin::Last);

    let mut in_edges: Vec<Vec<usize>> = vec![Vec::new(); graph.nodes.len()];
    let mut out_edges: Vec<Vec<usize>> = vec![Vec::new(); graph.nodes.len()];
    for conn in &graph.connections {
        let (src_id, _) = conn.from;
        let (dst_id, _) = conn.to;
        if let (Some(&si), Some(&di)) = (id_to_index.get(&src_id), id_to_index.get(&dst_id)) {
            if si == di {
                continue;
            }
            in_edges[di].push(si);
            out_edges[si].push(di);
        }
    }

    let n = graph.nodes.len();
    let mut layer: Vec<i32> = vec![-1; n];
    let mut remaining_in: Vec<usize> = in_edges.iter().map(Vec::len).collect();
    let mut queue: VecDeque<usize> = VecDeque::new();
    for (i, &r) in remaining_in.iter().enumerate() {
        if r == 0 {
            layer[i] = 0;
            queue.push_back(i);
        }
    }
    while let Some(u) = queue.pop_front() {
        let ul = layer[u];
        for &v in &out_edges[u] {
            let candidate = ul + 1;
            if candidate > layer[v] {
                layer[v] = candidate;
            }
            remaining_in[v] -= 1;
            if remaining_in[v] == 0 {
                queue.push_back(v);
            }
        }
    }
    let mut cycle_hit = false;
    for l in &mut layer {
        if *l < 0 {
            *l = 0;
            cycle_hit = true;
        }
    }
    if cycle_hit {
        tracing::warn!(
            target: "graph_canvas",
            "graph contains a cycle; cycle members pinned to column 0"
        );
    }

    if let Some(fi) = first_pin_idx
        && layer[fi] != 0
    {
        for (i, l) in layer.iter_mut().enumerate() {
            if i != fi {
                *l += 1;
            }
        }
        layer[fi] = 0;
    }
    let max_layer = layer.iter().copied().max().unwrap_or(0);
    if let Some(li) = last_pin_idx {
        layer[li] = max_layer + 1;
    }

    let mut columns: Vec<Vec<usize>> = Vec::new();
    for (i, &l) in layer.iter().enumerate() {
        let l = l.max(0).to_usize().unwrap_or_default();
        if columns.len() <= l {
            columns.resize_with(l + 1, Vec::new);
        }
        columns[l].push(i);
    }

    let already_placed_ids: HashSet<NodeId> = existing.keys().copied().collect();

    for (col_index, column) in columns.iter().enumerate() {
        let x = f32::from(col_index.to_u16().unwrap_or(u16::MAX)) * (NODE_WIDTH + COLUMN_GAP);
        let mut y_cursor: f32 = 0.0;
        for &node_idx in column {
            let node = &graph.nodes[node_idx];
            let rows = node.inputs.len().max(node.outputs.len());
            let h = node_height_for_socket_rows(rows).max(SOCKET_ROW_HEIGHT);
            if already_placed_ids.contains(&node.id) {
                let placed = existing[&node.id];
                y_cursor = y_cursor.max(placed.y + h + ROW_GAP);
                continue;
            }
            existing.insert(node.id, Vec2::new(x, y_cursor));
            y_cursor += h + ROW_GAP;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::graph::{
        ColumnPin, Connection, Graph, Node, NodeRole, Socket, SocketKind, SocketType,
    };
    use super::*;

    fn mk_node(path: &str) -> Node {
        Node {
            id: super::super::graph::node_id_for_path(path),
            prim_path: path.to_owned(),
            display_name: path.rsplit('/').next().unwrap_or("").to_owned(),
            role: NodeRole::Standard,
            pin: ColumnPin::None,
            accent_rgb: None,
            subtitle: None,
            inputs: vec![Socket {
                name: "in".to_owned(),
                full_name: "inputs:in".to_owned(),
                kind: SocketKind::Input,
                ty: SocketType::Float,
                authored_value: None,
            }],
            outputs: vec![Socket {
                name: "out".to_owned(),
                full_name: "outputs:out".to_owned(),
                kind: SocketKind::Output,
                ty: SocketType::Float,
                authored_value: None,
            }],
        }
    }

    #[test]
    fn empty_layout_gets_filled() {
        let a = mk_node("/M/A");
        let b = mk_node("/M/B");
        let a_id = a.id;
        let b_id = b.id;
        let graph = Graph {
            graph_key: "/M".to_owned(),
            nodes: vec![a, b],
            connections: vec![Connection {
                from: (a_id, 0),
                to: (b_id, 0),
            }],
        };
        let mut positions: HashMap<NodeId, Vec2> = HashMap::new();
        auto_layout(&graph, &mut positions);
        assert!(positions.contains_key(&a_id));
        assert!(positions.contains_key(&b_id));
        assert!(positions[&b_id].x > positions[&a_id].x);
    }

    #[test]
    fn existing_positions_preserved() {
        let a = mk_node("/M/A");
        let b = mk_node("/M/B");
        let a_id = a.id;
        let b_id = b.id;
        let graph = Graph {
            graph_key: "/M".to_owned(),
            nodes: vec![a, b],
            connections: vec![Connection {
                from: (a_id, 0),
                to: (b_id, 0),
            }],
        };
        let mut positions: HashMap<NodeId, Vec2> = HashMap::new();
        positions.insert(a_id, Vec2::new(999.0, 999.0));
        auto_layout(&graph, &mut positions);
        assert_eq!(positions[&a_id], Vec2::new(999.0, 999.0));
        assert!(positions.contains_key(&b_id));
    }

    #[test]
    fn longest_path_columns() {
        let a = mk_node("/M/A");
        let b = mk_node("/M/B");
        let c = mk_node("/M/C");
        let d = mk_node("/M/D");
        let (ai, bi, ci, di) = (a.id, b.id, c.id, d.id);
        let graph = Graph {
            graph_key: "/M".to_owned(),
            nodes: vec![a, b, c, d],
            connections: vec![
                Connection {
                    from: (ai, 0),
                    to: (bi, 0),
                },
                Connection {
                    from: (bi, 0),
                    to: (ci, 0),
                },
                Connection {
                    from: (di, 0),
                    to: (ci, 0),
                },
            ],
        };
        let mut positions: HashMap<NodeId, Vec2> = HashMap::new();
        auto_layout(&graph, &mut positions);
        assert!(positions[&ci].x > positions[&bi].x);
        assert!(positions[&ci].x > positions[&di].x);
    }
}
