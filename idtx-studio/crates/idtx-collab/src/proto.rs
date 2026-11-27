// Wire-protocol types for idtxcore collaboration.
// Mirrors the authoritative server schemas:
//   idtx-core/shared/proto_messages/{base,session,transform,prim,attribute}.proto
// (proto3, package idtxcore).
//
// NOT auto-generated — committed to avoid a protoc / prost-build dependency.
// Keep in sync with the .proto sources manually if the wire protocol changes.
//
// Ordering contract (from base.proto):
//   - One `BaseMessage` per binary WebSocket frame.
//   - `server_seq` (field 9) is a per-session version counter. Server messages that
//     reflect stage state (transform/attribute broadcasts, Acks, SnapshotComplete) carry the
//     `server_seq` of the state they describe. Snapshot frames can share a
//     sequence, including zero, and must all apply in arrival order. The client
//     tracks the highest value it has applied and echoes it as
//     the *base* on every outbound `TransformUpdate` (via `BaseMessage.server_seq`).
//   - `request_id` (field 8) is a client-chosen per-update counter echoed back on the
//     matching transform `Ack` or `SetPrimAttributesAck`. Attribute requests
//     have no stale-base check; successes are confirmed by their ordered echo.
//   - Connect sequence: Handshake → snapshot create/active/transform/attribute
//     broadcasts → SnapshotComplete. The client MUST NOT send before the
//     complete snapshot has been applied.

#![allow(clippy::derive_partial_eq_without_eq)] // f64 fields prevent Eq

// ─── transform.proto ─────────────────────────────────────────────────────────

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct Vector3 {
    #[prost(double, tag = "1")]
    pub x: f64,
    #[prost(double, tag = "2")]
    pub y: f64,
    #[prost(double, tag = "3")]
    pub z: f64,
}

/// Row-major 4×4 double matrix with translation on the bottom row (m30/m31/m32).
/// Field numbering is row-major: field `(row*4 + col + 1)`.
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct Matrix4dTransform {
    #[prost(double, tag = "1")]
    pub m00: f64,
    #[prost(double, tag = "2")]
    pub m01: f64,
    #[prost(double, tag = "3")]
    pub m02: f64,
    #[prost(double, tag = "4")]
    pub m03: f64,
    #[prost(double, tag = "5")]
    pub m10: f64,
    #[prost(double, tag = "6")]
    pub m11: f64,
    #[prost(double, tag = "7")]
    pub m12: f64,
    #[prost(double, tag = "8")]
    pub m13: f64,
    #[prost(double, tag = "9")]
    pub m20: f64,
    #[prost(double, tag = "10")]
    pub m21: f64,
    #[prost(double, tag = "11")]
    pub m22: f64,
    #[prost(double, tag = "12")]
    pub m23: f64,
    #[prost(double, tag = "13")]
    pub m30: f64,
    #[prost(double, tag = "14")]
    pub m31: f64,
    #[prost(double, tag = "15")]
    pub m32: f64,
    #[prost(double, tag = "16")]
    pub m33: f64,
}

/// Decomposed transform. Rotation is Euler degrees in XYZ order.
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct SeparateTransform {
    #[prost(message, optional, tag = "1")]
    pub translation: Option<Vector3>,
    #[prost(message, optional, tag = "2")]
    pub rotation: Option<Vector3>,
    #[prost(message, optional, tag = "3")]
    pub scale: Option<Vector3>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct TransformUpdate {
    #[prost(string, tag = "1")]
    pub session_id: String,
    #[prost(string, tag = "2")]
    pub usd_file: String,
    #[prost(string, tag = "3")]
    pub prim_path: String,
    #[prost(oneof = "transform_update::Transform", tags = "4, 5")]
    pub transform: Option<transform_update::Transform>,
    #[prost(int64, tag = "7")]
    pub timestamp: i64,
}

pub mod transform_update {
    // NOTE: `Seperate` spelling intentionally matches the wire protocol (typo from idtx-flow).
    #[derive(Clone, PartialEq, ::prost::Oneof)]
    pub enum Transform {
        #[prost(message, tag = "4")]
        Seperate(super::SeparateTransform),
        #[prost(message, tag = "5")]
        Matrix(super::Matrix4dTransform),
    }
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct TransformBroadcast {
    #[prost(string, tag = "1")]
    pub client_id: String,
    #[prost(message, optional, tag = "2")]
    pub update: Option<TransformUpdate>,
}

// ─── session.proto ────────────────────────────────────────────────────────────

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct Ack {
    #[prost(string, tag = "1")]
    pub ref_id: String,
    #[prost(bool, tag = "2")]
    pub ok: bool,
    #[prost(string, tag = "3")]
    pub error: String,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct ProtocolError {
    #[prost(string, tag = "1")]
    pub code: String,
    #[prost(string, tag = "2")]
    pub message: String,
}

// ─── prim.proto ─────────────────────────────────────────────────────────────

/// Built-in USD prim types a client may ask the server to create. Wire values
/// are stable (append, never renumber). `Unspecified` (0) is the proto3 default
/// and is rejected by the server. Mirrors `idtxcore.PrimType`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, ::prost::Enumeration)]
#[repr(i32)]
pub enum PrimType {
    Unspecified = 0,
    Cube = 1,
    Sphere = 2,
    Cone = 3,
    Cylinder = 4,
    Capsule = 5,
    Xform = 6,
    Plane = 7,
    /// A typeless prim holding a reference/payload to another layer/prim. The
    /// arc kind is carried by `PrimRefType` and the target by `reference_path`.
    /// Handled explicitly (not via `usd_type_name`), since it maps to a typeless
    /// `def` plus a composition arc rather than a built-in schema type.
    Ref = 8,
}

/// How a `PrimType::Ref` prim composes the layer/prim named by `reference_path`:
/// as a USD `reference` or as a (deferred-load) `payload`. Mirrors
/// `idtxcore.PrimRefType`. Wire values are stable (append, never renumber);
/// unlike `PrimType` there is no `Unspecified`, and `Reference` is the proto3
/// default (`0`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, ::prost::Enumeration)]
#[repr(i32)]
pub enum PrimRefType {
    Reference = 0,
    Payload = 1,
}

impl PrimType {
    /// Map a USD schema type name (`"Cube"`, `"Xform"`, …) to its wire enum, or
    /// `None` when the type is not a server-creatable built-in. Used to decide
    /// whether a create command can be routed to the collaboration server.
    #[must_use]
    pub fn from_usd_type_name(name: &str) -> Option<Self> {
        match name {
            "Cube" => Some(Self::Cube),
            "Sphere" => Some(Self::Sphere),
            "Cone" => Some(Self::Cone),
            "Cylinder" => Some(Self::Cylinder),
            "Capsule" => Some(Self::Capsule),
            "Xform" => Some(Self::Xform),
            "Plane" => Some(Self::Plane),
            _ => None,
        }
    }

    /// The USD schema type name for this prim type, or `None` for `Unspecified`.
    #[must_use]
    pub fn usd_type_name(self) -> Option<&'static str> {
        match self {
            Self::Cube => Some("Cube"),
            Self::Sphere => Some("Sphere"),
            Self::Cone => Some("Cone"),
            Self::Cylinder => Some("Cylinder"),
            Self::Capsule => Some("Capsule"),
            Self::Xform => Some("Xform"),
            Self::Plane => Some("Plane"),
            // `Ref` is a typeless reference/payload prim, handled explicitly by
            // the inbound path; it has no built-in schema type name.
            Self::Unspecified | Self::Ref => None,
        }
    }
}

/// Client → server: ask the server to author a new built-in primitive of `type`
/// as a child of `parent_path`. The server picks a unique name under the parent,
/// authors the prim with its schema defaults, and reports the result in a
/// [`CreatePrimBroadcast`]. Correlation is via `BaseMessage.request_id`.
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct CreatePrimRequest {
    #[prost(string, tag = "1")]
    pub session_id: String,
    #[prost(string, tag = "2")]
    pub parent_path: String,
    #[prost(enumeration = "PrimType", tag = "3")]
    pub r#type: i32,
    /// For `PrimType::Ref`: how `reference_path` is composed (reference vs
    /// payload). Unset for built-in gprim creates.
    #[prost(enumeration = "PrimRefType", optional, tag = "4")]
    pub ref_type: Option<i32>,
    /// For `PrimType::Ref`: the layer/prim path the new typeless prim should
    /// reference or payload. Unset for built-in gprim creates.
    #[prost(string, optional, tag = "5")]
    pub reference_path: Option<String>,
    /// Optional **local** transform of the new prim, relative to its parent,
    /// authored verbatim by the server (no world→local conversion). idtx-studio
    /// never sets this on outbound creates (the prim inherits its parent's
    /// transform), but the field exists so a future client/server may request one.
    #[prost(oneof = "create_prim_request::WorldTransform", tags = "6, 7")]
    pub world_transform: Option<create_prim_request::WorldTransform>,
}

pub mod create_prim_request {
    // NOTE: `Seperate` spelling intentionally matches the wire protocol (typo
    // from idtx-flow), consistent with `transform_update::Transform`.
    #[derive(Clone, PartialEq, ::prost::Oneof)]
    pub enum WorldTransform {
        #[prost(message, tag = "6")]
        Seperate(super::SeparateTransform),
        #[prost(message, tag = "7")]
        Matrix(super::Matrix4dTransform),
    }
}

/// Server → all clients (including the originator) and replayed in the join
/// snapshot for late joiners. Describes a primitive the server created: its
/// parent, the server-assigned `prim_name`, the `type`, and the default
/// attribute values that were authored. Only the attribute fields that apply to
/// `type` are set (proto3 optional has-field semantics): Cube → size; Sphere →
/// radius; Cone/Cylinder/Capsule → height, radius, axis; Plane → width, length,
/// axis; Xform → none.
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct CreatePrimBroadcast {
    #[prost(string, tag = "1")]
    pub parent_path: String,
    #[prost(string, tag = "2")]
    pub prim_name: String,
    #[prost(enumeration = "PrimType", tag = "3")]
    pub r#type: i32,
    /// Relevant for `Cube` only.
    #[prost(double, optional, tag = "4")]
    pub size: Option<f64>,
    /// Relevant for `Cylinder`, `Cone` and `Capsule` only.
    #[prost(double, optional, tag = "5")]
    pub height: Option<f64>,
    /// Relevant for `Cylinder`, `Cone`, `Capsule` and `Sphere` only.
    #[prost(double, optional, tag = "6")]
    pub radius: Option<f64>,
    /// Relevant for `Cylinder`, `Cone`, `Capsule` and `Plane` only.
    #[prost(string, optional, tag = "7")]
    pub axis: Option<String>,
    /// Relevant for `Plane` only.
    #[prost(double, optional, tag = "8")]
    pub width: Option<f64>,
    /// Relevant for `Plane` only.
    #[prost(double, optional, tag = "9")]
    pub length: Option<f64>,
    /// For `PrimType::Ref`: the composition arc kind the server authored.
    #[prost(enumeration = "PrimRefType", optional, tag = "10")]
    pub ref_type: Option<i32>,
    /// For `PrimType::Ref`: the layer/prim path that was referenced/payloaded.
    #[prost(string, optional, tag = "11")]
    pub reference_path: Option<String>,
    /// Local transform the server authored for the new prim, relative to its
    /// parent, if any. When unset the prim inherits its parent's transform
    /// (identity local). Authored verbatim by the client (no world→local
    /// conversion) under the current contract.
    #[prost(oneof = "create_prim_broadcast::WorldTransform", tags = "12, 13")]
    pub world_transform: Option<create_prim_broadcast::WorldTransform>,
}

pub mod create_prim_broadcast {
    // NOTE: `Seperate` spelling intentionally matches the wire protocol (typo
    // from idtx-flow), consistent with `transform_update::Transform`.
    #[derive(Clone, PartialEq, ::prost::Oneof)]
    pub enum WorldTransform {
        #[prost(message, tag = "12")]
        Seperate(super::SeparateTransform),
        #[prost(message, tag = "13")]
        Matrix(super::Matrix4dTransform),
    }
}

/// Client → server: ask the server to change the active state of an existing
/// prim. `active == false` is a *soft delete*: the server authors `active=false`
/// into the root layer, which prunes the prim and its subtree from the
/// composed stage (and so from imaging); `active == true` undoes that. USD
/// composition is strictly additive at the prim level, so deactivation is the
/// only composition-safe way to "remove" a prim a weaker layer defines. The
/// target must be an existing prim defined in the stage's root layer; prims
/// introduced across a reference/payload or defined only in a sublayer are
/// rejected. Correlation is via `BaseMessage.request_id`. Mirrors
/// `idtxcore.SetPrimActiveRequest`.
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct SetPrimActiveRequest {
    #[prost(string, tag = "1")]
    pub session_id: String,
    #[prost(string, tag = "2")]
    pub prim_path: String,
    #[prost(bool, tag = "3")]
    pub active: bool,
}

/// Server → all clients (including the originator) and replayed in the join
/// snapshot for late joiners. Announces that a prim's active state changed.
/// Clients mirror the flag on the prim in their own stage (authoring `active`,
/// not hard-removing), so delete and undo-delete are the same one-prim message
/// and neither needs a subtree snapshot: USD prunes/unprunes the subtree.
/// Mirrors `idtxcore.SetPrimActiveBroadcast`.
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct SetPrimActiveBroadcast {
    #[prost(string, tag = "1")]
    pub prim_path: String,
    #[prost(bool, tag = "2")]
    pub active: bool,
}

// ─── base.proto ───────────────────────────────────────────────────────────────

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct Handshake {
    #[prost(string, tag = "1")]
    pub session_id: String,
    #[prost(string, tag = "2")]
    pub session_layer: String,
    #[prost(string, tag = "3")]
    pub usd_path: String,
    #[prost(string, tag = "4")]
    pub usd_uri: String,
}

/// Terminal marker of a snapshot: the server sends one `TransformBroadcast`
/// per prim with authored session-layer overrides (plus the creation and
/// `active=false` frames of prims the session created or deleted), then a
/// single `SnapshotComplete` so the client knows it has the full current
/// state. A snapshot is sent when a client joins, and when it asks for one
/// with [`SnapshotRequest`]; `BaseMessage.request_id` of the `SnapshotComplete`
/// then echoes the request's, and is `0` after a join.
#[derive(Clone, Copy, PartialEq, Eq, ::prost::Message)]
pub struct SnapshotComplete {}

/// Client → server: "send me a snapshot of the session's current state now",
/// for example after rebuilding the stage on [`LayerReloaded`]. The client
/// sets `BaseMessage.request_id` so it can match the terminal `SnapshotComplete`
/// that ends the requested snapshot. The snapshot is taken in the session's
/// command order, like any command; a request the session cannot accept is
/// answered with a not-ok [`Ack`] instead. Mirrors `idtxcore.SnapshotRequest`.
#[derive(Clone, Copy, PartialEq, Eq, ::prost::Message)]
pub struct SnapshotRequest {}

/// Server → client: a file the session's stage uses (its root file, or one of
/// its sublayers, references or payloads) changed on the server — through a
/// commit of any session or an upload that replaced it — and the server has
/// reloaded it. The client re-fetches that one file (evicting any cached copy
/// of `usd_uri`) and rebuilds what it shows from it, then sends a
/// [`SnapshotRequest`] because a rebuilt stage no longer holds the session's
/// edits. Stage frames (transform / creation / active broadcasts) received
/// until that snapshot can be ignored: the file and the snapshot supersede
/// them. `server_seq` is the session's version after the reload. Mirrors
/// `idtxcore.LayerReloaded`.
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct LayerReloaded {
    /// Uploads-relative canonical path of the reloaded file, as in
    /// `Handshake.usd_path` (decoded, without `./` or repeated `/`).
    #[prost(string, tag = "1")]
    pub usd_path: String,
    /// Server-relative download URI of that file, built like
    /// `Handshake.usd_uri` (`/api/v1/download/<percent-encoded path>`).
    #[prost(string, tag = "2")]
    pub usd_uri: String,
}

/// Typed default value. Array-ness is independent of lane length.
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct AttributeValue {
    #[prost(double, repeated, tag = "1")]
    pub doubles: Vec<f64>,
    #[prost(sint64, repeated, tag = "2")]
    pub ints: Vec<i64>,
    #[prost(uint64, repeated, tag = "3")]
    pub uints: Vec<u64>,
    #[prost(bool, repeated, tag = "4")]
    pub bools: Vec<bool>,
    #[prost(string, repeated, tag = "5")]
    pub strings: Vec<String>,
    #[prost(bool, tag = "6")]
    pub is_array: bool,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct PrimAttribute {
    #[prost(string, tag = "1")]
    pub name: String,
    /// Scalar USD token, including its role (e.g. color3f).
    #[prost(string, tag = "2")]
    pub type_name: String,
    #[prost(message, optional, tag = "3")]
    pub value: Option<AttributeValue>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct SetPrimAttributesRequest {
    #[prost(string, tag = "1")]
    pub session_id: String,
    #[prost(string, tag = "2")]
    pub prim_path: String,
    #[prost(message, repeated, tag = "3")]
    pub attributes: Vec<PrimAttribute>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct SetPrimAttributesBroadcast {
    #[prost(string, tag = "1")]
    pub prim_path: String,
    #[prost(message, repeated, tag = "2")]
    pub attributes: Vec<PrimAttribute>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, ::prost::Enumeration)]
#[repr(i32)]
pub enum AttributeFailureReason {
    Unspecified = 0,
    PrimNotFound = 1,
    PrimNotEditable = 2,
    InvalidName = 3,
    ReservedAttribute = 4,
    UnknownType = 5,
    UnsupportedType = 6,
    TypeMismatch = 7,
    InvalidValue = 8,
    NotAuthorable = 9,
    QueueFull = 10,
    SessionClosing = 11,
    PropertyKindMismatch = 12,
    RequestTooLarge = 13,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct AttributeError {
    #[prost(string, tag = "1")]
    pub name: String,
    #[prost(enumeration = "AttributeFailureReason", tag = "2")]
    pub reason: i32,
    #[prost(string, tag = "3")]
    pub detail: String,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct SetPrimAttributesAck {
    #[prost(string, tag = "1")]
    pub prim_path: String,
    #[prost(message, repeated, tag = "2")]
    pub failed: Vec<AttributeError>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct BaseMessage {
    #[prost(string, optional, tag = "1")]
    pub session_id: Option<String>,
    #[prost(
        oneof = "base_message::Payload",
        tags = "2, 3, 4, 5, 6, 7, 10, 11, 12, 13, 14, 15, 16, 17, 18"
    )]
    pub payload: Option<base_message::Payload>,
    /// Client-chosen per-update id, echoed back on the matching `Ack`. On a
    /// `SnapshotRequest` it is echoed on the terminal `SnapshotComplete`. `0` = unset.
    #[prost(uint64, tag = "8")]
    pub request_id: u64,
    /// On outbound updates: the base `server_seq` the update was authored against.
    /// On inbound state-reflecting messages: the `server_seq` of that state.
    #[prost(uint64, tag = "9")]
    pub server_seq: u64,
}

pub mod base_message {
    #[derive(Clone, PartialEq, ::prost::Oneof)]
    pub enum Payload {
        #[prost(message, tag = "2")]
        Handshake(super::Handshake),
        #[prost(message, tag = "3")]
        XformUpdate(super::TransformUpdate),
        #[prost(message, tag = "4")]
        XformBroadcast(super::TransformBroadcast),
        #[prost(message, tag = "5")]
        Ack(super::Ack),
        #[prost(message, tag = "6")]
        Error(super::ProtocolError),
        #[prost(message, tag = "7")]
        SnapshotComplete(super::SnapshotComplete),
        #[prost(message, tag = "10")]
        CreatePrim(super::CreatePrimRequest),
        #[prost(message, tag = "11")]
        CreatePrimBroadcast(super::CreatePrimBroadcast),
        #[prost(message, tag = "12")]
        SetPrimActive(super::SetPrimActiveRequest),
        #[prost(message, tag = "13")]
        SetPrimActiveBroadcast(super::SetPrimActiveBroadcast),
        #[prost(message, tag = "14")]
        LayerReloaded(super::LayerReloaded),
        #[prost(message, tag = "15")]
        SnapshotRequest(super::SnapshotRequest),
        #[prost(message, tag = "16")]
        SetPrimAttributes(super::SetPrimAttributesRequest),
        #[prost(message, tag = "17")]
        SetPrimAttributesBroadcast(super::SetPrimAttributesBroadcast),
        #[prost(message, tag = "18")]
        SetPrimAttributesAck(super::SetPrimAttributesAck),
    }
}

// ─── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use prost::Message as _;

    use super::{
        Ack, BaseMessage, CreatePrimBroadcast, CreatePrimRequest, Handshake, LayerReloaded,
        PrimRefType, PrimType, SetPrimActiveBroadcast, SetPrimActiveRequest, SnapshotComplete,
        SnapshotRequest, TransformUpdate, base_message, create_prim_broadcast, create_prim_request,
        transform_update,
    };
    use crate::conv::wire_from_usd;

    fn identity() -> [f64; 16] {
        [
            1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
        ]
    }

    #[test]
    fn encode_decode_xform_update() {
        let update = TransformUpdate {
            session_id: "sess1".into(),
            usd_file: String::new(),
            prim_path: "/World/Cube".into(),
            transform: Some(transform_update::Transform::Matrix(wire_from_usd(
                identity(),
            ))),
            timestamp: 12_345,
        };
        let msg = BaseMessage {
            session_id: Some("sess1".into()),
            payload: Some(base_message::Payload::XformUpdate(update)),
            request_id: 42,
            server_seq: 7,
        };

        let mut buf = Vec::new();
        msg.encode(&mut buf).unwrap();
        let decoded = BaseMessage::decode(buf.as_slice()).unwrap();

        assert_eq!(decoded.session_id, Some("sess1".into()));
        assert_eq!(decoded.request_id, 42);
        assert_eq!(decoded.server_seq, 7);
        let base_message::Payload::XformUpdate(u) = decoded.payload.unwrap() else {
            panic!("expected XformUpdate");
        };
        assert_eq!(u.prim_path, "/World/Cube");
        assert_eq!(u.timestamp, 12_345);
    }

    #[test]
    fn encode_decode_handshake() {
        let msg = BaseMessage {
            session_id: Some("sid".into()),
            payload: Some(base_message::Payload::Handshake(Handshake {
                session_id: "sid".into(),
                session_layer: "layer".into(),
                usd_path: "/path/to.usda".into(),
                usd_uri: "http://server/to.usda".into(),
            })),
            request_id: 0,
            server_seq: 0,
        };
        let mut buf = Vec::new();
        msg.encode(&mut buf).unwrap();
        let decoded = BaseMessage::decode(buf.as_slice()).unwrap();
        let base_message::Payload::Handshake(h) = decoded.payload.unwrap() else {
            panic!("expected Handshake");
        };
        assert_eq!(h.usd_path, "/path/to.usda");
        assert_eq!(h.usd_uri, "http://server/to.usda");
    }

    #[test]
    fn encode_decode_ack() {
        let msg = BaseMessage {
            session_id: None,
            payload: Some(base_message::Payload::Ack(Ack {
                ref_id: "ref1".into(),
                ok: true,
                error: String::new(),
            })),
            request_id: 99,
            server_seq: 12,
        };
        let mut buf = Vec::new();
        msg.encode(&mut buf).unwrap();
        let decoded = BaseMessage::decode(buf.as_slice()).unwrap();
        assert_eq!(decoded.request_id, 99);
        assert_eq!(decoded.server_seq, 12);
        let base_message::Payload::Ack(a) = decoded.payload.unwrap() else {
            panic!("expected Ack");
        };
        assert!(a.ok);
        assert_eq!(a.ref_id, "ref1");
    }

    #[test]
    fn encode_decode_snapshot_complete() {
        let msg = BaseMessage {
            session_id: Some("sid".into()),
            payload: Some(base_message::Payload::SnapshotComplete(SnapshotComplete {})),
            request_id: 0,
            server_seq: 55,
        };
        let mut buf = Vec::new();
        msg.encode(&mut buf).unwrap();
        let decoded = BaseMessage::decode(buf.as_slice()).unwrap();
        assert_eq!(decoded.server_seq, 55);
        assert!(matches!(
            decoded.payload,
            Some(base_message::Payload::SnapshotComplete(_))
        ));
    }

    #[test]
    fn encode_decode_layer_reloaded() {
        let msg = BaseMessage {
            session_id: Some("sess1".into()),
            payload: Some(base_message::Payload::LayerReloaded(LayerReloaded {
                usd_path: "demo/scene.usda".into(),
                usd_uri: "/api/v1/download/demo/scene.usda".into(),
            })),
            request_id: 0,
            server_seq: 42,
        };
        let mut buf = Vec::new();
        msg.encode(&mut buf).unwrap();
        let decoded = BaseMessage::decode(buf.as_slice()).unwrap();
        assert_eq!(decoded.server_seq, 42);
        let Some(base_message::Payload::LayerReloaded(lr)) = decoded.payload else {
            panic!("expected LayerReloaded");
        };
        assert_eq!(lr.usd_path, "demo/scene.usda");
        assert_eq!(lr.usd_uri, "/api/v1/download/demo/scene.usda");
    }

    #[test]
    fn encode_decode_snapshot_request() {
        // The client sends a SnapshotRequest with a request_id it will match
        // against the terminal SnapshotComplete.
        let msg = BaseMessage {
            session_id: Some("sess1".into()),
            payload: Some(base_message::Payload::SnapshotRequest(SnapshotRequest {})),
            request_id: 7,
            server_seq: 0,
        };
        let mut buf = Vec::new();
        msg.encode(&mut buf).unwrap();
        let decoded = BaseMessage::decode(buf.as_slice()).unwrap();
        assert_eq!(decoded.request_id, 7);
        assert!(matches!(
            decoded.payload,
            Some(base_message::Payload::SnapshotRequest(_))
        ));
    }

    #[test]
    fn encode_decode_create_prim_request() {
        let msg = BaseMessage {
            session_id: Some("sess1".into()),
            payload: Some(base_message::Payload::CreatePrim(CreatePrimRequest {
                session_id: "sess1".into(),
                parent_path: "/World".into(),
                r#type: PrimType::Sphere as i32,
                ref_type: None,
                reference_path: None,
                world_transform: None,
            })),
            request_id: 17,
            server_seq: 4,
        };
        let mut buf = Vec::new();
        msg.encode(&mut buf).unwrap();
        let decoded = BaseMessage::decode(buf.as_slice()).unwrap();
        assert_eq!(decoded.request_id, 17);
        let Some(base_message::Payload::CreatePrim(req)) = decoded.payload else {
            panic!("expected CreatePrim");
        };
        assert_eq!(req.parent_path, "/World");
        assert_eq!(req.r#type, PrimType::Sphere as i32);
    }

    #[test]
    fn encode_decode_create_prim_broadcast() {
        let msg = BaseMessage {
            session_id: Some("sess1".into()),
            payload: Some(base_message::Payload::CreatePrimBroadcast(CreatePrimBroadcast {
                parent_path: "/World".into(),
                prim_name: "Sphere_1".into(),
                r#type: PrimType::Sphere as i32,
                size: None,
                height: None,
                radius: Some(1.0),
                axis: None,
                width: None,
                length: None,
                ref_type: None,
                reference_path: None,
                world_transform: None,
            })),
            request_id: 0,
            server_seq: 9,
        };
        let mut buf = Vec::new();
        msg.encode(&mut buf).unwrap();
        let decoded = BaseMessage::decode(buf.as_slice()).unwrap();
        assert_eq!(decoded.server_seq, 9);
        let Some(base_message::Payload::CreatePrimBroadcast(bc)) = decoded.payload else {
            panic!("expected CreatePrimBroadcast");
        };
        assert_eq!(bc.parent_path, "/World");
        assert_eq!(bc.prim_name, "Sphere_1");
        assert_eq!(bc.r#type, PrimType::Sphere as i32);
        assert_eq!(bc.radius, Some(1.0));
        assert_eq!(bc.size, None, "unset optional shape attrs stay None");
    }

    #[test]
    fn encode_decode_set_prim_active_request() {
        let msg = BaseMessage {
            session_id: Some("sess1".into()),
            payload: Some(base_message::Payload::SetPrimActive(SetPrimActiveRequest {
                session_id: "sess1".into(),
                prim_path: "/World/Cube".into(),
                active: false,
            })),
            request_id: 23,
            server_seq: 8,
        };
        let mut buf = Vec::new();
        msg.encode(&mut buf).unwrap();
        let decoded = BaseMessage::decode(buf.as_slice()).unwrap();
        assert_eq!(decoded.request_id, 23);
        let Some(base_message::Payload::SetPrimActive(req)) = decoded.payload else {
            panic!("expected SetPrimActive");
        };
        assert_eq!(req.prim_path, "/World/Cube");
        assert!(!req.active, "a soft delete carries active=false");
    }

    #[test]
    fn encode_decode_set_prim_active_broadcast() {        let msg = BaseMessage {
            session_id: Some("sess1".into()),
            payload: Some(base_message::Payload::SetPrimActiveBroadcast(
                SetPrimActiveBroadcast {
                    prim_path: "/World/Cube".into(),
                    active: true,
                },
            )),
            request_id: 0,
            server_seq: 11,
        };
        let mut buf = Vec::new();
        msg.encode(&mut buf).unwrap();
        let decoded = BaseMessage::decode(buf.as_slice()).unwrap();
        assert_eq!(decoded.server_seq, 11);
        let Some(base_message::Payload::SetPrimActiveBroadcast(bc)) = decoded.payload else {
            panic!("expected SetPrimActiveBroadcast");
        };
        assert_eq!(bc.prim_path, "/World/Cube");
        assert!(bc.active, "an undo-delete carries active=true");
    }

    #[test]
    fn prim_type_name_round_trips() {
        for name in ["Cube", "Sphere", "Cone", "Cylinder", "Capsule", "Xform", "Plane"] {
            let t = PrimType::from_usd_type_name(name).expect("built-in type maps");
            assert_eq!(t.usd_type_name(), Some(name));
        }
        assert_eq!(PrimType::from_usd_type_name("Scope"), None);
        assert_eq!(PrimType::from_usd_type_name("Camera"), None);
        assert_eq!(PrimType::Unspecified.usd_type_name(), None);
        // `Ref` is a typeless reference/payload prim, not a schema type: it has
        // no USD type name and is handled explicitly, not via name mapping.
        assert_eq!(PrimType::Ref.usd_type_name(), None);
        assert_eq!(PrimType::from_usd_type_name("Ref"), None);
    }

    #[test]
    fn prim_type_ref_and_ref_type_wire_values_are_stable() {
        // Wire values must match idtxcore.proto exactly (append, never renumber).
        assert_eq!(PrimType::Plane as i32, 7);
        assert_eq!(PrimType::Ref as i32, 8);
        assert_eq!(PrimRefType::Reference as i32, 0);
        assert_eq!(PrimRefType::Payload as i32, 1);
        assert_eq!(PrimType::try_from(7), Ok(PrimType::Plane));
        assert_eq!(PrimType::try_from(8), Ok(PrimType::Ref));
        assert_eq!(PrimRefType::try_from(1), Ok(PrimRefType::Payload));
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn encode_decode_create_prim_broadcast_plane() {
        let msg = BaseMessage {
            session_id: Some("sess1".into()),
            payload: Some(base_message::Payload::CreatePrimBroadcast(CreatePrimBroadcast {
                parent_path: "/World".into(),
                prim_name: "Plane_1".into(),
                r#type: PrimType::Plane as i32,
                size: None,
                height: None,
                radius: None,
                axis: Some("Y".into()),
                width: Some(2.0),
                length: Some(3.0),
                ref_type: None,
                reference_path: None,
                world_transform: None,
            })),
            request_id: 0,
            server_seq: 10,
        };
        let mut buf = Vec::new();
        msg.encode(&mut buf).unwrap();
        let decoded = BaseMessage::decode(buf.as_slice()).unwrap();
        let Some(base_message::Payload::CreatePrimBroadcast(bc)) = decoded.payload else {
            panic!("expected CreatePrimBroadcast");
        };
        assert_eq!(bc.r#type, PrimType::Plane as i32);
        assert_eq!(bc.axis.as_deref(), Some("Y"));
        assert_eq!(bc.width, Some(2.0));
        assert_eq!(bc.length, Some(3.0));
        assert_eq!(bc.size, None, "unset optional shape attrs stay None");
        assert_eq!(bc.radius, None);
    }

    #[test]
    fn encode_decode_create_prim_request_with_reference() {
        let msg = BaseMessage {
            session_id: Some("sess1".into()),
            payload: Some(base_message::Payload::CreatePrim(CreatePrimRequest {
                session_id: "sess1".into(),
                parent_path: "/World".into(),
                r#type: PrimType::Ref as i32,
                ref_type: Some(PrimRefType::Payload as i32),
                reference_path: Some("../assets/car.usda".into()),
                world_transform: None,
            })),
            request_id: 31,
            server_seq: 2,
        };
        let mut buf = Vec::new();
        msg.encode(&mut buf).unwrap();
        let decoded = BaseMessage::decode(buf.as_slice()).unwrap();
        let Some(base_message::Payload::CreatePrim(req)) = decoded.payload else {
            panic!("expected CreatePrim");
        };
        assert_eq!(req.r#type, PrimType::Ref as i32);
        assert_eq!(req.ref_type, Some(PrimRefType::Payload as i32));
        assert_eq!(req.reference_path.as_deref(), Some("../assets/car.usda"));
        assert!(
            req.world_transform.is_none(),
            "idtx-studio never sets a transform on outbound creates"
        );
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn encode_decode_create_prim_broadcast_with_reference_and_transform() {
        let msg = BaseMessage {
            session_id: Some("sess1".into()),
            payload: Some(base_message::Payload::CreatePrimBroadcast(CreatePrimBroadcast {
                parent_path: "/World".into(),
                prim_name: "car".into(),
                r#type: PrimType::Ref as i32,
                size: None,
                height: None,
                radius: None,
                axis: None,
                width: None,
                length: None,
                ref_type: Some(PrimRefType::Payload as i32),
                reference_path: Some("../assets/car.usda".into()),
                world_transform: Some(create_prim_broadcast::WorldTransform::Matrix(
                    wire_from_usd(identity()),
                )),
            })),
            request_id: 0,
            server_seq: 9,
        };
        let mut buf = Vec::new();
        msg.encode(&mut buf).unwrap();
        let decoded = BaseMessage::decode(buf.as_slice()).unwrap();
        let Some(base_message::Payload::CreatePrimBroadcast(bc)) = decoded.payload else {
            panic!("expected CreatePrimBroadcast");
        };
        assert_eq!(bc.r#type, PrimType::Ref as i32);
        assert_eq!(bc.ref_type, Some(PrimRefType::Payload as i32));
        assert_eq!(bc.reference_path.as_deref(), Some("../assets/car.usda"));
        assert!(
            matches!(
                bc.world_transform,
                Some(create_prim_broadcast::WorldTransform::Matrix(_))
            ),
            "a matrix world transform must survive the round-trip"
        );
    }

    #[test]
    fn create_prim_request_world_transform_oneof_round_trips() {
        let msg = BaseMessage {
            session_id: Some("sess1".into()),
            payload: Some(base_message::Payload::CreatePrim(CreatePrimRequest {
                session_id: "sess1".into(),
                parent_path: "/World".into(),
                r#type: PrimType::Cube as i32,
                ref_type: None,
                reference_path: None,
                world_transform: Some(create_prim_request::WorldTransform::Matrix(wire_from_usd(
                    identity(),
                ))),
            })),
            request_id: 5,
            server_seq: 1,
        };
        let mut buf = Vec::new();
        msg.encode(&mut buf).unwrap();
        let decoded = BaseMessage::decode(buf.as_slice()).unwrap();
        let Some(base_message::Payload::CreatePrim(req)) = decoded.payload else {
            panic!("expected CreatePrim");
        };
        assert!(matches!(
            req.world_transform,
            Some(create_prim_request::WorldTransform::Matrix(_))
        ));
    }
}
