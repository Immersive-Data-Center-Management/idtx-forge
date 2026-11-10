use super::StageService;
use crate::{ShadowMode, StageError};
#[cfg(feature = "authoring")]
use crate::{
    LayerEdit, PrimId, PrimTemplate,
    PropertyValue, StructuralCommand, TopologyDelta,
};
use idtx_usd_sys::bridge::ffi;

impl StageService {
    /// Serializes the stage's root layer to USDA text. Errors with [`StageError`]
    /// if the handle is null or serialization fails.
    #[cfg(feature = "authoring")]
    pub fn root_layer_text(&self) -> Result<String, StageError> {
        ffi::root_layer_text(self.handle_ref()?).map_err(Into::into)
    }

    /// Root opinions excluding static transform values carried by collaboration.
    #[cfg(feature = "authoring")]
    pub fn root_non_transform_text(&self) -> Result<String, StageError> {
        ffi::root_non_transform_text(self.handle_ref()?).map_err(Into::into)
    }

    /// Root opinions for one subtree; empty means no root spec. Includes children.
    #[cfg(feature = "authoring")]
    pub fn root_prim_text(&self, path: &str) -> Result<String, StageError> {
        ffi::root_prim_text(self.handle_ref()?, path).map_err(Into::into)
    }

    /// Restore a scoped snapshot, preserving all opinions outside this subtree.
    #[cfg(feature = "authoring")]
    pub fn replace_root_prim_text(&mut self, path: &str, source: &str) -> Result<(), StageError> {
        ffi::replace_root_prim_text(self.handle.pin_mut(), path, source)?;
        Ok(())
    }

    /// Reconcile create-protocol fields. Caller must roll back on later failure.
    #[cfg(feature = "authoring")]
    pub fn reconcile_root_prim(
        &mut self,
        path: &str,
        type_name: &str,
        asset: &str,
        payload: bool,
    ) -> Result<TopologyDelta, StageError> {
        ffi::reconcile_root_prim(self.handle.pin_mut(), path, type_name, asset, payload)?;
        self.project_topology_paths(&[path.to_owned()])
    }

    /// Author the IDTX stage-kind tag into this stage's root-layer
    /// `customLayerData.idtx` (`stageKind`, `schemaVersion = "1"`, and — when
    /// non-empty — `displayName`). Used by the project/level authoring flow to
    /// classify a stage file (see roadmap Appendix A.1). An empty `display_name`
    /// authors no `displayName` key.
    #[cfg(feature = "authoring")]
    pub fn set_stage_kind(
        &mut self,
        stage_kind: &str,
        display_name: &str,
    ) -> Result<(), StageError> {
        ffi::set_root_stage_kind(self.handle.pin_mut(), stage_kind, display_name)?;
        Ok(())
    }

    /// Replaces the entire root layer with the given USDA `source`. Errors with
    /// [`StageError`] if the text is invalid or the write fails.
    #[cfg(feature = "authoring")]
    pub fn replace_root_layer_text(&mut self, source: &str) -> Result<(), StageError> {
        ffi::replace_root_layer_text(self.handle.pin_mut(), source)?;
        Ok(())
    }

    /// Apply only changed authored opinions after validating their expected
    /// values against the current root. Unrelated opinions survive; conflicts
    /// leave the complete live layer unchanged.
    #[cfg(feature = "authoring")]
    pub fn apply_root_layer_delta(
        &mut self,
        expected: &str,
        desired: &str,
    ) -> Result<(), StageError> {
        ffi::apply_root_layer_delta(self.handle.pin_mut(), expected, desired)?;
        Ok(())
    }

    #[cfg(feature = "authoring")]
    pub fn root_transform_text(&self, path: &str) -> Result<String, StageError> {
        ffi::root_transform_text(self.handle_ref()?, path).map_err(Into::into)
    }

    /// Re-reads the current shadow mode straight from live stage metadata rather
    /// than the cached [`Self::metadata`]. Use after a root-layer edit that may
    /// have changed it.
    pub fn reread_shadow_mode(&self) -> Result<ShadowMode, StageError> {
        Ok(ShadowMode::from_i32(
            ffi::stage_metadata(self.handle_ref()?)?.shadow_mode,
        ))
    }

    #[cfg(feature = "authoring")]
    pub fn session_variant_opinion(
        &self,
        path: &str,
        set: &str,
    ) -> Result<Option<String>, StageError> {
        let raw = ffi::session_variant_opinion(self.handle_ref()?, path, set)?;
        Ok(raw.authored.then_some(raw.value))
    }

    #[cfg(feature = "authoring")]
    pub fn restore_session_variant_opinion(
        &mut self,
        path: &str,
        set: &str,
        value: Option<&str>,
    ) -> Result<(), StageError> {
        ffi::restore_session_variant_opinion(
            self.handle.pin_mut(),
            path,
            set,
            value.is_some(),
            value.unwrap_or_default(),
        )?;
        Ok(())
    }

    /// Discards queued, undrained change notices on the stage without applying
    /// them. Errors with [`StageError`] if the handle is null.
    #[cfg(feature = "authoring")]
    pub fn discard_pending_changes(&mut self) -> Result<(), StageError> {
        ffi::discard_pending_changes(self.handle.pin_mut())?;
        Ok(())
    }

    #[cfg(feature = "authoring")]
    pub fn root_property_text(&self, prim: PrimId, property: &str) -> Result<String, StageError> {
        self.validate(prim)?;
        ffi::root_property_text(self.handle_ref()?, self.path_for_prim(prim)?, property)
            .map_err(Into::into)
    }

    #[cfg(feature = "authoring")]
    pub fn replace_root_property_text(
        &mut self,
        prim: PrimId,
        property: &str,
        source: &str,
    ) -> Result<(), StageError> {
        self.validate(prim)?;
        let path = self.path_for_prim(prim)?.to_owned();
        ffi::replace_root_property_text(self.handle.pin_mut(), &path, property, source)?;
        Ok(())
    }

    /// Applies a single [`StructuralCommand`] (define/override/delete/rename/
    /// reparent, reference/payload/sublayer/API edits, set-active, bind-material,
    /// attach-behavior, or a typed set-property) by lowering it to the bridge's
    /// `apply_structural_edit` call. Validates finiteness of numeric property
    /// values. Does not reproject; the caller drains and reprojects. Errors if a
    /// referenced prim is stale or a value is non-finite/malformed.
    #[cfg(feature = "authoring")]
    pub fn apply_structural_command(
        &mut self,
        command: &StructuralCommand,
    ) -> Result<(), StageError> {
        let (kind, path, destination, type_name, asset_path) = match command {
            StructuralCommand::Define { path, type_name } => (
                "define",
                path.clone(),
                String::new(),
                type_name.clone(),
                String::new(),
            ),
            StructuralCommand::CreateOverride { path } => (
                "create-over",
                path.clone(),
                String::new(),
                String::new(),
                String::new(),
            ),
            StructuralCommand::RemoveOverride { path } => (
                "remove-over",
                path.clone(),
                String::new(),
                String::new(),
                String::new(),
            ),
            StructuralCommand::Delete { prim } => (
                "delete",
                self.path_for_prim(*prim)?.to_owned(),
                String::new(),
                String::new(),
                String::new(),
            ),
            StructuralCommand::SetActive { path, active } => (
                "set-active",
                path.clone(),
                String::new(),
                // The boolean rides the `type_name` slot (as `set-property-bool`
                // does); the shim reads "true" / "false".
                if *active { "true" } else { "false" }.to_owned(),
                String::new(),
            ),
            StructuralCommand::Rename { prim, new_name } => {
                let source = self.path_for_prim(*prim)?.to_owned();
                let parent = source.rsplit_once('/').map_or("", |(parent, _)| parent);
                let destination = if parent.is_empty() {
                    format!("/{new_name}")
                } else {
                    format!("{parent}/{new_name}")
                };
                ("rename", source, destination, String::new(), String::new())
            }
            StructuralCommand::Reparent { prim, new_parent } => {
                let source = self.path_for_prim(*prim)?.to_owned();
                let name = source.rsplit('/').next().unwrap_or_default();
                let parent = self.path_for_prim(*new_parent)?;
                let destination = if parent == "/" {
                    format!("/{name}")
                } else {
                    format!("{parent}/{name}")
                };
                (
                    "reparent",
                    source,
                    destination,
                    String::new(),
                    String::new(),
                )
            }
            StructuralCommand::AddReference {
                prim,
                asset_path,
                target_path,
            } => (
                "add-reference",
                self.path_for_prim(*prim)?.to_owned(),
                target_path.clone().unwrap_or_default(),
                String::new(),
                asset_path.clone(),
            ),
            StructuralCommand::AddPayload {
                prim,
                asset_path,
                target_path,
            } => (
                "add-payload",
                self.path_for_prim(*prim)?.to_owned(),
                target_path.clone().unwrap_or_default(),
                String::new(),
                asset_path.clone(),
            ),
            StructuralCommand::AttachBehavior { prim, asset_path } => (
                "attach-behavior",
                self.path_for_prim(*prim)?.to_owned(),
                String::new(),
                String::new(),
                asset_path.clone(),
            ),
            StructuralCommand::SetProperty {
                prim,
                property,
                value,
            } => {
                let (kind, value) = match value {
                    PropertyValue::Bool(value) => ("set-property-bool", value.to_string()),
                    PropertyValue::F32(value) => {
                        if !value.is_finite() {
                            return Err(StageError::InvalidProjection {
                                path: property.clone(),
                                detail: "property value must be finite".to_owned(),
                            });
                        }
                        ("set-property-float", value.to_string())
                    }
                    PropertyValue::F64(value) => {
                        if !value.is_finite() {
                            return Err(StageError::InvalidProjection {
                                path: property.clone(),
                                detail: "property value must be finite".to_owned(),
                            });
                        }
                        ("set-property-double", value.to_string())
                    }
                    PropertyValue::String(value) => ("set-property-string", value.clone()),
                    PropertyValue::Token(value) => ("set-property-token", value.clone()),
                    PropertyValue::TokenArray(values) => {
                        if values.iter().any(|v| v.contains('\n')) {
                            return Err(StageError::InvalidProjection {
                                path: property.clone(),
                                detail: "token value must not contain a newline".to_owned(),
                            });
                        }
                        ("set-property-token-array", values.join("\n"))
                    }
                    PropertyValue::Color3f(value) => {
                        if !value.iter().all(|component| component.is_finite()) {
                            return Err(StageError::InvalidProjection {
                                path: property.clone(),
                                detail: "property color must be finite".to_owned(),
                            });
                        }
                        (
                            "set-property-color3f",
                            format!("{} {} {}", value[0], value[1], value[2]),
                        )
                    }
                    PropertyValue::Vector3f(value) => {
                        if !value.iter().all(|component| component.is_finite()) {
                            return Err(StageError::InvalidProjection {
                                path: property.clone(),
                                detail: "property vector must be finite".to_owned(),
                            });
                        }
                        (
                            "set-property-vector3f",
                            format!("{} {} {}", value[0], value[1], value[2]),
                        )
                    }
                    PropertyValue::Asset(value) => ("set-property-asset", value.clone()),
                    PropertyValue::Int(value) => ("set-property-int", value.to_string()),
                    PropertyValue::Int2(value) => {
                        ("set-property-int2", format!("{} {}", value[0], value[1]))
                    }
                    PropertyValue::Point3Array(values) => {
                        if values.iter().any(|p| !p.iter().all(|c| c.is_finite())) {
                            return Err(StageError::InvalidProjection {
                                path: property.clone(),
                                detail: "point3 array values must be finite".to_owned(),
                            });
                        }
                        // Flat `x y z` triples, space-separated; empty authors an
                        // empty point3f[].
                        let encoded = values
                            .iter()
                            .flat_map(|p| [p[0], p[1], p[2]])
                            .map(|c| c.to_string())
                            .collect::<Vec<_>>()
                            .join(" ");
                        ("set-property-point3-array", encoded)
                    }
                    PropertyValue::IntArray(values) => {
                        let encoded = values
                            .iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                            .join(" ");
                        ("set-property-int-array", encoded)
                    }
                };
                (
                    kind,
                    self.path_for_prim(*prim)?.to_owned(),
                    property.clone(),
                    value,
                    String::new(),
                )
            }
            StructuralCommand::BindMaterial { prim, material } => (
                "bind-material",
                self.path_for_prim(*prim)?.to_owned(),
                self.path_for_prim(*material)?.to_owned(),
                String::new(),
                String::new(),
            ),
            StructuralCommand::AddSublayer { asset_path } => (
                "add-sublayer",
                String::new(),
                String::new(),
                String::new(),
                asset_path.clone(),
            ),
            StructuralCommand::RemoveSublayer { asset_path } => (
                "remove-sublayer",
                String::new(),
                String::new(),
                String::new(),
                asset_path.clone(),
            ),
            StructuralCommand::ApplyApi { path, schema } => (
                "apply-api",
                path.clone(),
                schema.clone(),
                String::new(),
                String::new(),
            ),
            StructuralCommand::RemoveApi { path, schema } => (
                "remove-api",
                path.clone(),
                schema.clone(),
                String::new(),
                String::new(),
            ),
        };
        #[cfg(feature = "structural-metrics")]
        let started = Instant::now();
        ffi::apply_structural_edit(
            self.handle.pin_mut(),
            kind,
            &path,
            &destination,
            &type_name,
            &asset_path,
        )?;
        #[cfg(feature = "structural-metrics")]
        {
            self.metrics.authoring.calls += 1;
            self.metrics.authoring.elapsed += started.elapsed();
        }
        Ok(())
    }

    /// Apply an interactive layer-stack edit (set edit target / mute / unmute /
    /// reorder sublayer). The caller reprojects topology afterwards, since
    /// muting and reordering change composition.
    #[cfg(feature = "authoring")]
    pub fn apply_layer_edit(&mut self, edit: &LayerEdit) -> Result<(), StageError> {
        let (kind, identifier, parent, direction): (&str, &str, &str, i32) = match edit {
            LayerEdit::SetEditTarget { identifier } => {
                ("set-edit-target", identifier.as_str(), "", 0)
            }
            LayerEdit::Mute { identifier } => ("mute", identifier.as_str(), "", 0),
            LayerEdit::Unmute { identifier } => ("unmute", identifier.as_str(), "", 0),
            LayerEdit::MoveSublayer {
                identifier,
                parent_identifier,
                stronger,
            } => (
                "move-sublayer",
                identifier.as_str(),
                parent_identifier.as_str(),
                if *stronger { -1 } else { 1 },
            ),
        };
        ffi::apply_layer_edit(self.handle.pin_mut(), kind, identifier, parent, direction)?;
        Ok(())
    }

    /// Author a typed prim from a [`PrimTemplate`] — a `Define` for the prim
    /// type followed by a `SetProperty` for each of the template's default
    /// attributes.
    ///
    /// The prim is reprojected between the define and the property writes
    /// because [`Self::prim_at_path`] reads the projected topology (not the live
    /// stage), so the freshly-defined prim must be projected before it resolves
    /// to a [`PrimId`] the `SetProperty` commands can target. Change notices are
    /// intentionally left un-drained so the caller's normal drain/reproject
    /// (the session tick, or the test harness) settles them once — matching the
    /// other structural wrappers. No-ops with `Ok(())` if the prim fails to
    /// resolve after the define (e.g. an invalid path).
    #[cfg(feature = "authoring")]
    pub fn define_prim_from_template(
        &mut self,
        path: &str,
        template: PrimTemplate,
    ) -> Result<(), StageError> {
        self.apply_structural_command(&StructuralCommand::Define {
            path: path.to_owned(),
            type_name: template.type_name().to_owned(),
        })?;
        self.project_topology()?;
        let Some(prim) = self.prim_at_path(path) else {
            return Ok(());
        };
        for (property, value) in template.default_properties() {
            self.apply_structural_command(&StructuralCommand::SetProperty {
                prim,
                property: property.to_owned(),
                value,
            })?;
        }
        Ok(())
    }
}
