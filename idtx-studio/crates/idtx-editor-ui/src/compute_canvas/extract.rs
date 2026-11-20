//! Build a canvas [`Graph`] from a backward-walked `idtx_usd::ComputeGraph`.
//!
//! Unlike the material canvas, the compute bridge carries socket type names
//! inline, so no second `inspect_prim` round-trip is needed.

use std::collections::HashMap;

use idtx_usd::{ComputeGraph as UsdComputeGraph, ComputeNodeRole};

use super::theme::{accent_compute, accent_consumer, accent_datasource, accent_other};
use crate::graph_canvas::graph::{
    ColumnPin, Connection, Graph, Node, NodeId, NodeRole, Socket, SocketKind, classify_type_name,
    node_id_for_path, split_connection_target, strip_ns,
};

/// The pre-fetched USD data the Compute Graph panel needs to build its graph.
/// Populated by the shell's cache-refresh logic and passed into
/// [`compute_canvas_panel`](super::compute_canvas_panel).
#[derive(Debug)]
pub struct ComputeGraphSource {
    /// The selected (consumer) prim path — the graph's cache-invalidation key.
    pub root_path: String,
    /// Stage-topology generation at the time this source was built.
    pub topology_generation: u64,
    /// The backward-walked compute graph from the C++ bridge (types inline).
    pub usd_graph: UsdComputeGraph,
}

/// Header accent + node role for a compute node's classified role.
fn role_appearance(role: ComputeNodeRole) -> (NodeRole, [u8; 3]) {
    match role {
        ComputeNodeRole::Datasource => (NodeRole::Standard, accent_datasource()),
        ComputeNodeRole::Compute => (NodeRole::Standard, accent_compute()),
        ComputeNodeRole::Consumer => (NodeRole::SinkAggregate, accent_consumer()),
        ComputeNodeRole::Other => (NodeRole::Standard, accent_other()),
    }
}

/// Build a canvas [`Graph`] from a [`ComputeGraphSource`].
#[must_use]
pub fn extract_compute_graph(source: &ComputeGraphSource) -> Graph {
    let usd_graph = &source.usd_graph;

    // ── 1. Build a Node per compute prim ──────────────────────────────────
    let mut nodes: Vec<Node> = Vec::with_capacity(usd_graph.nodes.len());
    for cn in &usd_graph.nodes {
        let (role, accent) = role_appearance(cn.role);

        let inputs: Vec<Socket> = cn
            .inputs
            .iter()
            .map(|s| Socket {
                name: strip_ns(&s.name).to_owned(),
                full_name: s.name.clone(),
                kind: SocketKind::Input,
                ty: classify_type_name(&s.type_name),
                // Wire beats literal: a connected input shows no value box.
                authored_value: (s.connections.is_empty() && !s.value_summary.is_empty())
                    .then(|| s.value_summary.clone()),
            })
            .collect();

        let outputs: Vec<Socket> = cn
            .outputs
            .iter()
            .map(|s| Socket {
                name: strip_ns(&s.name).to_owned(),
                full_name: s.name.clone(),
                kind: SocketKind::Output,
                ty: classify_type_name(&s.type_name),
                authored_value: None,
            })
            .collect();

        let display_name = cn.path.rsplit('/').next().unwrap_or(&cn.path).to_owned();
        let subtitle = (!cn.type_name.is_empty()).then(|| cn.type_name.clone());

        nodes.push(Node {
            id: node_id_for_path(&cn.path),
            prim_path: cn.path.clone(),
            display_name,
            role,
            pin: ColumnPin::None,
            accent_rgb: Some(accent),
            subtitle,
            inputs,
            outputs,
        });
    }

    // ── 2. Harvest connections (and synthesise any missing output socket) ─
    let node_id_map: HashMap<NodeId, usize> =
        nodes.iter().enumerate().map(|(i, n)| (n.id, i)).collect();

    let mut connections: Vec<Connection> = Vec::new();
    for cn in &usd_graph.nodes {
        let dst_id = node_id_for_path(&cn.path);
        let Some(&dst_idx) = node_id_map.get(&dst_id) else {
            continue;
        };
        for sock in &cn.inputs {
            let Some(dst_sock_idx) = nodes[dst_idx].find_input(&sock.name) else {
                continue;
            };
            for conn in &sock.connections {
                let (src_prim, src_attr_opt) = split_connection_target(conn);
                let Some(src_attr) = src_attr_opt else {
                    continue;
                };
                let src_id = node_id_for_path(&src_prim);
                let Some(&src_idx) = node_id_map.get(&src_id) else {
                    continue;
                };
                // Find, or synthesise, the source output socket.
                let src_sock_idx = if let Some(i) = nodes[src_idx].find_output(&src_attr) {
                    i
                } else {
                    nodes[src_idx].outputs.push(Socket {
                        name: strip_ns(&src_attr).to_owned(),
                        full_name: src_attr,
                        kind: SocketKind::Output,
                        ty: crate::graph_canvas::graph::SocketType::Other,
                        authored_value: None,
                    });
                    nodes[src_idx].outputs.len() - 1
                };
                connections.push(Connection {
                    from: (src_id, src_sock_idx),
                    to: (dst_id, dst_sock_idx),
                });
            }
        }
    }

    Graph {
        graph_key: source.root_path.clone(),
        nodes,
        connections,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph_canvas::graph::SocketType;
    use idtx_usd::{ComputeNode, ComputeSocket};

    /// Synthetic `MockDatasource → Compute_ValueFromJson → Cube.size` chain.
    fn chain_source() -> ComputeGraphSource {
        ComputeGraphSource {
            root_path: "/World/Cube".to_owned(),
            topology_generation: 1,
            usd_graph: UsdComputeGraph {
                root_path: Some("/World/Cube".to_owned()),
                diagnostics: vec![],
                nodes: vec![
                    ComputeNode {
                        path: "/World/Cube".to_owned(),
                        type_name: "Cube".to_owned(),
                        role: ComputeNodeRole::Consumer,
                        inputs: vec![ComputeSocket {
                            name: "size".to_owned(),
                            type_name: "double".to_owned(),
                            value_summary: "1.0".to_owned(),
                            connections: vec![
                                "/World/Data/Extract.outputs:jsonValue:double".to_owned(),
                            ],
                        }],
                        outputs: vec![],
                    },
                    ComputeNode {
                        path: "/World/Data/Extract".to_owned(),
                        type_name: "Compute_ValueFromJson".to_owned(),
                        role: ComputeNodeRole::Compute,
                        inputs: vec![ComputeSocket {
                            name: "inputs:jsonData".to_owned(),
                            type_name: "string".to_owned(),
                            value_summary: String::new(),
                            connections: vec!["/World/Data/Src.outputs:data".to_owned()],
                        }],
                        outputs: vec![ComputeSocket {
                            name: "outputs:jsonValue:double".to_owned(),
                            type_name: "double".to_owned(),
                            value_summary: String::new(),
                            connections: vec![],
                        }],
                    },
                    ComputeNode {
                        path: "/World/Data/Src".to_owned(),
                        type_name: "MockDatasource_RandomFloat".to_owned(),
                        role: ComputeNodeRole::Datasource,
                        inputs: vec![],
                        outputs: vec![ComputeSocket {
                            name: "outputs:data".to_owned(),
                            type_name: "string".to_owned(),
                            value_summary: String::new(),
                            connections: vec![],
                        }],
                    },
                ],
            },
        }
    }

    #[test]
    fn extracts_chain_nodes_and_edges() {
        let g = extract_compute_graph(&chain_source());
        assert_eq!(g.graph_key, "/World/Cube");
        assert_eq!(g.nodes.len(), 3);
        assert_eq!(g.connections.len(), 2);
    }

    #[test]
    fn consumer_is_sink_and_double_sockets_are_float() {
        let g = extract_compute_graph(&chain_source());
        let cube = g.node_by_path("/World/Cube").expect("cube node");
        assert_eq!(cube.role, NodeRole::SinkAggregate);
        assert_eq!(cube.accent_rgb, Some(accent_consumer()));
        let size = &cube.inputs[0];
        assert_eq!(size.name, "size");
        assert_eq!(size.ty, SocketType::Float);
        // Connected input shows no authored value box.
        assert_eq!(size.authored_value, None);
    }

    #[test]
    fn datasource_is_a_root_with_no_inputs() {
        let g = extract_compute_graph(&chain_source());
        let src = g.node_by_path("/World/Data/Src").expect("src node");
        assert_eq!(src.role, NodeRole::Standard);
        assert_eq!(src.accent_rgb, Some(accent_datasource()));
        assert!(src.inputs.is_empty());
        assert_eq!(src.outputs.len(), 1);
    }

    /// Intermediate compute nodes have mixed inputs: some connected (wire beats
    /// constant → no value box) and some plain-named constant-value attrs without
    /// an `inputs:` prefix (e.g. `jsonPath`, `jsonValueType` on
    /// `Compute_ValueFromJson`). The C++ bridge now includes the latter for
    /// non-consumer nodes; extract must surface all of them as visible pins.
    #[test]
    fn inbetween_node_shows_constant_value_pins() {
        let source = ComputeGraphSource {
            root_path: "/World/Cube".to_owned(),
            topology_generation: 1,
            usd_graph: UsdComputeGraph {
                root_path: Some("/World/Cube".to_owned()),
                diagnostics: vec![],
                nodes: vec![
                    ComputeNode {
                        path: "/World/Cube".to_owned(),
                        type_name: "Cube".to_owned(),
                        role: ComputeNodeRole::Consumer,
                        inputs: vec![ComputeSocket {
                            name: "size".to_owned(),
                            type_name: "double".to_owned(),
                            value_summary: "1.0".to_owned(),
                            connections: vec![
                                "/World/Data/Extract.outputs:jsonValue:double".to_owned(),
                            ],
                        }],
                        outputs: vec![],
                    },
                    ComputeNode {
                        path: "/World/Data/Extract".to_owned(),
                        type_name: "Compute_ValueFromJson".to_owned(),
                        role: ComputeNodeRole::Compute,
                        inputs: vec![
                            // Connected input: wire takes precedence, no value box.
                            ComputeSocket {
                                name: "inputs:jsonData".to_owned(),
                                type_name: "string".to_owned(),
                                value_summary: "{}".to_owned(),
                                connections: vec!["/World/Data/Src.outputs:data".to_owned()],
                            },
                            // Plain-named constant inputs (no "inputs:" prefix, no
                            // connection) — the C++ bridge now surfaces these for
                            // non-consumer nodes so the canvas shows a full picture.
                            ComputeSocket {
                                name: "jsonPath".to_owned(),
                                type_name: "string".to_owned(),
                                value_summary: "$.data.value".to_owned(),
                                connections: vec![],
                            },
                            ComputeSocket {
                                name: "jsonValueType".to_owned(),
                                type_name: "token".to_owned(),
                                value_summary: "double".to_owned(),
                                connections: vec![],
                            },
                        ],
                        outputs: vec![ComputeSocket {
                            name: "outputs:jsonValue:double".to_owned(),
                            type_name: "double".to_owned(),
                            value_summary: String::new(),
                            connections: vec![],
                        }],
                    },
                    ComputeNode {
                        path: "/World/Data/Src".to_owned(),
                        type_name: "MockDatasource_RandomFloat".to_owned(),
                        role: ComputeNodeRole::Datasource,
                        inputs: vec![],
                        outputs: vec![ComputeSocket {
                            name: "outputs:data".to_owned(),
                            type_name: "string".to_owned(),
                            value_summary: String::new(),
                            connections: vec![],
                        }],
                    },
                ],
            },
        };

        let g = extract_compute_graph(&source);
        let extract_node = g.node_by_path("/World/Data/Extract").expect("extract node");

        // All 3 inputs must appear as visible pins.
        assert_eq!(
            extract_node.inputs.len(),
            3,
            "expected 3 input pins on intermediate node"
        );

        // Connected input: pin visible but no value box (wire takes precedence).
        let json_data = extract_node
            .inputs
            .iter()
            .find(|s| s.name == "jsonData")
            .expect("jsonData pin");
        assert_eq!(json_data.authored_value, None);

        // Constant-value inputs without "inputs:" prefix: pin visible WITH value box.
        let json_path = extract_node
            .inputs
            .iter()
            .find(|s| s.name == "jsonPath")
            .expect("jsonPath pin");
        assert_eq!(
            json_path.authored_value,
            Some("$.data.value".to_owned())
        );

        let json_value_type = extract_node
            .inputs
            .iter()
            .find(|s| s.name == "jsonValueType")
            .expect("jsonValueType pin");
        assert_eq!(
            json_value_type.authored_value,
            Some("double".to_owned())
        );

        // Connections: Datasource→Extract + Extract→Consumer = 2.
        assert_eq!(g.connections.len(), 2);
    }
}
