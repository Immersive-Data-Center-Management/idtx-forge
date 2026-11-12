use std::collections::HashMap;

/// A coalesced outbound transform edit: either a full baked local matrix or an
/// exact decomposed TRS.
///
/// `Separate` is preferred for prims whose authored xform is a pure
/// translate/`rotateXYZ`/scale stack: it lets the server store the Euler angles
/// and per-axis scale verbatim instead of decomposing a baked matrix. A
/// matrix→TRS decompose is ambiguous for a non-uniform scale under a ±90°
/// rotation and swaps the Y/Z scale axes on reopen — sending the authored TRS
/// directly avoids any decompose. See `StudioSession::flush_outbound_collab`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OutboundXform {
    /// Flat row-major USD local matrix (row-vector convention).
    Matrix([f64; 16]),
    /// Exact authored TRS. Rotation is Euler **degrees**, XYZ order.
    Separate {
        translation: [f64; 3],
        rotation_deg: [f64; 3],
        scale: [f64; 3],
    },
}

/// Transform and the applied server revision against which it was authored.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AuthoredXform {
    pub transform: OutboundXform,
    pub base_seq: u64,
}

/// Last-writer-wins coalescing buffer: stores the most recent transform edit
/// per prim path. Designed to be drained once per frame by the send loop,
/// bounding socket traffic during a drag operation.
///
/// Not thread-safe on its own; wrap in a `Mutex` for cross-thread use.
#[must_use]
#[derive(Debug, Default)]
pub struct CoalesceQueue {
    pending: HashMap<String, AuthoredXform>,
}

impl CoalesceQueue {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record (or overwrite) the latest transform edit for `prim_path`. A later
    /// edit of either variant supersedes an earlier one (last-writer-wins).
    pub fn push(&mut self, prim_path: String, xform: OutboundXform, base_seq: u64) {
        self.pending.insert(prim_path, AuthoredXform { transform: xform, base_seq });
    }

    pub(crate) fn restore_unsent(&mut self, edits: impl Iterator<Item = (String, AuthoredXform)>) {
        for (path, edit) in edits { self.pending.entry(path).or_insert(edit); }
    }

    /// Drain all pending edits. After this call `is_empty()` returns true.
    #[must_use]
    pub fn flush(&mut self) -> Vec<(String, AuthoredXform)> {
        let mut edits: Vec<_> = self.pending.drain().collect();
        edits.sort_by(|a, b| a.0.cmp(&b.0));
        edits
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.pending.len()
    }
}

#[cfg(test)]
mod tests {
    use super::{CoalesceQueue, OutboundXform};

    fn mat(v: f64) -> OutboundXform {
        OutboundXform::Matrix([v; 16])
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn same_prim_last_writer_wins() {
        let mut q = CoalesceQueue::new();
        q.push("/World/Cube".into(), mat(1.0), 7);
        q.push("/World/Cube".into(), mat(2.0), 8);
        let flushed = q.flush();
        assert_eq!(flushed.len(), 1, "expected one entry after coalescing");
        assert_eq!(flushed[0].0, "/World/Cube");
        assert_eq!(flushed[0].1.transform, mat(2.0), "expected last value to win");
        assert_eq!(flushed[0].1.base_seq, 8, "revision belongs to the winning edit");
    }

    #[test]
    fn distinct_prims_each_emitted() {
        let mut q = CoalesceQueue::new();
        q.push("/World/A".into(), mat(1.0), 0);
        q.push("/World/B".into(), mat(2.0), 0);
        let mut flushed = q.flush();
        assert_eq!(flushed.len(), 2);
        flushed.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(flushed[0].0, "/World/A");
        assert_eq!(flushed[1].0, "/World/B");
    }

    #[test]
    fn empty_after_flush() {
        let mut q = CoalesceQueue::new();
        q.push("/World/Cube".into(), mat(0.0), 0);
        assert!(!q.is_empty());
        let _ = q.flush();
        assert!(q.is_empty());
        assert_eq!(q.len(), 0);
    }

    #[test]
    fn flush_empty_queue_returns_empty_vec() {
        let mut q = CoalesceQueue::new();
        let flushed = q.flush();
        assert!(flushed.is_empty());
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn multiple_overwrites_same_prim_last_value() {
        let mut q = CoalesceQueue::new();
        for i in 0..10_u32 {
            q.push("/Prim".into(), mat(f64::from(i)), 0);
        }
        let flushed = q.flush();
        assert_eq!(flushed.len(), 1);
        assert_eq!(flushed[0].1.transform, mat(9.0));
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn separate_variant_coalesces_and_supersedes_matrix() {
        let mut q = CoalesceQueue::new();
        // A matrix edit then a later Separate edit for the same prim: the
        // Separate (exact TRS) wins — the outbound path upgrades to the
        // decompose-free representation as soon as it has the authored ops.
        q.push("/World/Plane".into(), mat(7.0), 7);
        let sep = OutboundXform::Separate {
            translation: [0.0, 0.0, 0.0],
            rotation_deg: [90.0, 0.0, 0.0],
            scale: [2.0, 3.0, 1.0],
        };
        q.push("/World/Plane".into(), sep, 8);
        let flushed = q.flush();
        assert_eq!(flushed.len(), 1);
        assert_eq!(
            flushed[0].1.transform, sep,
            "the later Separate edit must supersede the earlier matrix"
        );
    }
}
