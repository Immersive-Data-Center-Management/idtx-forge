//! Authoring methods for [`StageService`]: write paths that author transforms,
//! properties, and session-only prims into the stage's **session** and **root**
//! layers via the `idtx_usd_sys` FFI shim, mapping bridge failures to
//! [`StageError`]. Split out from [`crate::stageservice`] to keep the write
//! surface separate from projection and inspection.
#[cfg(feature = "authoring")]
use idtx_usd_sys::bridge::ffi;

use crate::stageservice::StageService;
#[cfg(feature = "authoring")]
use crate::{
    StageError, PropertyValue,
    StageBehaviorRunScope,
    ShadowMode,
};

impl StageService {
    /// Authors a full 4×4 pose matrix onto `path`'s `xformOp:transform` in the
    /// stage's **session** layer, defining the prim as an `Xform` if absent.
    ///
    /// `matrix` is the transform in **glam column-major order** (i.e.
    /// `Mat4::to_cols_array` widened to `f64`): element `i*4 + j` is column `i`,
    /// row `j`. That layout equals USD's row-major layout of the *transpose*,
    /// which is exactly the `GfMatrix4d` that reproduces the same geometric
    /// transform under USD's row-vector convention — so a glam column-vector
    /// pose round-trips correctly back through [`Self::get_world_transform`].
    ///
    /// Purpose-built for the XR controller pose channel: called each frame with
    /// the `OpenXR` grip pose on the controller's pose Xform. The write lands in the
    /// session layer and surfaces as a `TRANSFORM` change through
    /// `drain_changes` → scoped `project_viewport_roots`, which re-drives the
    /// (possibly skinned) controller subtree beneath the pose Xform.
    ///
    /// Takes a path rather than a [`PrimId`] because the pose Xform is authored
    /// by us at a fixed, well-known path and written every frame; there is no
    /// resync-identity concern for a prim the caller both creates and addresses.
    #[cfg(feature = "authoring")]
    pub fn author_session_transform(
        &mut self,
        path: &str,
        matrix: [f64; 16],
    ) -> Result<(), StageError> {
        ffi::author_session_transform(self.handle.pin_mut(), path, &matrix)?;
        Ok(())
    }

    /// Bulk variant of [`author_session_transform`](Self::author_session_transform):
    /// authors one `xformOp:transform` matrix per `(path, matrix)` entry into the
    /// **session** layer under a single stage-lifecycle lock, a single
    /// `SdfChangeBlock`, and a single edit context — collapsing N notice batches
    /// and N re-projections into one.
    ///
    /// This is the per-frame physics write-back path: a simulation moving many
    /// dynamic bodies authors them all here, then the caller runs **one**
    /// transform-only re-projection ([`StudioSession::tick`]), honouring the
    /// "bulk, not per-prim FFI" invariant. Each `matrix` is glam column-major
    /// (`Mat4::to_cols_array` widened to `f64`), exactly like the scalar variant,
    /// so a glam column-vector pose round-trips through
    /// [`Self::get_world_transform`].
    ///
    /// The whole batch is checkpoint/rollback as a unit: on any failure the
    /// change block closes, the observer unwinds the batch, and an error is
    /// returned with no partial authoring surviving. An empty `entries` is a
    /// no-op `Ok(())`.
    #[cfg(feature = "authoring")]
    pub fn author_session_transforms(
        &mut self,
        entries: &[(String, [f64; 16])],
    ) -> Result<(), StageError> {
        if entries.is_empty() {
            return Ok(());
        }
        let paths: Vec<String> = entries.iter().map(|(p, _)| p.clone()).collect();
        let mut matrices: Vec<f64> = Vec::with_capacity(entries.len() * 16);
        for (_, m) in entries {
            matrices.extend_from_slice(m);
        }
        ffi::author_session_transforms(self.handle.pin_mut(), &paths, &matrices)?;
        Ok(())
    }

    /// Overrides `path`'s `visibility` in the stage's **session** layer —
    /// `inherited` when `visible`, else `invisible`. The prim must already
    /// compose from a weaker layer (this authors only the override, not a
    /// `def`).
    ///
    /// Used by the XR controller hydration path to reveal the otherwise-invisible
    /// controller proxy (`/XR/Controllers/{Left,Right}`) for the hand driven
    /// through normal hydration, keeping visibility a USD opinion (single source
    /// of truth) rather than a render-side flag. The change surfaces as a
    /// `VISIBILITY` record through `drain_changes`.
    ///
    /// A `path` that does not compose, or composes to a non-imageable (typeless)
    /// prim, is a **soft no-op** (`Ok`): a session visibility opinion cannot
    /// attach there, and the ECS write drives rendering regardless. Only genuine
    /// authoring failures return `Err`.
    #[cfg(feature = "authoring")]
    pub fn author_session_visibility(
        &mut self,
        path: &str,
        visible: bool,
    ) -> Result<(), StageError> {
        ffi::author_session_visibility(self.handle.pin_mut(), path, visible)?;
        Ok(())
    }

    /// Authors a single scalar/asset/token/`int2` attribute onto an existing
    /// prim in the stage's **session** layer (never dirtying the saved
    /// assembly). The prim must already compose — define it first with
    /// [`define_session_prim`](Self::define_session_prim).
    ///
    /// This is the attribute-authoring counterpart of
    /// [`author_session_transform`](Self::author_session_transform): it is used
    /// to author runtime-only parameters (e.g. the `IdtxHud` crosshair's
    /// `idtx:hud:image` / `idtx:hud:sizePx`, or an `IdtxWidgetFieldString`'s
    /// `idtx:widget:value`) that a behavior writes into the session layer. Only
    /// the value kinds the session shim understands are supported —
    /// [`PropertyValue::Asset`], [`PropertyValue::Token`], [`PropertyValue::Bool`],
    /// [`PropertyValue::F32`], [`PropertyValue::String`] and [`PropertyValue::Int2`];
    /// any other variant returns [`StageError::InvalidProjection`].
    ///
    /// The change surfaces through [`drain_changes`](Self::drain_changes) tagged
    /// as *not* authored in the root layer; the caller re-projects afterwards.
    #[cfg(feature = "authoring")]
    pub fn author_session_property(
        &mut self,
        path: &str,
        attr: &str,
        value: &PropertyValue,
    ) -> Result<(), StageError> {
        let (kind, encoded) = match value {
            PropertyValue::Asset(v) => ("asset", v.clone()),
            PropertyValue::Token(v) => ("token", v.clone()),
            PropertyValue::String(v) => ("string", v.clone()),
            PropertyValue::Bool(v) => ("bool", v.to_string()),
            PropertyValue::F32(v) => {
                if !v.is_finite() {
                    return Err(StageError::InvalidProjection {
                        path: attr.to_owned(),
                        detail: "session property value must be finite".to_owned(),
                    });
                }
                ("float", v.to_string())
            }
            PropertyValue::Int2(v) => ("int2", format!("{} {}", v[0], v[1])),
            _ => {
                return Err(StageError::InvalidProjection {
                    path: attr.to_owned(),
                    detail: "unsupported session property value kind \
                             (expected asset/token/string/bool/float/int2)"
                        .to_owned(),
                });
            }
        };
        ffi::author_session_property(self.handle.pin_mut(), path, attr, kind, &encoded)?;
        Ok(())
    }

    /// Authors the four-state stage shadow mode into the **root** layer's
    /// `customLayerData["idtx:shadowMode"]` token and erases the legacy
    /// `idtx:softShadows` bool (single source of truth). This is the write
    /// counterpart of the [`StageMetadata::shadow_mode`] read; the cached
    /// [`StageMetadata`] is *not* refreshed by this call, so the caller (the
    /// session) owns the live copy.
    #[cfg(feature = "authoring")]
    pub fn author_root_shadow_mode(&mut self, mode: ShadowMode) -> Result<(), StageError> {
        ffi::author_root_shadow_mode(self.handle.pin_mut(), mode.as_i32())?;
        Ok(())
    }

    /// Authors the stage-level behavior assignment into the **root** layer's
    /// `customLayerData`: `idtx:stageBehavior` (the prim name, e.g.
    /// `"StageBehavior"`) and `idtx:stageBehaviorRunScope` (the run-condition
    /// token). The write counterpart of [`StageMetadata::stage_behavior_name`] /
    /// [`StageMetadata::stage_behavior_run_scope`]; the cached [`StageMetadata`]
    /// is *not* refreshed here, so the caller (session) owns the live copy.
    #[cfg(feature = "authoring")]
    pub fn author_root_stage_behavior(
        &mut self,
        name: &str,
        run_scope: StageBehaviorRunScope,
    ) -> Result<(), StageError> {
        ffi::author_root_stage_behavior(self.handle.pin_mut(), name, run_scope.as_token())?;
        Ok(())
    }

    /// Erases both stage-behavior `customLayerData` keys from the root layer
    /// (counterpart of [`Self::author_root_stage_behavior`]).
    #[cfg(feature = "authoring")]
    pub fn clear_root_stage_behavior(&mut self) -> Result<(), StageError> {
        ffi::clear_root_stage_behavior(self.handle.pin_mut())?;
        Ok(())
    }

    #[cfg(test)]
    fn conformance_author_double(
        &mut self,
        property_path: &str,
        value: f64,
    ) -> Result<(), StageError> {
        ffi::conformance_author_double(self.handle.pin_mut(), property_path, value)?;
        Ok(())
    }

    #[cfg(test)]
    fn conformance_author_double_at_time(
        &mut self,
        property_path: &str,
        value: f64,
        time_code: f64,
    ) -> Result<(), StageError> {
        ffi::conformance_author_double_at_time(
            self.handle.pin_mut(),
            property_path,
            value,
            time_code,
        )?;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn conformance_author_vec3f_array_at_time(
        &mut self,
        property_path: &str,
        values: &[[f32; 3]],
        time_code: f64,
    ) -> Result<(), StageError> {
        let flat = values
            .iter()
            .flat_map(|value| value.iter().copied())
            .collect::<Vec<_>>();
        ffi::conformance_author_vec3f_array_at_time(
            self.handle.pin_mut(),
            property_path,
            &flat,
            time_code,
        )?;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn conformance_set_relationship_target(
        &mut self,
        relationship_path: &str,
        target_path: &str,
    ) -> Result<(), StageError> {
        ffi::conformance_set_relationship_target(
            self.handle.pin_mut(),
            relationship_path,
            target_path,
        )?;
        Ok(())
    }

    #[cfg(test)]
    fn conformance_set_custom_data(
        &mut self,
        prim_path: &str,
        key: &str,
        value: &str,
    ) -> Result<(), StageError> {
        ffi::conformance_set_custom_data(self.handle.pin_mut(), prim_path, key, value)?;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn conformance_define_xform(&mut self, path: &str) -> Result<(), StageError> {
        ffi::conformance_define_xform(self.handle.pin_mut(), path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;