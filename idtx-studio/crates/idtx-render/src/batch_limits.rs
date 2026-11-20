//! Checked sizing for draw geometry. These buffers are vertex/index bindings,
//! not storage bindings: `max_storage_buffer_binding_size` does not constrain
//! them. Keep limits tied to actual buffer usage rather than adapter maxima.

/// Per-buffer geometry budgets, further capped by the renderer's device limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DrawBatchLimits {
    /// Main/skin vertex data and per-instance transforms.
    pub max_vertex_bytes: u64,
    /// Triangle index data.
    pub max_index_bytes: u64,
}

impl DrawBatchLimits {
    #[must_use]
    pub fn for_device(limits: &wgpu::Limits) -> Self {
        Self { max_vertex_bytes: limits.max_buffer_size, max_index_bytes: limits.max_buffer_size }
    }

    #[must_use]
    pub fn clamped(self, device: &wgpu::Limits) -> Self {
        Self {
            max_vertex_bytes: self.max_vertex_bytes.min(device.max_buffer_size),
            max_index_bytes: self.max_index_bytes.min(device.max_buffer_size),
        }
    }

    pub(crate) fn check(self, counts: GeometryCounts) -> Result<(), GeometryError> {
        checked_bytes(counts.vertices, std::mem::size_of::<crate::vertex::Vertex>(), self.max_vertex_bytes, "vertex")?;
        checked_bytes(counts.indices, std::mem::size_of::<u32>(), self.max_index_bytes, "index")?;
        Ok(())
    }
}

/// A geometry allocation or index cannot be represented within the draw budget.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GeometryError {
    #[error("{kind} buffer requires {bytes} bytes, limit is {limit}")]
    BufferLimit { kind: &'static str, bytes: u64, limit: u64 },
    #[error("geometry count or byte-size overflow")]
    Overflow,
    #[error("invalid vertex arrays or index range")]
    InvalidGeometry,
}

#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct GeometryCounts {
    pub vertices: usize,
    pub indices: usize,
}

impl GeometryCounts {
    pub fn checked_add(self, other: Self) -> Result<Self, GeometryError> {
        Ok(Self {
            vertices: self.vertices.checked_add(other.vertices).ok_or(GeometryError::Overflow)?,
            indices: self.indices.checked_add(other.indices).ok_or(GeometryError::Overflow)?,
        })
    }
}

pub(crate) fn checked_bytes(count: usize, stride: usize, limit: u64, kind: &'static str) -> Result<(), GeometryError> {
    // Both draw counts and rebased indices use u32. Never saturate either.
    u32::try_from(count).map_err(|_| GeometryError::Overflow)?;
    let bytes = count.checked_mul(stride).and_then(|bytes| u64::try_from(bytes).ok()).ok_or(GeometryError::Overflow)?;
    let aligned = bytes.max(1).checked_add(wgpu::COPY_BUFFER_ALIGNMENT - 1).ok_or(GeometryError::Overflow)?
        / wgpu::COPY_BUFFER_ALIGNMENT * wgpu::COPY_BUFFER_ALIGNMENT;
    if aligned > limit { return Err(GeometryError::BufferLimit { kind, bytes: aligned, limit }); }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_check_both_buffers_boundaries_and_overflow() {
        let limits = DrawBatchLimits { max_vertex_bytes: 128, max_index_bytes: 12 };
        assert!(limits.check(GeometryCounts { vertices: 2, indices: 3 }).is_ok());
        assert!(matches!(limits.check(GeometryCounts { vertices: 3, indices: 3 }), Err(GeometryError::BufferLimit { kind: "vertex", .. })));
        assert!(matches!(limits.check(GeometryCounts { vertices: 2, indices: 4 }), Err(GeometryError::BufferLimit { kind: "index", .. })));
        assert!(GeometryCounts { vertices: usize::MAX, indices: 0 }.checked_add(GeometryCounts { vertices: 1, indices: 0 }).is_err());
        assert!(checked_bytes(2, usize::MAX, u64::MAX, "vertex").is_err());
        assert!(checked_bytes(0, 4, 3, "index").is_err()); // Empty-batch sentinel allocation.
        assert!(checked_bytes(1, 1, 3, "index").is_err()); // GPU copy alignment.
        assert!(checked_bytes(u32::MAX as usize + 1, 4, u64::MAX, "index").is_err());
        let device = wgpu::Limits { max_buffer_size: 64, max_storage_buffer_binding_size: 16, ..Default::default() };
        assert_eq!(limits.clamped(&device), DrawBatchLimits { max_vertex_bytes: 64, max_index_bytes: 12 });
    }
}
