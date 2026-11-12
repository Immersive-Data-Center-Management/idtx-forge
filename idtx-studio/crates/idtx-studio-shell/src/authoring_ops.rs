//! Inspector edit plumbing.
//!
//! [`InspectorEditSink`] turns inspector-panel edits into [`ShellAction`]s
//! (variant selection always; attribute/transform authoring under the
//! `authoring` feature). `apply_edit_command_{reverse,forward}` replay journal
//! commands for undo/redo.

use idtx_editor_ui::inspector::EditSink;

use crate::model::ShellAction;

#[cfg(feature = "authoring")]
use idtx_authoring::{EditCommand, TransformTrs};

#[cfg(feature = "authoring")]
pub(crate) fn journal_root_edit<T>(journal: &mut idtx_authoring::EditJournal,
    session: &mut idtx_studio_session::StudioSession, label: impl Into<String>,
    edit: impl FnOnce(&mut idtx_studio_session::StudioSession) -> Result<T, idtx_studio_session::SessionError>) -> Result<T, idtx_studio_session::SessionError> {
    let before = session.root_layer_snapshot()?;
    match edit(session).and_then(|value| session.root_layer_snapshot().map(|after| (value, after))) {
        Ok((value, after)) => { journal.push(EditCommand::Structural { label: label.into(), before, after }); Ok(value) }
        Err(error) => { session.restore_root_layer_snapshot(&before)?; Err(error) }
    }
}

#[cfg(feature = "authoring")]
pub(crate) fn capture_property_edit(journal: &mut idtx_authoring::EditJournal,
    session: &idtx_studio_session::StudioSession, path: &str, property: &str) -> Result<(), idtx_studio_session::SessionError> {
    if !journal.has_property_capture(path, property)
        && !journal.capture_property(path, property, session.snapshot_root_property(path, property)?) {
        return Err(idtx_usd::StageError::OpenUsd("pending undo capture capacity reached".into()).into());
    }
    Ok(())
}

#[cfg(feature = "authoring")]
pub(crate) fn finish_property_edit(journal: &mut idtx_authoring::EditJournal,
    session: &idtx_studio_session::StudioSession, path: &str, property: &str) -> Result<(), idtx_studio_session::SessionError> {
    journal.finish_property(path, property, session.snapshot_root_property(path, property)?);
    Ok(())
}

#[cfg(feature = "authoring")]
pub(crate) fn capture_transform_edit(journal: &mut idtx_authoring::EditJournal,
    session: &idtx_studio_session::StudioSession, path: &str) -> Result<(), idtx_studio_session::SessionError> {
    if !journal.has_property_capture(path, "") && !journal.capture_property(path, "", session.root_transform_snapshot(path)?) {
        return Err(idtx_usd::StageError::OpenUsd("pending undo capture capacity reached".into()).into());
    }
    Ok(())
}

#[cfg(feature = "authoring")]
pub(crate) fn finish_transform_edit(journal: &mut idtx_authoring::EditJournal,
    session: &idtx_studio_session::StudioSession, path: &str) -> Result<(), idtx_studio_session::SessionError> {
    journal.finish_transform(path, session.root_transform_snapshot(path)?);
    Ok(())
}

#[cfg(all(test, feature = "authoring"))]
mod undo_tests {
    use super::*;
    use idtx_studio_session::{StudioSession, TextureDecodeConfig, RenderWorldUnitScale};
    use std::path::PathBuf;

    struct Fixture { session: StudioSession, directory: PathBuf }
    impl std::ops::Deref for Fixture { type Target = StudioSession; fn deref(&self) -> &StudioSession { &self.session } }
    impl std::ops::DerefMut for Fixture { fn deref_mut(&mut self) -> &mut StudioSession { &mut self.session } }
    impl Drop for Fixture { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.directory); } }

    fn open() -> Fixture {
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testfiles/usd/undo");
        let directory = fixture.join(format!("test-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir(&directory).unwrap();
        std::fs::copy(fixture.join("stage.usda"), directory.join("stage.usda")).unwrap();
        let (device, queue) = pollster::block_on(async {
            let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
            instance.request_adapter(&wgpu::RequestAdapterOptions::default()).await.unwrap()
                .request_device(&wgpu::DeviceDescriptor::default()).await.unwrap()
        });
        let session = StudioSession::open_with_gpu(directory.join("stage.usda"),
            device, queue, [32, 32], TextureDecodeConfig::FULL_RESOLUTION, RenderWorldUnitScale::METRE).unwrap();
        Fixture { session, directory }
    }

    #[test]
    #[ignore = "requires OpenUSD and headless GPU"]
    fn first_attribute_undo_restores_absent_authored_opinion() {
        let mut session = open();
        let before = session.snapshot_root_property("/World/Cube", "size").unwrap();
        assert!(before.is_empty());
        session.author_attribute("/World/Cube", "size", idtx_usd::PropertyValue::F64(3.0)).unwrap();
        let command = EditCommand::SetAttribute { path: "/World/Cube".into(), property: "size".into(),
            before: None, after: idtx_usd::PropertyValue::F64(3.0) };
        apply_edit_command_reverse(&command, &mut session).unwrap();
        assert_eq!(session.snapshot_root_property("/World/Cube", "size").unwrap(), before);
    }

    #[test]
    #[ignore = "requires OpenUSD and headless GPU"]
    fn structural_undo_preserves_unrelated_peer_opinions() {
        let mut session = open();
        let before = session.root_layer_snapshot().unwrap();
        session.define_prim("/World/Created", "Xform").unwrap();
        let after = session.root_layer_snapshot().unwrap();
        session.author_attribute("/World/Cube", "size", idtx_usd::PropertyValue::F64(7.0)).unwrap();
        let peer_opinion = session.snapshot_root_property("/World/Cube", "size").unwrap();
        let command = EditCommand::Structural { label: "Create".into(), before, after };
        apply_edit_command_reverse(&command, &mut session).unwrap();
        assert!(session.stage().prim_at_path("/World/Created").is_none());
        assert_eq!(session.snapshot_root_property("/World/Cube", "size").unwrap(), peer_opinion);
        apply_edit_command_forward(&command, &mut session).unwrap();
        assert!(session.stage().prim_at_path("/World/Created").is_some());
        assert_eq!(session.snapshot_root_property("/World/Cube", "size").unwrap(), peer_opinion);
    }

    #[test]
    #[ignore = "requires OpenUSD and headless GPU"]
    fn live_property_gesture_records_authored_absence_and_detects_peer_conflicts() {
        let mut session = open();
        let mut journal = idtx_authoring::EditJournal::new();
        for value in [3.0, 4.0] {
            capture_property_edit(&mut journal, &session, "/World/Cube", "size").unwrap();
            session.author_attribute("/World/Cube", "size", idtx_usd::PropertyValue::F64(value)).unwrap();
        }
        finish_property_edit(&mut journal, &session, "/World/Cube", "size").unwrap();
        let after = session.snapshot_root_property("/World/Cube", "size").unwrap();
        journal.try_undo(|cmd| apply_edit_command_reverse(cmd, &mut session)).unwrap();
        assert!(session.snapshot_root_property("/World/Cube", "size").unwrap().is_empty());
        journal.try_redo(|cmd| apply_edit_command_forward(cmd, &mut session)).unwrap();
        assert_eq!(session.snapshot_root_property("/World/Cube", "size").unwrap(), after);
        session.author_attribute("/World/Cube", "size", idtx_usd::PropertyValue::F64(7.0)).unwrap();
        let peer = session.root_layer_snapshot().unwrap();
        assert!(journal.try_undo(|cmd| apply_edit_command_reverse(cmd, &mut session)).is_err());
        assert_eq!((journal.undo_depth(), journal.redo_depth()), (1, 0));
        assert_eq!(session.root_layer_snapshot().unwrap(), peer);
    }

    #[test]
    #[ignore = "requires OpenUSD and headless GPU"]
    fn transform_gesture_restores_absence_and_exact_matrix_without_reversing_peer_attributes() {
        let mut session = open();
        let mut journal = idtx_authoring::EditJournal::new();
        let before = session.root_transform_snapshot("/World/Cube").unwrap();
        capture_transform_edit(&mut journal, &session, "/World/Cube").unwrap();
        let matrix = [-1.0, 0.0, 0.0, 0.0, 0.25, 1.0, 0.0, 0.0, 0.5, 0.0, 1.0, 0.0, 1000000.123456789, 2.0, 3.0, 1.0];
        session.author_matrix_transform("/World/Cube", matrix).unwrap();
        session.author_attribute("/World/Cube", "size", idtx_usd::PropertyValue::F64(7.0)).unwrap();
        finish_transform_edit(&mut journal, &session, "/World/Cube").unwrap();
        let after = session.root_transform_snapshot("/World/Cube").unwrap();
        let peer = session.snapshot_root_property("/World/Cube", "size").unwrap();
        journal.try_undo(|cmd| apply_edit_command_reverse(cmd, &mut session)).unwrap();
        assert_eq!(session.root_transform_snapshot("/World/Cube").unwrap(), before);
        journal.try_redo(|cmd| apply_edit_command_forward(cmd, &mut session)).unwrap();
        assert_eq!(session.root_transform_snapshot("/World/Cube").unwrap(), after);
        assert_eq!(session.get_local_transform_f64("/World/Cube").unwrap(), matrix);
        assert_eq!(session.snapshot_root_property("/World/Cube", "size").unwrap(), peer);
    }

    #[test]
    #[ignore = "requires OpenUSD and headless GPU"]
    fn structural_conflicts_and_partial_reset_failure_preserve_stage_and_cursor() {
        let mut session = open();
        let mut journal = idtx_authoring::EditJournal::new();
        let before = session.root_layer_snapshot().unwrap();
        session.define_prim("/World/Created", "Xform").unwrap();
        let after = session.root_layer_snapshot().unwrap();
        journal.push(EditCommand::Structural { label: "Create".into(), before, after });
        session.define_prim("/World/Created/Peer", "Xform").unwrap();
        let peer = session.root_layer_snapshot().unwrap();
        assert!(journal.try_undo(|cmd| apply_edit_command_reverse(cmd, &mut session)).is_err());
        assert_eq!(session.root_layer_snapshot().unwrap(), peer);
        assert_eq!((journal.undo_depth(), journal.redo_depth()), (1, 0));
        session.author_attribute("/World/Cube", "size", idtx_usd::PropertyValue::F64(3.0)).unwrap();
        let captured = session.snapshot_root_property("/World/Cube", "size").unwrap();
        session.clear_property_opinion("/World/Cube", "size").unwrap();
        let before = session.root_layer_snapshot().unwrap();
        journal.push(EditCommand::ResetTransform { path: "/World/Cube".into(), snapshots: vec![("size".into(), captured), ("radius".into(), "invalid USDA".into())] });
        assert!(journal.try_undo(|cmd| apply_edit_command_reverse(cmd, &mut session)).is_err());
        assert_eq!(session.root_layer_snapshot().unwrap(), before, "partial replay must roll back");
        assert_eq!((journal.undo_depth(), journal.redo_depth()), (2, 0));
    }

    #[test]
    #[ignore = "requires OpenUSD and headless GPU"]
    fn variant_undo_restores_session_absence_without_changing_root_selection() {
        let mut session = open();
        let root = session.root_layer_snapshot().unwrap();
        let before = session.session_variant_opinion("/World/Cube", "shape").unwrap();
        assert_eq!(before, None);
        session.set_session_variant("/World/Cube", "shape", "large").unwrap();
        let command = EditCommand::SessionVariant { path: "/World/Cube".into(), set_name: "shape".into(), before, after: Some("large".into()) };
        apply_edit_command_reverse(&command, &mut session).unwrap();
        assert_eq!(session.session_variant_opinion("/World/Cube", "shape").unwrap(), None);
        assert_eq!(session.root_layer_snapshot().unwrap(), root);
        apply_edit_command_forward(&command, &mut session).unwrap();
        assert_eq!(session.session_variant_opinion("/World/Cube", "shape").unwrap().as_deref(), Some("large"));
    }

    #[test]
    #[ignore = "requires OpenUSD and headless GPU"]
    fn metadata_delta_restores_absence_preserves_peer_keys_and_refreshes_cached_mode() {
        let mut session = open();
        let mut journal = idtx_authoring::EditJournal::new();
        journal_root_edit(&mut journal, &mut session, "Shadow", |session| session.set_stage_shadow_mode(idtx_usd::ShadowMode::Hard)).unwrap();
        session.set_stage_behavior_run_scope(idtx_usd::StageBehaviorRunScope::Always).unwrap();
        journal.try_undo(|cmd| apply_edit_command_reverse(cmd, &mut session)).unwrap();
        let root = session.root_layer_snapshot().unwrap();
        assert!(!root.contains("idtx:shadowMode"));
        assert!(root.contains("idtx:stageBehaviorRunScope"));
        assert_eq!(session.stage_shadow_mode(), idtx_usd::ShadowMode::Off);
    }
}

/// [`EditSink`] impl that collects variant-selection changes as shell actions.
///
/// Created per-frame inside the egui closure; captured by the inspector panel.
pub(crate) struct InspectorEditSink<'a> {
    pub(crate) tab_id: u64,
    pub(crate) actions: &'a mut Vec<ShellAction>,
}

impl EditSink for InspectorEditSink<'_> {
    fn set_variant_selection(&mut self, path: &str, set_name: &str, before: &str, after: &str) {
        self.actions.push(ShellAction::SetVariantSelection {
            tab_id: self.tab_id,
            path: path.to_owned(),
            set_name: set_name.to_owned(),
            before: before.to_owned(),
            selection: after.to_owned(),
        });
    }

    #[cfg(feature = "authoring")]
    fn author_attribute(
        &mut self,
        path: &str,
        property: &str,
        before: Option<idtx_editor_model::AttributeValue>,
        value: idtx_editor_model::AttributeValue,
    ) {
        use idtx_editor_model::AttributeValue as AV;
        use idtx_usd::PropertyValue as PV;
        let convert = |av: AV| match av {
            AV::Bool(v) => PV::Bool(v),
            AV::F32(v) => PV::F32(v),
            AV::F64(v) => PV::F64(v),
            AV::String(v) => PV::String(v),
            AV::Token(v) => PV::Token(v),
            AV::TokenArray(v) => PV::TokenArray(v),
            AV::Color3f(v) => PV::Color3f(v),
            AV::Asset(v) => PV::Asset(v),
            AV::Int(v) => PV::Int(v),
        };
        let usd_before = before.map(convert);
        let usd_value = convert(value);
        self.actions.push(ShellAction::AuthorAttribute {
            tab_id: self.tab_id,
            path: path.to_owned(),
            property: property.to_owned(),
            before: usd_before,
            value: usd_value,
        });
    }

    #[cfg(feature = "authoring")]
    fn commit_attribute(
        &mut self,
        path: &str,
        property: &str,
        before: Option<idtx_editor_model::AttributeValue>,
        value: idtx_editor_model::AttributeValue,
    ) {
        use idtx_editor_model::AttributeValue as AV;
        use idtx_usd::PropertyValue as PV;
        let convert = |av: AV| match av {
            AV::Bool(v) => PV::Bool(v),
            AV::F32(v) => PV::F32(v),
            AV::F64(v) => PV::F64(v),
            AV::String(v) => PV::String(v),
            AV::Token(v) => PV::Token(v),
            AV::TokenArray(v) => PV::TokenArray(v),
            AV::Color3f(v) => PV::Color3f(v),
            AV::Asset(v) => PV::Asset(v),
            AV::Int(v) => PV::Int(v),
        };
        let usd_before = before.map(convert);
        let usd_value = convert(value);

        self.actions.push(ShellAction::CommitAttributeEdit {
            tab_id: self.tab_id,
            path: path.to_owned(),
            property: property.to_owned(),
            before: usd_before,
            value: usd_value,
        });
    }

    #[cfg(feature = "authoring")]
    fn author_transform(&mut self, path: &str, edit: idtx_editor_model::TransformEdit) {
        self.actions.push(ShellAction::AuthorTransform {
            tab_id: self.tab_id,
            path: path.to_owned(),
            translation: edit.translation,
            rotation_xyz_degrees: edit.rotation_xyz_degrees,
            scale: edit.scale,
        });
    }

    #[cfg(feature = "authoring")]
    fn commit_transform(
        &mut self,
        path: &str,
        before: [[f32; 4]; 4],
        after: idtx_editor_model::TransformEdit,
        before_trs: Option<idtx_editor_model::TransformEdit>,
    ) {
        self.actions.push(ShellAction::CommitTransformEdit {
            tab_id: self.tab_id,
            path: path.to_owned(),
            before,
            translation: after.translation,
            rotation_xyz_degrees: after.rotation_xyz_degrees,
            scale: after.scale,
            before_trs: before_trs.map(|trs| TransformTrs {
                translation: trs.translation,
                rotation_xyz_degrees: trs.rotation_xyz_degrees,
                scale: trs.scale,
            }),
        });
    }

    // ── Behavior attachment ──────────────────────────────────────────
    // These route to the shell; none of them play/run a behavior — playback
    // stays owned by the transport/session.
    #[cfg(feature = "cooking")]
    fn attach_behavior(&mut self, path: &str) {
        self.actions.push(ShellAction::AttachBehavior {
            tab_id: self.tab_id,
            path: path.to_owned(),
        });
    }

    #[cfg(feature = "cooking")]
    fn create_behavior(&mut self, path: &str) {
        self.actions.push(ShellAction::CreateBehavior {
            tab_id: self.tab_id,
            path: path.to_owned(),
        });
    }

    #[cfg(feature = "cooking")]
    fn overwrite_behavior(&mut self, path: &str) {
        self.actions.push(ShellAction::OverwriteBehavior {
            tab_id: self.tab_id,
            path: path.to_owned(),
        });
    }

    #[cfg(feature = "authoring")]
    fn detach_behavior(&mut self, path: &str) {
        self.actions.push(ShellAction::DetachBehavior {
            tab_id: self.tab_id,
            path: path.to_owned(),
        });
    }

    #[cfg(feature = "cooking")]
    fn edit_behavior_source(&mut self, path: &str) {
        self.actions.push(ShellAction::EditBehaviorSource {
            tab_id: self.tab_id,
            path: path.to_owned(),
        });
    }

    #[cfg(feature = "cooking")]
    fn check_behavior(&mut self, path: &str) {
        self.actions.push(ShellAction::CheckBehavior {
            tab_id: self.tab_id,
            path: path.to_owned(),
        });
    }

    #[cfg(feature = "cooking")]
    fn verify_behavior(&mut self, path: &str) {
        self.actions.push(ShellAction::VerifyBehavior {
            tab_id: self.tab_id,
            path: path.to_owned(),
        });
    }

    #[cfg(feature = "authoring")]
    fn set_behavior_enabled(&mut self, path: &str, enabled: bool) {
        self.actions.push(ShellAction::SetBehaviorEnabled {
            tab_id: self.tab_id,
            path: path.to_owned(),
            enabled,
        });
    }

    #[cfg(feature = "authoring")]
    fn set_behavior_kind(&mut self, path: &str, kind: &str) {
        self.actions.push(ShellAction::SetBehaviorKind {
            tab_id: self.tab_id,
            path: path.to_owned(),
            kind: kind.to_owned(),
        });
    }

    #[cfg(feature = "authoring")]
    fn set_behavior_scope(&mut self, path: &str, scope: &str) {
        self.actions.push(ShellAction::SetBehaviorScope {
            tab_id: self.tab_id,
            path: path.to_owned(),
            scope: scope.to_owned(),
        });
    }

    #[cfg(feature = "authoring")]
    fn set_behavior_server_authoritative(&mut self, path: &str, server_authoritative: bool) {
        self.actions
            .push(ShellAction::SetBehaviorServerAuthoritative {
                tab_id: self.tab_id,
                path: path.to_owned(),
                value: server_authoritative,
            });
    }

    #[cfg(feature = "authoring")]
    fn set_behavior_property(
        &mut self,
        path: &str,
        property: &str,
        value: idtx_editor_model::AttributeValue,
    ) {
        use idtx_editor_model::AttributeValue as AV;
        use idtx_usd::PropertyValue as PV;
        let value = match value {
            AV::Bool(v) => PV::Bool(v),
            AV::F32(v) => PV::F32(v),
            AV::F64(v) => PV::F64(v),
            AV::String(v) => PV::String(v),
            AV::Token(v) => PV::Token(v),
            AV::TokenArray(v) => PV::TokenArray(v),
            AV::Color3f(v) => PV::Color3f(v),
            AV::Asset(v) => PV::Asset(v),
            AV::Int(v) => PV::Int(v),
        };
        self.actions.push(ShellAction::SetBehaviorProperty {
            tab_id: self.tab_id,
            path: path.to_owned(),
            property: property.to_owned(),
            value,
        });
    }

    #[cfg(feature = "authoring")]
    fn apply_api_schema(&mut self, path: &str, schema: &str) {
        self.actions.push(ShellAction::ApplyApiSchema {
            tab_id: self.tab_id,
            path: path.to_owned(),
            schema: schema.to_owned(),
        });
    }

    #[cfg(feature = "authoring")]
    fn remove_api_schema(&mut self, path: &str, schema: &str) {
        self.actions.push(ShellAction::RemoveApiSchema {
            tab_id: self.tab_id,
            path: path.to_owned(),
            schema: schema.to_owned(),
        });
    }

    #[cfg(feature = "authoring")]
    fn apply_collision_api(&mut self, path: &str) {
        self.actions.push(ShellAction::AddCollisionApi {
            tab_id: self.tab_id,
            path: path.to_owned(),
        });
    }

    #[cfg(feature = "authoring")]
    fn remove_collision_api(&mut self, path: &str) {
        self.actions.push(ShellAction::RemoveCollisionApi {
            tab_id: self.tab_id,
            path: path.to_owned(),
        });
    }

    #[cfg(feature = "authoring")]
    fn generate_collision_hull(&mut self, path: &str, params: idtx_editor_model::HullGenParams) {
        self.actions.push(ShellAction::GenerateCollisionHull {
            tab_id: self.tab_id,
            path: path.to_owned(),
            params,
        });
    }

    #[cfg(feature = "authoring")]
    fn remove_property_opinion(&mut self, path: &str, property: &str) {
        self.actions.push(ShellAction::RemovePropertyOpinion {
            tab_id: self.tab_id,
            path: path.to_owned(),
            property: property.to_owned(),
        });
    }

    #[cfg(feature = "authoring")]
    fn reset_transform(&mut self, path: &str, properties: Vec<String>) {
        self.actions.push(ShellAction::ResetTransform {
            tab_id: self.tab_id,
            path: path.to_owned(),
            properties,
        });
    }
}

/// Apply the **undo** direction of `cmd` to `session`.
///
/// For `SetLocalTransform` this authors the `before` matrix back onto the stage.
#[cfg(feature = "authoring")]
pub(crate) fn apply_edit_command_reverse(
    cmd: &EditCommand,
    session: &mut idtx_studio_session::StudioSession,
) -> Result<(), idtx_studio_session::SessionError> {
    check_replay_target(cmd, session)?;
    match cmd {
        EditCommand::SetLocalTransform {
            path,
            before,
            before_trs,
            ..
        } => {
            // Prefer the authored Euler TRS for `rotateXYZ`-op prims so undo
            // restores the exact angles (e.g. `Y = 180°`); matrix prims and gizmo
            // drags carry `None` and fall back to decomposing the matrix.
            match before_trs {
                Some(trs) => session.author_transform(path, trs.translation, trs.rotation_xyz_degrees, trs.scale)?,
                None => session.author_matrix_transform(path, before.to_cols_array().map(f64::from))?,
            }
        }
        EditCommand::SetAttribute {
            path,
            property,
            before,
            ..
        } => {
            if let Some(before_value) = before {
                session.author_attribute(path, property, before_value.clone())?;
            } else { session.clear_property_opinion(path, property)?; }
        }
        EditCommand::RootProperty { path, property, before, after } => replay_property(session, path, property, after, before)?,
        EditCommand::RootTransform { path, before, after } => session.replay_root_transform(path, after, before)?,
        EditCommand::SessionVariant { path, set_name, before, after } => session.replay_session_variant(path, set_name, after.as_deref(), before.as_deref())?,
        EditCommand::SetVariantSelection {
            path,
            set_name,
            before,
            ..
        } => {
            session.set_session_variant(path, set_name, before)?;
        }
        // Undo "Create Opinion" by removing the empty override; undo "Remove
        // Opinion" by re-authoring it.
        EditCommand::CreateOverride { path } => {
            session.remove_opinion(path)?;
        }
        EditCommand::RemoveOverride { path } => {
            session.create_opinion(path)?;
        }
        // Undo a structural edit by restoring the root layer to its pre-edit
        // text. Structural edits have no cheap semantic inverse, so the whole
        // root layer is snapshotted and restored.
        EditCommand::Structural { before, after, .. } => {
            session.apply_root_layer_delta(after, before)?;
        }
        EditCommand::SetShadowMode { before, .. } => {
            session.set_stage_shadow_mode(*before)?;
        }
        // Undo of "Add Shadow Casting" removes the applied API schema.
        EditCommand::ApplyApiSchema { path, schema } => {
            session.remove_api_schema(path, schema)?;
        }
        // Undo of "Remove Shadow Casting" applies the removed API schema.
        EditCommand::RemoveApiSchema { path, schema } => {
            session.apply_api_schema(path, schema)?;
        }
        // Undo "Remove Opinion" by restoring the captured root-layer spec.
        EditCommand::ClearProperty {
            path,
            property,
            snapshot,
        } => {
            replay_property(session, path, property, "", snapshot)?;
        }
        // Undo "Reset Transform" by restoring every removed transform opinion.
        EditCommand::ResetTransform { path, snapshots } => {
            replay_transform_reset(session, path, snapshots, true)?;
        }
    }
    Ok(())
}

/// Apply the **redo** direction of `cmd` to `session`.
///
/// For `SetLocalTransform` this re-authors the `after` matrix.
#[cfg(feature = "authoring")]
pub(crate) fn apply_edit_command_forward(
    cmd: &EditCommand,
    session: &mut idtx_studio_session::StudioSession,
) -> Result<(), idtx_studio_session::SessionError> {
    check_replay_target(cmd, session)?;
    match cmd {
        EditCommand::SetLocalTransform {
            path,
            after,
            after_trs,
            ..
        } => {
            match after_trs {
                Some(trs) => session.author_transform(path, trs.translation, trs.rotation_xyz_degrees, trs.scale)?,
                None => session.author_matrix_transform(path, after.to_cols_array().map(f64::from))?,
            }
        }
        EditCommand::SetAttribute {
            path,
            property,
            after,
            ..
        } => {
            session.author_attribute(path, property, after.clone())?;
        }
        EditCommand::RootProperty { path, property, before, after } => replay_property(session, path, property, before, after)?,
        EditCommand::RootTransform { path, before, after } => session.replay_root_transform(path, before, after)?,
        EditCommand::SessionVariant { path, set_name, before, after } => session.replay_session_variant(path, set_name, before.as_deref(), after.as_deref())?,
        EditCommand::SetVariantSelection {
            path,
            set_name,
            after,
            ..
        } => {
            session.set_session_variant(path, set_name, after)?;
        }
        // Redo re-applies the original direction.
        EditCommand::CreateOverride { path } => {
            session.create_opinion(path)?;
        }
        EditCommand::RemoveOverride { path } => {
            session.remove_opinion(path)?;
        }
        // Redo a structural edit by restoring the root layer to its post-edit
        // text.
        EditCommand::Structural { before, after, .. } => {
            session.apply_root_layer_delta(before, after)?;
        }
        EditCommand::SetShadowMode { after, .. } => {
            session.set_stage_shadow_mode(*after)?;
        }
        // Redo re-applies the API schema.
        EditCommand::ApplyApiSchema { path, schema } => {
            session.apply_api_schema(path, schema)?;
        }
        // Redo removes the API schema.
        EditCommand::RemoveApiSchema { path, schema } => {
            session.remove_api_schema(path, schema)?;
        }
        // Redo "Remove Opinion" by clearing the root-layer opinion again.
        EditCommand::ClearProperty { path, property, snapshot } => {
            replay_property(session, path, property, snapshot, "")?;
        }
        // Redo "Reset Transform" by clearing every transform opinion again.
        EditCommand::ResetTransform { path, snapshots } => {
            replay_transform_reset(session, path, snapshots, false)?;
        }
    }
    Ok(())
}

#[cfg(feature = "authoring")]
fn replay_property(session: &mut idtx_studio_session::StudioSession, path: &str, property: &str,
    expected: &str, desired: &str) -> Result<(), idtx_studio_session::SessionError> {
    if session.stage().prim_at_path(path).is_none() || session.snapshot_root_property(path, property)? != expected {
        return Err(idtx_usd::StageError::OpenUsd(format!("undo/redo conflict at {path}.{property}")).into());
    }
    if desired.is_empty() { session.clear_property_opinion(path, property) }
    else { session.restore_property_opinion(path, property, desired) }
}

#[cfg(feature = "authoring")]
fn check_replay_target(cmd: &EditCommand, session: &idtx_studio_session::StudioSession) -> Result<(), idtx_studio_session::SessionError> {
    let path = cmd.target_path();
    if !path.is_empty() && path != "/" && session.stage().prim_at_path(path).is_none() {
        return Err(idtx_usd::StageError::UnknownPath(path.into()).into());
    }
    Ok(())
}

#[cfg(feature = "authoring")]
fn replay_transform_reset(session: &mut idtx_studio_session::StudioSession, path: &str,
    snapshots: &[(String, String)], reverse: bool) -> Result<(), idtx_studio_session::SessionError> {
    let before = session.root_layer_snapshot()?;
    for (property, snapshot) in snapshots {
        let (expected, desired) = if reverse { ("", snapshot.as_str()) } else { (snapshot.as_str(), "") };
        if let Err(error) = replay_property(session, path, property, expected, desired) {
            session.restore_root_layer_snapshot(&before)?;
            return Err(error);
        }
    }
    Ok(())
}
