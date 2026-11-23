//! Routing and fresh-projection regressions. Run the GPU cases with the USD
//! environment configured and `cargo test -p idtx-studio-session --features
//! authoring --lib invalidation -- --include-ignored`.
use super::*;

fn prim(slot: u32) -> PrimId {
    PrimId::from_raw_parts(slot, idtx_usd::StageGeneration::from_raw(1))
}

fn change(path: &str, domains: DirtyDomains) -> ChangedPrim {
    ChangedPrim {
        // Deliberately share a routed identity: an unknown spawn resolves to
        // the same ancestor ID as a genuine notice on that ancestor.
        prim: prim(1),
        notice_path: path.into(),
        property_name: None,
        resync: domains.contains(DirtyDomains::TOPOLOGY),
        domains,
        authored_in_root_layer: false,
    }
}

#[test]
fn exact_spawn_scope_does_not_discard_ancestor_notice() {
    let summary = BatchInvalidation::from_changes(&[
        change("/World/New", DirtyDomains::TOPOLOGY),
        change("/World", DirtyDomains::TOPOLOGY),
    ]);
    assert_eq!(summary.structural_paths, ["/World", "/World/New"]);
    assert!(summary.can_scope());
    let spawn = BatchInvalidation::from_changes(&[change("/World/New", DirtyDomains::TOPOLOGY)]);
    assert_eq!(spawn.structural_paths, ["/World/New"]);
    assert!(spawn.can_splice(), "a spawn alone must stay scoped");
}

#[test]
fn every_uncovered_value_domain_requires_full_reconciliation() {
    for domain in [
        DirtyDomains::TRANSFORM,
        DirtyDomains::GEOMETRY,
        DirtyDomains::VISIBILITY,
        DirtyDomains::METADATA,
        DirtyDomains::MATERIAL,
        DirtyDomains::GENERIC,
        DirtyDomains::ANIMATION,
        DirtyDomains::RELATIONSHIP,
    ] {
        let summary = BatchInvalidation::from_changes(&[
            change("/World/New", DirtyDomains::TOPOLOGY),
            change("/World/Box", domain),
        ]);
        assert_eq!(summary.value_roots[0].domains, domain);
        assert!(!summary.can_scope(), "lost value domain {domain:?}");
        assert!(!summary.can_splice());
    }
}

#[test]
fn covered_values_are_scoped_but_material_consumers_are_ambiguous() {
    let summary = |path, domain| {
        BatchInvalidation::from_changes(&[
            change("/World/New", DirtyDomains::TOPOLOGY),
            change(path, domain),
        ])
    };
    assert!(summary("/World/New/Child", DirtyDomains::TRANSFORM).can_scope());
    assert!(!summary("/World/NewSibling", DirtyDomains::TRANSFORM).can_scope());
    assert!(!summary("/World/New/Material", DirtyDomains::MATERIAL).can_scope());
    assert!(!summary("/World/New/Link", DirtyDomains::RELATIONSHIP).can_scope());
    assert!(!summary("/World/New/Unknown", DirtyDomains::UNKNOWN).can_scope());
}

#[test]
fn nonlocal_domains_folded_into_resyncs_require_consumer_reconciliation() {
    for domain in [
        DirtyDomains::MATERIAL,
        DirtyDomains::RELATIONSHIP,
        DirtyDomains::UNKNOWN,
    ] {
        let summary = BatchInvalidation::from_changes(&[change(
            "/World/New",
            DirtyDomains::TOPOLOGY
                .union(DirtyDomains::SKELETON)
                .union(domain),
        )]);
        assert!(
            summary.value_roots.is_empty(),
            "USD folds covered values into resyncs"
        );
        assert!(!summary.can_scope());
        assert!(!summary.can_splice());
    }
    let spawn = BatchInvalidation::from_changes(&[change(
        "/World/New",
        DirtyDomains::TOPOLOGY.union(DirtyDomains::SKELETON),
    )]);
    assert!(
        spawn.can_splice(),
        "the generic topology/skeleton bits must not disable every spawn"
    );
}

#[test]
fn removed_identities_and_stage_root_preclude_splice() {
    let mut summary =
        BatchInvalidation::from_changes(&[change("/World/Old", DirtyDomains::TOPOLOGY)]);
    summary.removed.insert(prim(2));
    assert!(!summary.can_splice());
    assert!(!BatchInvalidation::from_changes(&[change("/", DirtyDomains::TOPOLOGY)]).can_scope());
}

#[test]
fn pending_splices_union_until_consumed() {
    let mut pending = None;
    merge_topology_splice_into(&mut pending, &HashSet::from([prim(1), prim(2)]));
    merge_topology_splice_into(&mut pending, &HashSet::from([prim(2), prim(3)]));
    assert_eq!(
        pending.take().unwrap(),
        HashSet::from([prim(1), prim(2), prim(3)])
    );
    merge_topology_splice_into(&mut pending, &HashSet::from([prim(4)]));
    assert_eq!(pending.unwrap(), HashSet::from([prim(4)]));
}

#[cfg(feature = "authoring")]
mod projection {
    use super::*;
    use crate::*;
    use idtx_scene::{BoundsComp, CubeComp, VisibilityComp, WorldTransform};

    fn open(fixture: &str) -> StudioSession {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/usd")
            .join(fixture);
        // Root-value authoring must not mutate a shared fixture layer in USD's
        // process-global cache (or its on-disk file).
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let isolated = std::env::temp_dir().join(format!(
            "idtx-invalidation-{}-{}.usda",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::copy(path, &isolated).unwrap();
        let (device, queue) = pollster::block_on(async {
            let instance =
                wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
            let adapter = instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    compatible_surface: None,
                    force_fallback_adapter: false,
                    power_preference: wgpu::PowerPreference::HighPerformance,
                })
                .await
                .unwrap();
            adapter
                .request_device(&wgpu::DeviceDescriptor::default())
                .await
                .unwrap()
        });
        let session = StudioSession::open_with_gpu(
            &isolated,
            device,
            queue,
            [64, 64],
            TextureDecodeConfig::FULL_RESOLUTION,
            RenderWorldUnitScale::METRE,
        )
        .unwrap();
        std::fs::remove_file(isolated).unwrap();
        session
    }

    fn edit(session: &StudioSession, path: &str, op: SessionEditOp) -> SessionEditCommand {
        SessionEditCommand {
            target: idtx_behavior::entity_to_wire(session.hydrator.entity_for_path(path).unwrap()),
            op,
        }
    }

    fn spawn(session: &StudioSession, name: &str) -> SessionEditCommand {
        edit(
            session,
            "/World",
            SessionEditOp::DefinePrim {
                type_name: "Cube".into(),
                name: name.into(),
            },
        )
    }

    fn pose(x: f32) -> SessionEditOp {
        SessionEditOp::SetTransform(idtx_behavior_abi::Xform::from_trs(
            Vec3::new(x, 5.0, 0.0),
            glam::Quat::IDENTITY,
            Vec3::ONE,
        ))
    }

    /// Compare incremental ECS and the prepared warm snapshot against a fresh
    /// full projection from live USD, not a re-extract of the incremental ECS.
    fn assert_fresh(session: &mut StudioSession) {
        session.prepare_presentation(1.0);
        let delta = session.service.project_topology_paths(&[]).unwrap();
        let projection = session.service.project_viewport().unwrap();
        let mut fresh = SceneHydrator::new(PopulatorRegistry::new());
        fresh.apply_topology_delta(&delta, &session.service);
        fresh.apply_viewport_projection(&projection).unwrap();
        fresh.initialize_visibility_overrides(&session.service);
        let paths = |hydrator: &SceneHydrator| {
            hydrator
                .world()
                .iter_entities()
                .filter_map(|e| e.get::<PrimNode>().map(|node| node.path.clone()))
                .collect::<HashSet<_>>()
        };
        assert_eq!(paths(&session.hydrator), paths(&fresh));
        for entity in fresh.world().iter_entities() {
            let Some(node) = entity.get::<PrimNode>() else {
                continue;
            };
            let actual = session.hydrator.entity_for_path(&node.path).unwrap();
            let world = session.hydrator.world();
            assert_eq!(
                world.get::<WorldTransform>(actual).map(|c| c.matrix),
                entity.get::<WorldTransform>().map(|c| c.matrix),
                "transform {}",
                node.path
            );
            assert_eq!(
                world.get::<CubeComp>(actual).map(|c| c.size),
                entity.get::<CubeComp>().map(|c| c.size),
                "geometry {}",
                node.path
            );
            assert_eq!(
                world
                    .get::<BoundsComp>(actual)
                    .map(|c| (c.minimum, c.maximum)),
                entity.get::<BoundsComp>().map(|c| (c.minimum, c.maximum)),
                "bounds {}",
                node.path
            );
            assert_eq!(
                world
                    .get::<VisibilityComp>(actual)
                    .map(|c| (c.visible, c.purpose)),
                entity
                    .get::<VisibilityComp>()
                    .map(|c| (c.visible, c.purpose)),
                "visibility {}",
                node.path
            );
            if !node.is_pseudo_root {
                let context = session
                    .service
                    .authored_string(node.prim, "idtx:behavior:context", 65536)
                    .unwrap();
                assert_eq!(
                    session
                        .runtime_behavior_data(&node.path)
                        .and_then(|data| data.context.clone()),
                    context,
                    "metadata {}",
                    node.path
                );
            }
        }
        let expected = RenderWorldSnapshot::extract_with_shadow(
            fresh.world_mut(),
            1.0,
            session.projection_basis,
            session.resolved_shadow_settings(),
        );
        let actual = session.last_snapshot.as_ref().unwrap();
        assert_eq!(actual.meshes.len(), expected.meshes.len());
        for mesh in &expected.meshes {
            let actual = actual
                .meshes
                .iter()
                .find(|m| m.prim_path == mesh.prim_path)
                .unwrap();
            assert_eq!(
                actual.world_matrix, mesh.world_matrix,
                "render transform {}",
                mesh.prim_path
            );
            assert_eq!(
                actual.mesh.positions, mesh.mesh.positions,
                "render geometry {}",
                mesh.prim_path
            );
            assert_eq!(actual.mesh.indices, mesh.mesh.indices);
            assert_eq!(actual.material.base_color, mesh.material.base_color);
            assert_eq!(
                actual.material.roughness, mesh.material.roughness,
                "render material roughness {}",
                mesh.prim_path
            );
        }
        assert_eq!(actual.materials.len(), expected.materials.len());
        for (path, material) in &expected.materials {
            assert_eq!(
                actual.materials[path].roughness, material.roughness,
                "material {path}"
            );
        }
    }

    #[test]
    #[ignore = "requires OpenUSD link + headless GPU"]
    fn spawn_plus_transform_matches_fresh_projection() {
        let mut session = open("physics/box_fall_plane.usda");
        session.apply_session_edits(&[edit(&session, "/World/Box", pose(1.0))]);
        session.prepare_presentation(1.0);
        session.apply_session_edits(&[
            spawn(&session, "New"),
            edit(&session, "/World/Box", pose(7.0)),
        ]);
        assert_eq!(
            session.get_world_transform_f64("/World/Box").unwrap()[12],
            7.0
        );
        assert!(
            session.snapshot_dirty,
            "mixed batch must not claim a spawn-only splice"
        );
        assert_fresh(&mut session);
    }

    #[test]
    #[ignore = "requires OpenUSD link + headless GPU"]
    fn spawn_plus_geometry_visibility_or_metadata_matches_fresh_projection() {
        for domain in ["geometry", "visibility", "metadata"] {
            let mut session = open("physics/box_fall_plane.usda");
            if domain == "visibility" {
                session.apply_session_edits(&[edit(
                    &session,
                    "/World/Box",
                    SessionEditOp::SetVisibility(true),
                )]);
            } else if domain == "metadata" {
                session
                    .service
                    .author_session_property(
                        "/World/Box",
                        "idtx:behavior:context",
                        &idtx_usd::PropertyValue::String("old".into()),
                    )
                    .unwrap();
                session.tick().unwrap();
            }
            session.prepare_presentation(1.0);
            let generation = session.visibility_generation;
            match domain {
                "geometry" => session
                    .author_attribute("/World/Box", "size", idtx_usd::PropertyValue::F64(3.0))
                    .unwrap(),
                "metadata" => session
                    .service
                    .author_session_property(
                        "/World/Box",
                        "idtx:behavior:context",
                        &idtx_usd::PropertyValue::String("new".into()),
                    )
                    .unwrap(),
                _ => (),
            }
            let mut edits = vec![spawn(&session, "New")];
            if domain == "visibility" {
                edits.push(edit(
                    &session,
                    "/World/Box",
                    SessionEditOp::SetVisibility(false),
                ));
            }
            session.apply_session_edits(&edits);
            if domain == "visibility" {
                assert!(session.visibility_generation > generation);
            }
            assert_fresh(&mut session);
        }
    }

    #[test]
    #[ignore = "requires OpenUSD link + headless GPU"]
    fn spawn_plus_material_matches_fresh_projection() {
        let mut session = open("materials/preview_surface.usda");
        session.prepare_presentation(1.0);
        session
            .service
            .author_session_property(
                "/World/Looks/Blue/Surface",
                "inputs:roughness",
                &idtx_usd::PropertyValue::F32(0.9),
            )
            .unwrap();
        session.apply_session_edits(&[spawn(&session, "New")]);
        assert_fresh(&mut session);
    }

    #[test]
    #[ignore = "requires OpenUSD link + headless GPU"]
    fn material_variant_resync_plus_spawn_updates_consumers_outside_scope() {
        let mut session = open("materials/preview_surface.usda");
        let text = include_str!("../../../../tests/fixtures/usd/materials/preview_surface.usda")
            .replace(
                "def Material \"Blue\"",
                "def Material \"Blue\" (\n    variants = { string finish = \"smooth\" }\n    prepend variantSets = \"finish\"\n)",
            )
            .replace("float inputs:roughness = 0.35", "")
            .replace(
                "token outputs:surface.connect = </World/Looks/Blue/Surface.outputs:surface>",
                r#"token outputs:surface.connect = </World/Looks/Blue/Surface.outputs:surface>
                variantSet "finish" = {
                    "smooth" {
                        over "Surface" {
                            float inputs:roughness = 0.35
                        }
                    }
                    "rough" {
                        over "Surface" {
                            float inputs:roughness = 0.9
                        }
                    }
                }"#,
            );
        session.service.replace_root_layer_text(&text).unwrap();
        session.tick().unwrap();
        // Seed the session-layer ancestors before the warm projection. Otherwise
        // the first selection creates /World and accidentally broadens the scope
        // to include every material consumer, hiding the incremental failure.
        session
            .set_session_variant("/World/Looks/Blue", "finish", "smooth")
            .unwrap();
        session.tick().unwrap();
        session.prepare_presentation(1.0);
        session
            .set_session_variant("/World/Looks/Blue", "finish", "rough")
            .unwrap();
        session.apply_session_edits(&[spawn(&session, "New")]);
        assert!(
            session.snapshot_dirty,
            "shading resync must update consumers outside its namespace"
        );
        assert_fresh(&mut session);
    }

    #[test]
    #[ignore = "requires OpenUSD link + headless GPU"]
    fn shader_removal_plus_spawn_updates_external_material_consumers() {
        let mut session = open("materials/preview_surface.usda");
        session.apply_session_edits(&[spawn(&session, "Seed")]);
        session.prepare_presentation(1.0);
        session.apply_session_edits(&[
            spawn(&session, "New"),
            edit(
                &session,
                "/World/Looks/Blue/Surface",
                SessionEditOp::RemovePrim,
            ),
        ]);
        assert!(session.snapshot_dirty);
        assert_fresh(&mut session);
    }

    #[test]
    #[ignore = "requires OpenUSD link + headless GPU"]
    fn additive_reference_with_its_own_materials_keeps_the_scoped_splice() {
        let mut session = open("physics/box_fall_plane.usda");
        session.apply_session_edits(&[spawn(&session, "Seed")]);
        session.prepare_presentation(1.0);
        let serial = session.last_snapshot.as_ref().unwrap().serial;
        let asset = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/usd/materials/preview_surface.usda");
        session
            .service
            .spawn_session_reference(
                "/World/Prefab",
                asset.to_str().unwrap(),
                "/World",
                Mat4::IDENTITY.to_cols_array().map(f64::from),
                "{}",
                &[],
            )
            .unwrap();
        session.tick().unwrap();
        assert!(
            !session.snapshot_dirty,
            "new local materials must not make every reference spawn broad"
        );
        assert!(session.snapshot_topology_splice.is_some());
        assert_fresh(&mut session);
        assert_eq!(session.last_snapshot.as_ref().unwrap().serial, serial);
    }

    #[test]
    #[ignore = "requires OpenUSD link + headless GPU"]
    fn ancestor_variant_switch_plus_spawn_keeps_resync_and_removals() {
        let mut session = open("scene/variants.usda");
        session.prepare_presentation(1.0);
        session
            .set_session_variant("/World", "shape", "Sphere")
            .unwrap();
        session.apply_session_edits(&[spawn(&session, "First")]);
        assert!(
            session
                .hydrator
                .entity_for_path("/World/Geometry")
                .is_some()
        );
        assert_fresh(&mut session);
        session
            .set_session_variant("/World", "shape", "Cube")
            .unwrap();
        session.apply_session_edits(&[spawn(&session, "Second")]);
        assert!(
            session
                .hydrator
                .entity_for_path("/World/Geometry")
                .is_none()
        );
        assert!(
            session.snapshot_topology_splice.is_none(),
            "removal requires full extraction"
        );
        assert_fresh(&mut session);
    }

    #[test]
    #[ignore = "requires OpenUSD link + headless GPU"]
    fn two_spawns_before_presentation_keep_scopes_and_transform_patches() {
        let mut session = open("physics/box_fall_plane.usda");
        session.apply_session_edits(&[edit(&session, "/World/Box", pose(1.0))]);
        session.prepare_presentation(1.0);
        let serial = session.last_snapshot.as_ref().unwrap().serial;
        session.apply_session_edits(&[edit(&session, "/World/Box", pose(7.0))]);
        for name in ["First", "Second"] {
            let before = session.service.metrics().topology.records;
            session.apply_session_edits(&[spawn(&session, name)]);
            assert_eq!(
                session.service.metrics().topology.records - before,
                1,
                "spawn traversed unrelated topology"
            );
            assert!(!session.snapshot_dirty, "spawn-only batch must stay scoped");
        }
        assert_eq!(session.snapshot_topology_splice.as_ref().unwrap().len(), 2);
        assert!(
            session
                .snapshot_transform_patches
                .as_ref()
                .unwrap()
                .iter()
                .any(|(p, m)| p == "/World/Box" && m[12] == 7.0)
        );
        assert_fresh(&mut session);
        assert_eq!(session.last_snapshot.as_ref().unwrap().serial, serial);
    }

    #[test]
    #[ignore = "requires OpenUSD link + headless GPU"]
    fn topology_projection_supersedes_an_earlier_pending_pose() {
        let mut session = open("scene/variants.usda");
        session.apply_session_edits(&[edit(&session, "/World", pose(1.0))]);
        session.prepare_presentation(1.0);
        session.apply_session_edits(&[edit(&session, "/World", pose(7.0))]);
        session
            .service
            .author_session_transform(
                "/World",
                Mat4::from_translation(Vec3::new(9.0, 5.0, 0.0))
                    .to_cols_array()
                    .map(f64::from),
            )
            .unwrap();
        session
            .set_session_variant("/World", "shape", "Sphere")
            .unwrap();
        session.apply_session_edits(&[spawn(&session, "New")]);
        assert!(
            !session.snapshot_dirty,
            "all values are covered by the ancestor scope"
        );
        assert!(
            session
                .snapshot_transform_patches
                .as_ref()
                .unwrap()
                .iter()
                .any(|(path, matrix)| path == "/World" && matrix[12] == 9.0)
        );
        assert_fresh(&mut session);
    }
}
