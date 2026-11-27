//! In-memory uncommitted operation evidence, retained across connections.
use std::collections::BTreeMap;
use crate::proto::{BaseMessage, base_message};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Pending,
    Accepted { server_seq: u64 },
    Rejected { reason: String },
    /// The server may have applied this request. Never automatically resend it.
    Indeterminate,
}

#[derive(Debug, Clone)]
pub struct Operation {
    pub request: BaseMessage,
    pub outcome: Outcome,
}

#[derive(Debug, Default)]
pub(crate) struct Journal {
    pub operations: BTreeMap<u64, Operation>,
    pub next_id: u64,
}

impl Journal {
    pub fn allocate_id(&mut self) -> u64 {
        self.next_id = self.next_id.checked_add(1).expect("request id exhausted");
        self.next_id
    }

    pub fn record(&mut self, request: &BaseMessage) {
        self.next_id = self.next_id.max(request.request_id);
        self.operations.insert(request.request_id, Operation { request: request.clone(), outcome: Outcome::Pending });
    }

    pub fn acknowledge(&mut self, message: &BaseMessage) {
        let Some(base_message::Payload::Ack(ack)) = &message.payload else { return };
        if let Some(operation) = self.operations.get_mut(&message.request_id)
            && operation.outcome == Outcome::Pending {
            operation.outcome = if ack.ok { Outcome::Accepted { server_seq: message.server_seq } }
                else { Outcome::Rejected { reason: ack.error.clone() } };
        }
    }

    pub fn disconnect(&mut self) {
        for operation in self.operations.values_mut() {
            if operation.outcome == Outcome::Pending { operation.outcome = Outcome::Indeterminate; }
        }
    }
}
