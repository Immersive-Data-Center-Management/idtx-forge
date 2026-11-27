//! Build a canvas [`Graph`] from pre-fetched USD material data.
//!
//! The studio does not expose a `Prim` abstraction with an `.attributes()`
//! iterator. Instead, extraction is driven by:
//!   - `idtx_usd::MaterialGraph`  — connection graph from the FFI bridge
//!   - `idtx_usd::PropertyRecord` — attribute metadata from `inspect_prim`

use std::collections::{HashMap, HashSet};

use idtx_usd::{MaterialGraph as UsdMaterialGraph, PropertyRecord};

use crate::graph_canvas::graph::{
    ColumnPin, Connection, Graph, Node, NodeId, NodeRole, Socket, SocketKind, SocketType,
    aggregate_node_id, classify_type_name, node_id_for_path, split_connection_target, strip_ns,
};

const MATERIAL_INPUTS_TAG: &str = "material-inputs";
const MATERIAL_OUTPUTS_TAG: &str = "material-outputs";

// ── Type maps from PropertyRecord slices ─────────────────────────────────────

/// Build a `full_name → type_name` map from a slice of property records,
/// keeping only `inputs:*` and `outputs:*` attribute records.
fn socket_type_map(records: &[PropertyRecord]) -> HashMap<String, String> {
    records
        .iter()
        .filter(|r| {
            (r.name.starts_with("inputs:") || r.name.starts_with("outputs:"))
                && matches!(r.kind, idtx_usd::PropertyKind::Attribute)
        })
        .map(|r| (r.name.clone(), r.type_name.clone()))
        .collect()
}

// ── Extraction ────────────────────────────────────────────────────────────────

/// The pre-fetched USD data that the canvas panel needs to build or
/// refresh its graph. Populated by the shell's cache-refresh logic and
/// passed into [`material_canvas_panel`](super::material_canvas_panel).
#[derive(Debug)]
pub struct CanvasGraphSource {
    /// The resolved material prim path (may differ from the selected prim
    /// when the selected prim carries an inherited material binding).
    pub material_path: String,
    /// Stage-topology generation at the time this source was built —
    /// used as the cache-invalidation key.
    pub topology_generation: u64,
    /// The pre-parsed connection graph from the C++ bridge.
    pub usd_graph: UsdMaterialGraph,
    /// Property records for the material prim itself (for `outputs:*` /
    /// `inputs:*` sockets on the aggregate nodes).
    pub material_properties: Vec<PropertyRecord>,
    /// Per-shader-node property records, keyed by prim path.
    pub node_properties: HashMap<String, Vec<PropertyRecord>>,
}

/// Build a canvas [`Graph`] from a [`CanvasGraphSource`].
///
/// Flaws addressed vs. a naïve port:
/// - Synthetic output sockets are created from connection targets so
///   wires never dangle even when the shader's `outputs:*` attr was not
///   reported by `inspect_prim`.
/// - Connection-driven shader inputs have their `authored_value` cleared
///   (wire beats literal in the paint pass).
/// - Material aggregate nodes are suppressed when empty to avoid
///   rendering vacant rectangles.
/// - Deterministic socket ordering (alphabetical) so re-extraction is
///   stable across topology-only bumps.
#[must_use]
pub fn extract_canvas_graph(source: &CanvasGraphSource) -> Graph {
    let material_path = &source.material_path;

    // Type maps for the material prim and each shader node.
    let mat_type_map = socket_type_map(&source.material_properties);

    // ── 1. Build Shader nodes from usd_graph.nodes ────────────────────────
    let mut nodes: Vec<Node> = Vec::new();
    let mut shader_paths: Vec<&str> = Vec::new();

    for usd_node in &source.usd_graph.nodes {
        let node_type_map = source
            .node_properties
            .get(&usd_node.path)
            .map(|r| socket_type_map(r))
            .unwrap_or_default();

        // Collect output sockets — synthesise from connection targets so
        // every wire terminus exists even when inspect_prim omitted it.
        let mut outputs: Vec<Socket> = Vec::new();
        let mut output_names: HashSet<String> = HashSet::new();

        // First, add any `outputs:*` attributes reported by inspect_prim.
        for (full_name, type_name) in &node_type_map {
            if full_name.starts_with("outputs:") {
                outputs.push(Socket {
                    name: strip_ns(full_name).to_owned(),
                    full_name: full_name.clone(),
                    kind: SocketKind::Output,
                    ty: classify_type_name(type_name),
                    authored_value: None,
                });
                output_names.insert(full_name.clone());
            }
        }

        // Then synthesise any output referenced by a connection that we
        // didn't find in inspect_prim.
        for mat_input in &usd_node.inputs {
            for conn in &mat_input.connections {
                let (conn_prim, conn_attr) = split_connection_target(conn);
                if conn_prim == usd_node.path {
                    // Self-referential (unusual); skip.
                    continue;
                }
                // Collect output attr names used as sources on OTHER
                // shader prims only when processed from that prim's loop
                // below — not here (we are on the *destination* node).
                let _ = conn_attr;
            }
        }

        // Build input sockets.
        let mut inputs: Vec<Socket> = usd_node
            .inputs
            .iter()
            .map(|mi| {
                let full_name = format!("inputs:{}", mi.name);
                let ty = node_type_map
                    .get(&full_name)
                    .map_or(SocketType::Other, |t| classify_type_name(t));
                let authored_value = if mi.connections.is_empty() && !mi.value_summary.is_empty() {
                    Some(mi.value_summary.clone())
                } else {
                    None
                };
                Socket {
                    name: mi.name.clone(),
                    full_name,
                    kind: SocketKind::Input,
                    ty,
                    authored_value,
                }
            })
            .collect();

        inputs.sort_by(|a, b| a.name.cmp(&b.name));
        outputs.sort_by(|a, b| a.name.cmp(&b.name));

        let display_name = usd_node
            .path
            .rsplit('/')
            .next()
            .unwrap_or(&usd_node.path)
            .to_owned();
        let subtitle = if usd_node.identifier.is_empty() {
            None
        } else {
            Some(usd_node.identifier.clone())
        };

        nodes.push(Node {
            id: node_id_for_path(&usd_node.path),
            prim_path: usd_node.path.clone(),
            display_name,
            role: NodeRole::Standard,
            pin: ColumnPin::None,
            accent_rgb: None,
            subtitle,
            inputs,
            outputs,
        });
        shader_paths.push(&usd_node.path);
    }

    // ── 2. Synthesise output sockets on each shader from connection edges ─
    // Walk all shader inputs' connection lists. When a connection points
    // to *another shader node*'s output, ensure that output socket exists.
    let path_to_node_idx: HashMap<String, usize> = nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.prim_path.clone(), i))
        .collect();

    for usd_node in &source.usd_graph.nodes {
        for mi in &usd_node.inputs {
            for conn in &mi.connections {
                let (src_prim, src_attr_opt) = split_connection_target(conn);
                let Some(src_attr) = src_attr_opt else {
                    continue;
                };
                if !src_attr.starts_with("outputs:") {
                    continue;
                }
                if src_prim == *material_path {
                    continue; // → MaterialInputs fanout; handled later
                }
                if let Some(&src_idx) = path_to_node_idx.get(&src_prim)
                    && nodes[src_idx].find_output(&src_attr).is_none()
                {
                    nodes[src_idx].outputs.push(Socket {
                        name: strip_ns(&src_attr).to_owned(),
                        full_name: src_attr,
                        kind: SocketKind::Output,
                        ty: SocketType::Other,
                        authored_value: None,
                    });
                }
            }
        }
    }

    // ── 3. Discover which material inputs are referenced by shaders ───────
    let mut referenced_material_inputs: HashSet<String> = HashSet::new();
    for usd_node in &source.usd_graph.nodes {
        for mi in &usd_node.inputs {
            for conn in &mi.connections {
                let (conn_prim, conn_attr) = split_connection_target(conn);
                if conn_prim == *material_path
                    && let Some(attr) = conn_attr
                    && attr.starts_with("inputs:")
                {
                    referenced_material_inputs.insert(attr);
                }
            }
        }
    }

    // ── 4. MaterialInputs aggregate ───────────────────────────────────────
    let inputs_id = aggregate_node_id(material_path, MATERIAL_INPUTS_TAG);

    // Union of authored material inputs + referenced ones.
    let mut input_names: Vec<String> = source
        .material_properties
        .iter()
        .filter(|r| {
            r.name.starts_with("inputs:") && matches!(r.kind, idtx_usd::PropertyKind::Attribute)
        })
        .map(|r| r.name.clone())
        .chain(referenced_material_inputs.iter().cloned())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    input_names.sort();

    let mat_input_sockets: Vec<(Socket, Socket)> = input_names
        .iter()
        .map(|full_name| {
            let leaf = strip_ns(full_name).to_owned();
            let (ty, authored_value) = match mat_type_map.get(full_name) {
                Some(t) => {
                    let ty = classify_type_name(t);
                    // authored value from property summary
                    let val = source
                        .material_properties
                        .iter()
                        .find(|r| &r.name == full_name)
                        .and_then(|r| {
                            if r.value_summary.is_empty() {
                                None
                            } else {
                                Some(r.value_summary.clone())
                            }
                        });
                    (ty, val)
                }
                None => (SocketType::Other, None),
            };
            let left = Socket {
                name: leaf.clone(),
                full_name: full_name.clone(),
                kind: SocketKind::Input,
                ty,
                authored_value,
            };
            let right = Socket {
                name: leaf,
                full_name: full_name.clone(),
                kind: SocketKind::Output,
                ty,
                authored_value: None,
            };
            (left, right)
        })
        .collect();

    if !mat_input_sockets.is_empty() {
        let (lefts, rights): (Vec<Socket>, Vec<Socket>) = mat_input_sockets.into_iter().unzip();
        nodes.insert(
            0,
            Node {
                id: inputs_id,
                prim_path: material_path.clone(),
                display_name: "Inputs".to_owned(),
                role: NodeRole::SourceAggregate,
                pin: ColumnPin::First,
                accent_rgb: None,
                subtitle: None,
                inputs: lefts,
                outputs: rights,
            },
        );
    }

    // ── 5. MaterialOutputs aggregate ──────────────────────────────────────
    let outputs_id = aggregate_node_id(material_path, MATERIAL_OUTPUTS_TAG);

    // Collect `outputs:*` attributes from the material prim.
    let mut output_attrs: Vec<String> = source
        .material_properties
        .iter()
        .filter(|r| {
            r.name.starts_with("outputs:") && matches!(r.kind, idtx_usd::PropertyKind::Attribute)
        })
        .map(|r| r.name.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    // Also pick up outputs that appear only as connection targets in
    // `usd_graph` (the material node may not have declared the attribute
    // explicitly but the connection shim surfaced a terminal for it).
    if let Some(ref usd_mat_path) = source.usd_graph.material_path {
        // Any MaterialNode input connection whose source is the material
        // path (should not happen for outputs) — skip. Instead look at
        // `usd_graph.material_paths` which in some render-context layouts
        // can carry the terminal names, but typically the surface/displacement
        // terminals come through `material_properties.outputs:*`.
        let _ = usd_mat_path;
    }

    output_attrs.sort();

    if !output_attrs.is_empty() {
        let sockets: Vec<Socket> = output_attrs
            .iter()
            .map(|full_name| Socket {
                name: strip_ns(full_name).to_owned(),
                full_name: full_name.clone(),
                kind: SocketKind::Input, // drawn on left edge of the MaterialOutputs rect
                ty: mat_type_map
                    .get(full_name)
                    .map_or(SocketType::Other, |t| classify_type_name(t)),
                authored_value: None,
            })
            .collect();
        nodes.push(Node {
            id: outputs_id,
            prim_path: material_path.clone(),
            display_name: "Outputs".to_owned(),
            role: NodeRole::SinkAggregate,
            pin: ColumnPin::Last,
            accent_rgb: None,
            subtitle: None,
            inputs: sockets,
            outputs: Vec::new(),
        });
    }

    // Rebuild the path→index map now that MaterialInputs/Outputs may have
    // been inserted.
    let node_id_map: HashMap<NodeId, usize> =
        nodes.iter().enumerate().map(|(i, n)| (n.id, i)).collect();

    // ── 6. Harvest connections ────────────────────────────────────────────
    let mut connections: Vec<Connection> = Vec::new();

    for usd_node in &source.usd_graph.nodes {
        let dst_node_id = node_id_for_path(&usd_node.path);
        let Some(&dst_idx_in_nodes) = node_id_map.get(&dst_node_id) else {
            continue;
        };

        for mi in &usd_node.inputs {
            let dst_full = format!("inputs:{}", mi.name);
            let Some(dst_sock_idx) = nodes[dst_idx_in_nodes].find_input(&dst_full) else {
                continue;
            };

            for conn in &mi.connections {
                let (src_prim, src_attr_opt) = split_connection_target(conn);
                let Some(src_attr) = src_attr_opt else {
                    continue;
                };

                if src_prim == *material_path && src_attr.starts_with("inputs:") {
                    // Wire from MaterialInputs fan-out socket.
                    let Some(&mi_node_idx) = node_id_map.get(&inputs_id) else {
                        continue;
                    };
                    let Some(fanout_idx) = nodes[mi_node_idx].find_output(&src_attr) else {
                        continue;
                    };
                    connections.push(Connection {
                        from: (inputs_id, fanout_idx),
                        to: (dst_node_id, dst_sock_idx),
                    });
                } else if src_attr.starts_with("outputs:") {
                    // Wire from another shader node's output.
                    let src_node_id = node_id_for_path(&src_prim);
                    let Some(&src_idx_in_nodes) = node_id_map.get(&src_node_id) else {
                        continue;
                    };
                    let Some(src_sock_idx) = nodes[src_idx_in_nodes].find_output(&src_attr) else {
                        continue;
                    };
                    connections.push(Connection {
                        from: (src_node_id, src_sock_idx),
                        to: (dst_node_id, dst_sock_idx),
                    });
                }
            }
        }
    }

    // ── 7. Wire shader outputs → MaterialOutputs ──────────────────────────
    // Use the material_output_connections from the bridge: these are the
    // `outputs:*` attrs on the material prim and their connection targets,
    // e.g. `("outputs:surface", "/World/M/surfaceShader.outputs:surface")`.
    for (mat_attr, target_str) in &source.usd_graph.material_output_connections {
        if target_str.is_empty() {
            continue;
        }
        let (src_prim, src_attr_opt) = split_connection_target(target_str);
        let Some(src_attr) = src_attr_opt else {
            continue;
        };

        // Ensure the MaterialOutputs socket exists.
        let Some(&mo_idx) = node_id_map.get(&outputs_id) else {
            continue;
        };
        let dst_sock_idx = if let Some(i) = nodes[mo_idx].find_input(mat_attr) {
            i
        } else {
            nodes[mo_idx].inputs.push(Socket {
                name: strip_ns(mat_attr).to_owned(),
                full_name: mat_attr.clone(),
                kind: SocketKind::Input,
                ty: mat_type_map
                    .get(mat_attr)
                    .map_or(SocketType::Other, |t| classify_type_name(t)),
                authored_value: None,
            });
            nodes[mo_idx].inputs.len() - 1
        };

        let src_node_id = node_id_for_path(&src_prim);
        let Some(&src_idx_in_nodes) = node_id_map.get(&src_node_id) else {
            continue;
        };

        // Ensure the source output socket exists.
        let src_sock_idx = if let Some(i) = nodes[src_idx_in_nodes].find_output(&src_attr) {
            i
        } else {
            nodes[src_idx_in_nodes].outputs.push(Socket {
                name: strip_ns(&src_attr).to_owned(),
                full_name: src_attr,
                kind: SocketKind::Output,
                ty: SocketType::Other,
                authored_value: None,
            });
            nodes[src_idx_in_nodes].outputs.len() - 1
        };

        connections.push(Connection {
            from: (src_node_id, src_sock_idx),
            to: (outputs_id, dst_sock_idx),
        });
    }

    // ── 8. Clear authored_value on connected shader input sockets ─────────
    for conn in &connections {
        let (to_id, to_idx) = conn.to;
        let Some(&to_node_idx) = node_id_map.get(&to_id) else {
            continue;
        };
        if nodes[to_node_idx].role != NodeRole::Standard {
            continue;
        }
        if let Some(sock) = nodes[to_node_idx].inputs.get_mut(to_idx) {
            sock.authored_value = None;
        }
    }

    Graph {
        graph_key: material_path.clone(),
        nodes,
        connections,
    }
}
