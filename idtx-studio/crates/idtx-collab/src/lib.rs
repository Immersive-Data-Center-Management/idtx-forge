//! Remote collaboration sync for IDTX Studio.
//!
//! Connects a session to a collaboration server and keeps the local stage in
//! step with it: [`session`] opens or joins a server-hosted stage, [`client`]
//! owns the socket connection and worker, [`proto`] is the protobuf wire
//! protocol, and [`conv`] converts between wire and local types (including the
//! matrix conventions). [`journal`] and [`coalesce`] batch and de-duplicate
//! outbound edits to bound socket traffic during interactive drags.

pub mod client;
pub mod coalesce;
pub mod conv;
pub mod error;
pub mod journal;
pub mod proto;
pub mod session;
mod transport;
