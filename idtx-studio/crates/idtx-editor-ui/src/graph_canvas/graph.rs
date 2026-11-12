//! View-model types for the shared graph canvas (material + compute).
//!
//! Pure data — no egui, no rendering, no USD handles. Built once from USD
//! inspection data by a graph-specific `extract` step and cached inside
//! [`crate::graph_canvas::state::GraphCanvasState`] for the paint code to read.
//!
//! Colours here are stored as `[u8; 3]` (not `egui::Color32`) so this module
//! stays egui-free and the view-model keeps `PartialEq`/`Eq`.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// Stable identifier for a node inside a [`Graph`].
pub type NodeId = u64;

/// Compute a stable [`NodeId`] from a prim's absolute SDF path.
#[must_use]
pub fn node_id_for_path(path: &str) -> NodeId {
    let mut h = DefaultHasher::new();
    path.hash(&mut h);
    h.finish()
}

/// Compute a stable [`NodeId`] for an aggregate (synthetic) node — one not
/// backed by a single prim, identified by `(graph_key, side_tag)`.
#[must_use]
pub fn aggregate_node_id(graph_key: &str, side_tag: &str) -> NodeId {
    let mut h = DefaultHasher::new();
    graph_key.hash(&mut h);
    "::".hash(&mut h);
    side_tag.hash(&mut h);
    h.finish()
}

/// Whether a socket is drawn on the left (`Input`) or right (`Output`) side of a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SocketKind {
    Input,
    Output,
}

/// Type buckets for socket colour-coding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SocketType {
    /// `string`, `token`.
    String,
    /// `float`, `double`, `half`, `int`, `bool` (scalar).
    Float,
    /// `float3`, `double3`, `color3f`, `color3d`, `normal3f`, `point3f`, `vector3f`.
    Float3OrColor,
    /// `asset`, `SdfPath`.
    Path,
    /// Anything else.
    Other,
}

/// A single socket on a node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Socket {
    /// Display name — the attribute name with the leading namespace stripped.
    pub name: String,
    /// Full attribute name including namespace.
    pub full_name: String,
    /// Side of the node this socket renders on.
    pub kind: SocketKind,
    /// Colour bucket for the socket dot / wire tint.
    pub ty: SocketType,
    /// Formatted authored default value, if any.
    pub authored_value: Option<String>,
}

/// Presentational role of a node — drives width, header height, socket side
/// and socket labels in the paint pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeRole {
    /// A regular node: full width, inputs on the left, outputs on the right.
    /// (Material shader prims; compute/datasource prims.)
    Standard,
    /// A narrow left-side aggregate exposing an interface; inputs on the left,
    /// outputs on the right. (Material `inputs:*` aggregate.)
    SourceAggregate,
    /// A narrow right-side sink; all sockets on the left edge. (Material
    /// `outputs:*` aggregate; a compute consumer prim.)
    SinkAggregate,
}

/// Layout hint pinning a node to the first / last column, overriding the
/// longest-path sweep. `None` lets the sweep decide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnPin {
    None,
    First,
    Last,
}

/// A node in a graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    pub id: NodeId,
    pub prim_path: String,
    pub display_name: String,
    pub role: NodeRole,
    pub pin: ColumnPin,
    /// Header tint override as `[r, g, b]`. `None` → the palette accent tint.
    pub accent_rgb: Option<[u8; 3]>,
    /// Secondary line / tooltip content (material `info:id`, compute type name).
    pub subtitle: Option<String>,
    pub inputs: Vec<Socket>,
    pub outputs: Vec<Socket>,
}

impl Node {
    #[must_use]
    pub fn find_input(&self, full_name: &str) -> Option<usize> {
        self.inputs.iter().position(|s| s.full_name == full_name)
    }

    #[must_use]
    pub fn find_output(&self, full_name: &str) -> Option<usize> {
        self.outputs.iter().position(|s| s.full_name == full_name)
    }
}

/// An authored connection between two sockets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connection {
    pub from: (NodeId, usize),
    pub to: (NodeId, usize),
}

/// A fully-extracted graph ready to lay out and paint.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Graph {
    /// Stable identity of the graph (resolved material path, or compute root
    /// prim path) — used as the cache-invalidation key.
    pub graph_key: String,
    pub nodes: Vec<Node>,
    pub connections: Vec<Connection>,
}

impl Graph {
    #[must_use]
    pub fn node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }

    #[must_use]
    pub fn node_by_path(&self, path: &str) -> Option<&Node> {
        self.nodes
            .iter()
            .find(|n| n.prim_path == path && n.role == NodeRole::Standard)
            .or_else(|| self.nodes.iter().find(|n| n.prim_path == path))
    }

    /// First node pinned to the given column, if any.
    #[must_use]
    pub fn first_pinned(&self, pin: ColumnPin) -> Option<&Node> {
        self.nodes.iter().find(|n| n.pin == pin)
    }
}

// ── Shared extraction helpers (graph-kind agnostic) ───────────────────────────

/// Classify a USD type-name string into a [`SocketType`] bucket.
#[must_use]
pub fn classify_type_name(type_name: &str) -> SocketType {
    let lower = type_name.to_ascii_lowercase();
    match lower.as_str() {
        "string" | "token" => SocketType::String,
        "float" | "double" | "half" | "int" | "bool" => SocketType::Float,
        "color3f" | "float3" | "double3" | "normal3f" | "point3f" | "vector3f" | "color3d"
        | "vector3d" | "point3d" | "normal3d" => SocketType::Float3OrColor,
        "asset" | "sdfpath" => SocketType::Path,
        _ => SocketType::Other,
    }
}

/// Split a USD connection target string of the form
/// `</path/to/Shader.outputs:rgb>` into `(prim_path, Option<attr_name>)`.
#[must_use]
pub fn split_connection_target(target: &str) -> (String, Option<String>) {
    let stripped = target
        .trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .trim();
    if let Some(dot) = stripped.rfind('.') {
        let (prim_path, tail) = stripped.split_at(dot);
        let full = tail.trim_start_matches('.').to_owned();
        (prim_path.to_owned(), (!full.is_empty()).then_some(full))
    } else {
        (stripped.to_owned(), None)
    }
}

/// Strip a leading `inputs:` / `outputs:` namespace from an attribute name.
#[must_use]
pub fn strip_ns(name: &str) -> &str {
    if let Some(rest) = name.strip_prefix("inputs:") {
        return rest;
    }
    if let Some(rest) = name.strip_prefix("outputs:") {
        return rest;
    }
    name
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_type_name_scalars() {
        assert_eq!(classify_type_name("float"), SocketType::Float);
        assert_eq!(classify_type_name("int"), SocketType::Float);
        assert_eq!(classify_type_name("bool"), SocketType::Float);
        assert_eq!(classify_type_name("half"), SocketType::Float);
        assert_eq!(classify_type_name("double"), SocketType::Float);
    }

    #[test]
    fn classify_type_name_color_family() {
        assert_eq!(classify_type_name("color3f"), SocketType::Float3OrColor);
        assert_eq!(classify_type_name("normal3f"), SocketType::Float3OrColor);
        assert_eq!(classify_type_name("vector3f"), SocketType::Float3OrColor);
    }

    #[test]
    fn classify_type_name_asset() {
        assert_eq!(classify_type_name("asset"), SocketType::Path);
    }

    #[test]
    fn split_connection_target_parses_forms() {
        assert_eq!(
            split_connection_target("</World/M/Tex.outputs:rgb>"),
            ("/World/M/Tex".to_owned(), Some("outputs:rgb".to_owned())),
        );
        assert_eq!(
            split_connection_target("/World/M/Tex"),
            ("/World/M/Tex".to_owned(), None),
        );
    }

    #[test]
    fn strip_ns_removes_known_prefixes() {
        assert_eq!(strip_ns("inputs:jsonData"), "jsonData");
        assert_eq!(strip_ns("outputs:result"), "result");
        assert_eq!(strip_ns("size"), "size");
    }
}
