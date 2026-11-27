//! Bounded native PointInstancer arrays. Positions are in instancer-local USD
//! units; rotations are unit quaternions in **XYZW** order. USD includes the
//! prototype transform; the renderer applies the instancer world transform once.
#[cfg(all(test, feature = "authoring"))]
mod tests;
use crate::{PointInstancerRecord, StageError, StageService};
use idtx_usd_sys::bridge::ffi;

pub const MAX_POINT_INSTANCES: usize = 4096;
pub const MAX_POINT_PROTOTYPES: usize = 256;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct PointInstancerPlacement {
    pub positions: Vec<[f32; 3]>,
    /// Empty means identity for every entry.
    pub orientations_xyzw: Vec<[f32; 4]>,
    /// Empty means unit scale for every entry.
    pub scales: Vec<[f32; 3]>,
    pub prototype_indices: Vec<i32>,
    /// Explicit, unique signed USD ids; order and contiguity are immaterial.
    pub ids: Vec<i64>,
    pub invisible_ids: Vec<i64>,
}

#[derive(Debug, Clone)]
pub struct PointInstancerInspection {
    pub placement: PointInstancerPlacement,
    pub prototypes: Vec<String>,
    pub mask: Vec<u8>,
    pub inactive_ids: Vec<i64>,
    pub count: u64,
    pub truncated: bool,
    pub diagnostics: Vec<String>,
}

impl PointInstancerPlacement {
    /// Checks shape/finite/id invariants without touching USD. Prototype
    /// resolution and the composed schema type are rechecked at native commit.
    pub fn validate(&self, prototype_count: usize) -> Result<(), StageError> {
        let invalid = |s: &str| StageError::OpenUsd(format!("PointInstancer: {s}"));
        let n = self.ids.len();
        if n > MAX_POINT_INSTANCES || prototype_count > MAX_POINT_PROTOTYPES {
            return Err(invalid("exceeds 4096 instances / 256 prototypes"));
        }
        if self.positions.len() != n
            || self.prototype_indices.len() != n
            || (!self.orientations_xyzw.is_empty() && self.orientations_xyzw.len() != n)
            || (!self.scales.is_empty() && self.scales.len() != n)
        {
            return Err(invalid("array lengths differ"));
        }
        if self
            .positions
            .iter()
            .flatten()
            .chain(self.scales.iter().flatten())
            .any(|v| !v.is_finite())
        {
            return Err(invalid("positions / scales must be finite"));
        }
        if self.orientations_xyzw.iter().any(|q| {
            let norm: f64 = q.iter().map(|&v| f64::from(v).powi(2)).sum();
            !norm.is_finite() || (norm - 1.0).abs() > 0.002
        }) {
            return Err(invalid("orientations must be unit XYZW quaternions"));
        }
        if self
            .prototype_indices
            .iter()
            .any(|&i| i < 0 || i as usize >= prototype_count)
        {
            return Err(invalid("prototype index out of range"));
        }
        let ids: std::collections::HashSet<_> = self.ids.iter().copied().collect();
        let mut hidden = std::collections::HashSet::new();
        if ids.len() != n
            || self
                .invisible_ids
                .iter()
                .any(|id| !ids.contains(id) || !hidden.insert(id))
        {
            return Err(invalid(
                "ids must be unique; invisibleIds must be unique existing ids",
            ));
        }
        Ok(())
    }

    #[cfg(feature = "authoring")]
    fn raw(&self) -> ffi::PointInstancerUpdateRaw {
        ffi::PointInstancerUpdateRaw {
            positions: self.positions.iter().flatten().copied().collect(),
            orientations_xyzw: self.orientations_xyzw.iter().flatten().copied().collect(),
            scales: self.scales.iter().flatten().copied().collect(),
            proto_indices: self.prototype_indices.clone(),
            ids: self.ids.clone(),
            invisible_ids: self.invisible_ids.clone(),
        }
    }
}

impl StageService {
    /// Inspect the native session-layer opinions without flattening or saving them.
    pub fn session_layer_text(&self) -> Result<String, StageError> {
        Ok(ffi::session_layer_text(self.handle_ref()?)?)
    }
    /// Replaces placement and visibility together, atomically, in the session
    /// layer. Reallocation is explicit: ids retained in the replacement retain
    /// their identity, even if reordered. No new prims or prototype edits.
    #[cfg(feature = "authoring")]
    pub fn update_point_instancer(
        &mut self,
        path: &str,
        placement: &PointInstancerPlacement,
    ) -> Result<(), StageError> {
        self.write_point_instancer(path, placement, true)
    }

    /// Normal editor authoring path (root-layer defaults, journalled by Conductor).
    #[cfg(feature = "authoring")]
    pub fn author_point_instancer_defaults(
        &mut self,
        path: &str,
        placement: &PointInstancerPlacement,
    ) -> Result<(), StageError> {
        self.write_point_instancer(path, placement, false)
    }

    #[cfg(feature = "authoring")]
    fn write_point_instancer(
        &mut self,
        path: &str,
        placement: &PointInstancerPlacement,
        session: bool,
    ) -> Result<(), StageError> {
        // Reject oversized arrays before flattening / crossing FFI. The actual
        // composed relationship's (possibly smaller) size is checked at commit.
        placement.validate(MAX_POINT_PROTOTYPES)?;
        ffi::update_point_instancer(
            self.handle.pin_mut(),
            path,
            &placement.raw(),
            false,
            session,
        )?;
        Ok(())
    }

    /// Replaces only native invisibleIds. Defaults are overwritten, not appended
    /// time samples: pulses have bounded memory and remain inspectable in USD.
    /// inactiveIds metadata and all placement/prototype arrays are preserved.
    #[cfg(feature = "authoring")]
    pub fn update_point_instancer_mask(
        &mut self,
        path: &str,
        invisible_ids: &[i64],
    ) -> Result<(), StageError> {
        if invisible_ids.len() > MAX_POINT_INSTANCES {
            return Err(StageError::OpenUsd(
                "PointInstancer mask exceeds size limit".into(),
            ));
        }
        let raw = ffi::PointInstancerUpdateRaw {
            invisible_ids: invisible_ids.to_vec(),
            ..Default::default()
        };
        ffi::update_point_instancer(self.handle.pin_mut(), path, &raw, true, true)?;
        Ok(())
    }

    /// Schema-aware, bounded inspection. Invalid entries remain inspectable even
    /// when native transform computation cannot render them.
    pub fn inspect_point_instancer(
        &self,
        path: &str,
        time: Option<f64>,
    ) -> Result<PointInstancerInspection, StageError> {
        if time.is_some_and(|t| !t.is_finite()) {
            return Err(StageError::OpenUsd("non-finite time".into()));
        }
        let raw = ffi::inspect_point_instancer(
            self.handle_ref()?,
            path,
            time.unwrap_or_default(),
            time.is_none(),
        )?;
        Ok(PointInstancerInspection {
            placement: PointInstancerPlacement {
                positions: raw
                    .arrays
                    .positions
                    .chunks_exact(3)
                    .map(|v| [v[0], v[1], v[2]])
                    .collect(),
                orientations_xyzw: raw
                    .arrays
                    .orientations_xyzw
                    .chunks_exact(4)
                    .map(|v| [v[0], v[1], v[2], v[3]])
                    .collect(),
                scales: raw
                    .arrays
                    .scales
                    .chunks_exact(3)
                    .map(|v| [v[0], v[1], v[2]])
                    .collect(),
                prototype_indices: raw.arrays.proto_indices,
                ids: raw.arrays.ids,
                invisible_ids: raw.arrays.invisible_ids,
            },
            prototypes: raw.prototype_paths,
            mask: raw.mask,
            inactive_ids: raw.inactive_ids,
            count: raw.count,
            truncated: raw.truncated,
            diagnostics: raw.diagnostics,
        })
    }

    /// Narrow projection: no stage traversal, bounds, mesh/material reads, or
    /// prototype-cache invalidation. Mask-only samples omit transforms entirely.
    pub fn sample_point_instancers(
        &self,
        paths: &[String],
        mask_only: bool,
    ) -> Result<(Vec<PointInstancerRecord>, Vec<String>), StageError> {
        let raw = ffi::sample_point_instancers(self.handle_ref()?, paths, 0.0, true, mask_only)?;
        let records = raw
            .records
            .into_iter()
            .map(|r| self.decode_point_instancer(r))
            .collect::<Result<_, _>>()?;
        Ok((records, raw.diagnostics))
    }

    pub(crate) fn decode_point_instancer(
        &self,
        raw: ffi::PointInstancerRecordRaw,
    ) -> Result<PointInstancerRecord, StageError> {
        let mut prototypes = Vec::new();
        let remap: Vec<_> = raw
            .prototype_paths
            .iter()
            .map(|p| {
                self.prim_at_path(p).map(|id| {
                    let index = prototypes.len() as u32;
                    prototypes.push(id);
                    index
                })
            })
            .collect();
        // Unloaded/dangling prototypes cannot abort opening the rest of a stage.
        // Keep the entry's id, but an invalid selector never produces a draw.
        let proto_indices = raw
            .proto_indices
            .iter()
            .map(|&i| {
                usize::try_from(i)
                    .ok()
                    .and_then(|i| remap.get(i))
                    .copied()
                    .flatten()
                    .unwrap_or(u32::MAX)
            })
            .collect();
        Ok(PointInstancerRecord {
            prim: self
                .prim_at_path(&raw.path)
                .ok_or_else(|| StageError::UnknownPath(raw.path.clone()))?,
            prototypes,
            proto_indices,
            transforms: raw
                .transforms
                .chunks_exact(16)
                .map(|m| m.try_into().expect("matrix chunk"))
                .collect(),
            ids: raw.ids,
            mask: raw.mask,
            opacities: raw.opacities,
        })
    }
}
