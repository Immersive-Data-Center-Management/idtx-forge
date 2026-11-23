#![allow(clippy::float_cmp)] // exact-value assertions on known constants

use super::instancing::promote_topology_instances_from_groups;
use super::light::{caster_light_size, caster_shadow_fit, light_point_intensity};
use super::*;

use std::collections::HashMap;
use std::sync::Arc;

use bevy_ecs::prelude::*;
use glam::{Mat3, Mat4};
use idtx_scene::hydrator::mat4_from_usd_row_major;
use idtx_scene::{CameraComp, PrimNode, SkeletonPoseComp, SkinBindingComp};
use idtx_usd::{
    DomeTextureFormat, RenderPurpose, SkeletonPoseRecord, SkinBindingRecord, SkinningExecution,
};

use crate::mesh::{GpuMeshDesc, SkinData};
use crate::vertex::SkinVertex;

/// A `CameraComp` fixture with the given lens/clip params and no physical
/// exposure (matches the Demo1 player-rig camera defaults otherwise).
fn camera_comp(focal_length: f64, horizontal_aperture: f64, clipping: [f64; 2]) -> CameraComp {
    use idtx_usd::{PrimId, StageGeneration};
    CameraComp {
        record: idtx_usd::CameraRecord {
            prim: PrimId::from_raw_parts(0, StageGeneration::from_raw(1)),
            projection: "perspective".to_owned(),
            focal_length,
            horizontal_aperture,
            vertical_aperture: 24.0,
            horizontal_aperture_offset: 0.0,
            vertical_aperture_offset: 0.0,
            clipping,
            clipping_planes: Vec::new(),
            f_stop: 0.0,
            focus_distance: 0.0,
            stereo_role: String::new(),
            shutter: [0.0, 0.0],
            exposure: 0.0,
            exposure_iso: 0.0,
            exposure_time: 0.0,
            exposure_f_stop: 0.0,
            exposure_responsivity: 0.0,
        },
    }
}

#[test]
fn from_scene_camera_matches_inlined_math() {
    let aspect = 16.0 / 9.0_f32;
    let cam = camera_comp(20.784_61, 36.0, [0.05, 10_000.0]);
    let desc = RenderCameraDesc::from_scene_camera(
        "/World/Player/Head/Camera".to_owned(),
        &cam,
        Mat4::IDENTITY,
        Mat4::IDENTITY,
        aspect,
    );

    // Identity world × identity basis → identity view.
    assert_eq!(desc.view, Mat4::IDENTITY);
    assert_eq!(desc.prim_path, "/World/Player/Head/Camera");
    // Authored clip range is honoured.
    assert!((desc.clip_near - 0.05).abs() < 1e-6);
    assert!((desc.clip_far - 10_000.0).abs() < 1e-2);
    // Horizontal FOV = 2·atan((hAperture/2)/focal).
    let expected_fov_h = 2.0 * ((36.0_f32 / 2.0) / 20.784_61).atan();
    assert!((desc.fov_horizontal - expected_fov_h).abs() < 1e-5);
    // No physical exposure authored → linear (0.0).
    assert_eq!(desc.exposure_ev, 0.0);
}

#[test]
fn from_scene_camera_falls_back_on_bad_clip_range() {
    // near == far (degenerate) → the (0.1, 10_000.0) fallback.
    let cam = camera_comp(20.784_61, 36.0, [0.0, 0.0]);
    let desc = RenderCameraDesc::from_scene_camera(
        String::new(),
        &cam,
        Mat4::IDENTITY,
        Mat4::IDENTITY,
        1.0,
    );
    assert!((desc.clip_near - 0.1).abs() < 1e-6);
    assert!((desc.clip_far - 10_000.0).abs() < 1e-2);
}

#[test]
fn from_scene_camera_computes_physical_ev100() {
    let mut cam = camera_comp(20.784_61, 36.0, [0.05, 10_000.0]);
    // f/8, 1/125s, ISO 100 → EV100 = log2(N²·ISO / (100·t)).
    cam.record.exposure_f_stop = 8.0;
    cam.record.exposure_time = 1.0 / 125.0;
    cam.record.exposure_iso = 100.0;
    let desc = RenderCameraDesc::from_scene_camera(
        String::new(),
        &cam,
        Mat4::IDENTITY,
        Mat4::IDENTITY,
        1.0,
    );
    let expected = (8.0_f32 * 8.0 * 100.0 / (100.0 * (1.0 / 125.0))).log2();
    assert!((desc.exposure_ev - expected).abs() < 1e-4);
}

/// else a non-caster (no `shadow_view_proj`). `power` drives importance.
fn alloc_light(path: &str, power: f32, cast: bool) -> RenderLightDesc {
    RenderLightDesc {
        prim_path: path.to_owned(),
        kind: RenderLightKind::Distant {
            angle_radians: 0.01,
        },
        world_matrix: Mat4::IDENTITY,
        color: [1.0, 1.0, 1.0],
        power,
        diffuse_contribution: 1.0,
        specular_contribution: 1.0,
        shadow_enabled: cast,
        cone_angle: None,
        cone_softness: 0.0,
        shadow_view_proj: cast.then_some(Mat4::IDENTITY),
        shadow_near: if cast { 1.0 } else { 0.0 },
        shadow_far: if cast { 3.0 } else { 0.0 },
        light_size: if cast { 0.01 } else { 0.0 },
        shadow_slot: None,
        dome_texture_path: None,
        dome_texture_format: DomeTextureFormat::default(),
        env_rotation: Mat3::IDENTITY,
        ambient: [0.0; 3],
    }
}

#[test]
fn allocator_assigns_distinct_slots_within_budget() {
    // Three casters ≤ budget → each gets a distinct slot, none dropped.
    let mut lights = vec![
        alloc_light("/A", 10.0, true),
        alloc_light("/B", 20.0, true),
        alloc_light("/C", 30.0, true),
    ];
    let slots = ShadowAtlasAllocator::allocate(&mut lights);
    assert_eq!(slots.len(), 3, "all three casters fit in the budget");
    let slot_ids: std::collections::HashSet<u32> = slots.iter().map(|s| s.slot).collect();
    assert_eq!(slot_ids.len(), 3, "slots must be distinct");
    assert!(slot_ids.iter().all(|&s| (s as usize) < SHADOW_BUDGET));
    // Every caster light received a slot.
    assert!(lights.iter().all(|l| l.shadow_slot.is_some()));
    // Highest importance (power 30, "/C") ranks into slot 0.
    assert_eq!(slots[0].slot, 0);
    assert_eq!(
        slots[0].light_index, 2,
        "/C is lights[2] and most important"
    );
}

#[test]
fn allocator_drops_least_important_on_overflow() {
    // Five casters > SHADOW_BUDGET(4): exactly the four brightest keep a slot;
    // the dimmest is dropped deterministically.
    let mut lights = vec![
        alloc_light("/dim", 1.0, true),
        alloc_light("/a", 50.0, true),
        alloc_light("/b", 40.0, true),
        alloc_light("/c", 30.0, true),
        alloc_light("/d", 20.0, true),
    ];
    let slots = ShadowAtlasAllocator::allocate(&mut lights);
    assert_eq!(slots.len(), SHADOW_BUDGET, "exactly the budget is filled");
    // The dimmest ("/dim", lights[0]) is the overflow loser.
    assert_eq!(
        lights[0].shadow_slot, None,
        "least-important caster is dropped"
    );
    assert!(
        lights[1..].iter().all(|l| l.shadow_slot.is_some()),
        "the four brightest all keep a slot"
    );
    // Rank order: brightest → slot 0.
    assert_eq!(slots[0].light_index, 1, "/a (power 50) is slot 0");
}

#[test]
fn allocator_ignores_non_casters() {
    // Non-caster lights (no shadow_view_proj) never receive a slot.
    let mut lights = vec![
        alloc_light("/caster", 10.0, true),
        alloc_light("/fill", 100.0, false),
    ];
    let slots = ShadowAtlasAllocator::allocate(&mut lights);
    assert_eq!(slots.len(), 1, "only the caster is allocated");
    assert_eq!(slots[0].light_index, 0);
    assert_eq!(
        lights[1].shadow_slot, None,
        "the bright fill light casts no shadow"
    );
}

#[test]
fn allocator_slot_identity_is_stable_across_frames() {
    // The same light set must yield the same light→slot mapping every frame
    // (the Phase-6 ShadowCache depends on this). Rebuild the identical set and
    // re-run: each prim path maps to the same slot.
    let build = || {
        vec![
            alloc_light("/key", 100.0, true),
            alloc_light("/rim", 50.0, true),
            alloc_light("/back", 75.0, true),
        ]
    };
    let mut frame_a = build();
    let mut frame_b = build();
    let slots_a = ShadowAtlasAllocator::allocate(&mut frame_a);
    let slots_b = ShadowAtlasAllocator::allocate(&mut frame_b);
    assert_eq!(
        slots_a, slots_b,
        "identical light sets allocate identically"
    );
    for (a, b) in frame_a.iter().zip(frame_b.iter()) {
        assert_eq!(
            a.shadow_slot, b.shadow_slot,
            "{} slot must be stable",
            a.prim_path
        );
    }
}

#[test]
fn light_point_intensity_folds_area_and_normalize() {
    use std::f32::consts::PI;
    let base = 10.0_f32;
    let r = 0.5_f32;

    // Sphere: non-normalized scales by emitter area π·r²; normalized → base/4.
    let sphere = RenderLightKind::Sphere { radius: r };
    assert!((light_point_intensity(&sphere, base, false) - base * PI * r * r).abs() < 1e-5);
    assert!((light_point_intensity(&sphere, base, true) - base * 0.25).abs() < 1e-5);

    // Disk: non-normalized scales by π·r²; normalized passes base through.
    let disk = RenderLightKind::Disk { radius: r };
    assert!((light_point_intensity(&disk, base, false) - base * PI * r * r).abs() < 1e-5);
    assert!((light_point_intensity(&disk, base, true) - base).abs() < 1e-5);

    // Rect: non-normalized scales by w·h; normalized passes base through.
    let rect = RenderLightKind::Rect {
        width: 2.0,
        height: 3.0,
    };
    assert!((light_point_intensity(&rect, base, false) - base * 6.0).abs() < 1e-5);
    assert!((light_point_intensity(&rect, base, true) - base).abs() < 1e-5);

    // Cylinder: non-normalized scales by the lateral area 2π·r·l; normalized
    // passes base through.
    let cyl = RenderLightKind::Cylinder {
        radius: r,
        length: 3.0,
    };
    assert!(
        (light_point_intensity(&cyl, base, false) - base * 2.0 * PI * r * 3.0).abs() < 1e-4
    );
    assert!((light_point_intensity(&cyl, base, true) - base).abs() < 1e-5);

    // Distant / Dome: area-independent regardless of normalize.
    let distant = RenderLightKind::Distant { angle_radians: 0.1 };
    assert!((light_point_intensity(&distant, base, false) - base).abs() < 1e-5);
    assert!((light_point_intensity(&distant, base, true) - base).abs() < 1e-5);
    assert!((light_point_intensity(&RenderLightKind::Dome, base, false) - base).abs() < 1e-5);
}

#[test]
fn caster_light_size_is_per_kind_and_non_negative() {
    // Distant → angular diameter (radians), clamped at 0.
    assert!(
        (caster_light_size(&RenderLightKind::Distant {
            angle_radians: 0.02
        }) - 0.02)
            .abs()
            < 1e-6
    );
    assert_eq!(
        caster_light_size(&RenderLightKind::Distant {
            angle_radians: -1.0
        }),
        0.0
    );
    // Positional area lights derive a size from their dominant radius/extent.
    assert!((caster_light_size(&RenderLightKind::Sphere { radius: 0.5 }) - 0.5).abs() < 1e-6);
    assert!((caster_light_size(&RenderLightKind::Disk { radius: 0.3 }) - 0.3).abs() < 1e-6);
    assert!(
        (caster_light_size(&RenderLightKind::Rect {
            width: 4.0,
            height: 2.0
        }) - 2.0)
            .abs()
            < 1e-6
    );
    assert!(
        (caster_light_size(&RenderLightKind::Cylinder {
            radius: 0.7,
            length: 5.0
        }) - 0.7)
            .abs()
            < 1e-6
    );
    // Dome / procedural have no discrete penumbra.
    assert_eq!(caster_light_size(&RenderLightKind::Dome), 0.0);
    assert_eq!(caster_light_size(&RenderLightKind::DomeProcedural), 0.0);
}

#[test]
fn empty_world_gives_empty_snapshot() {
    let mut world = World::new();
    let snap = RenderWorldSnapshot::extract(&mut world, 16.0 / 9.0, Mat4::IDENTITY);
    assert!(snap.meshes.is_empty());
    assert!(snap.lights.is_empty());
    assert!(snap.camera.is_none());
    assert!(snap.material_overrides.is_empty());
}

#[test]
fn material_override_map_set_get_clear() {
    use idtx_usd::{PrimId, StageGeneration};
    let generation = StageGeneration::from_raw(1);
    let a = PrimId::from_raw_parts(3, generation);
    let b = PrimId::from_raw_parts(4, generation);

    let mut map = MaterialOverrideMap::default();
    assert!(map.is_empty());
    assert_eq!(map.get(a), None);

    map.set(a, "/World/Materials/Highlight");
    assert_eq!(map.len(), 1);
    assert_eq!(map.get(a), Some("/World/Materials/Highlight"));
    assert_eq!(map.get(b), None);

    // Re-setting the same prim replaces the override.
    map.set(a, "/World/Materials/Selected");
    assert_eq!(map.len(), 1);
    assert_eq!(map.get(a), Some("/World/Materials/Selected"));

    // Clear returns the removed path and empties the map.
    assert_eq!(map.clear(a).as_deref(), Some("/World/Materials/Selected"));
    assert!(map.is_empty());
    assert_eq!(map.clear(a), None);
}

#[test]
fn perspective_near_plane_maps_to_zero() {
    let proj = glam::camera::rh::proj::directx::perspective(std::f32::consts::FRAC_PI_2, 1.0, 1.0, 1000.0);
    // RH convention: near plane is at z_view = -near (negative z = in front of camera).
    let p = proj * glam::Vec4::new(0.0, 0.0, -1.0, 1.0);
    let ndc_z = p.z / p.w;
    assert!(
        (ndc_z - 0.0).abs() < 1e-4,
        "near maps to NDC 0, got {ndc_z}"
    );
}

/// Build a minimal triangle `GpuMeshDesc` (3 unique positions, 1 triangle).
fn make_triangle_mesh(offset: [f32; 3]) -> GpuMeshDesc {
    let [ox, oy, oz] = offset;
    GpuMeshDesc {
        prim_path: String::new(),
        material_path: None,
        local_aabb: Some(([ox, oy, oz], [ox + 1.0, oy + 1.0, oz])),
        positions: vec![[ox, oy, oz], [ox + 1.0, oy, oz], [ox + 0.5, oy + 1.0, oz]],
        normals: vec![[0.0, 0.0, 1.0]; 3],
        texcoords: vec![[0.0, 0.0]; 3],
        vertex_colors: vec![],
        tangents: vec![],
        indices: vec![0, 1, 2],
        double_sided: false,
        skin: None,
        serial: 0,
    }
}

fn make_mesh_instance(
    path: &str,
    mut mesh: GpuMeshDesc,
    world: glam::Mat4,
) -> RenderMeshInstance {
    // Real extraction sets `GpuMeshDesc::prim_path` to the OWNING USD prim
    // (== `node.path`) for both plain and subset-split meshes. For a plain
    // mesh that equals the instance path, so default an unset mesh path to
    // the instance path here; subset-split tests pass a mesh whose
    // `prim_path` is the owning prim while `path` carries the synthetic
    // `/subset_*` suffix, and that explicit value is preserved.
    if mesh.prim_path.is_empty() {
        mesh.prim_path = path.to_owned();
    }
    RenderMeshInstance {
        prim_path: path.to_owned(),
        mesh: Arc::new(mesh),
        world_matrix: world,
        material: crate::material::fallback_material(path),
        purpose: RenderPurpose::Default,
        casts_shadow: true,
    }
}

#[test]
fn topology_instances_grouped_by_internal_ref_arc() {
    let mesh = make_triangle_mesh([0.0, 0.0, 0.0]);
    let t1 = glam::Mat4::from_translation(glam::Vec3::new(0.0, 0.0, 0.0));
    let t2 = glam::Mat4::from_translation(glam::Vec3::new(2.0, 0.0, 0.0));
    let t3 = glam::Mat4::from_translation(glam::Vec3::new(4.0, 0.0, 0.0));

    // Simulate: Bolt1, Bolt2, Bolt3 each have an instance mesh child
    let mut snap = RenderWorldSnapshot::default();
    snap.meshes
        .push(make_mesh_instance("/Bolts/Bolt1/Body", mesh.clone(), t1));
    snap.meshes
        .push(make_mesh_instance("/Bolts/Bolt2/Body", mesh.clone(), t2));
    snap.meshes
        .push(make_mesh_instance("/Bolts/Bolt3/Body", mesh.clone(), t3));

    // Simulated InternalRefArc groups (prototype → [instance roots])
    let mut groups = HashMap::new();
    groups.insert(
        "/BoltPrototype".to_string(),
        vec![
            "/Bolts/Bolt1".to_string(),
            "/Bolts/Bolt2".to_string(),
            "/Bolts/Bolt3".to_string(),
        ],
    );

    promote_topology_instances_from_groups(&mut snap, &groups);

    assert!(
        snap.meshes.is_empty(),
        "all instance meshes should be promoted; {} remain",
        snap.meshes.len()
    );
    assert_eq!(snap.instanced_batches.len(), 1);
    let batch = &snap.instanced_batches[0];
    assert_eq!(batch.instance_transforms.len(), 3);
    assert_eq!(batch.instance_prim_paths.len(), 3);
    assert!(
        batch
            .instance_prim_paths
            .contains(&"/Bolts/Bolt1".to_string())
    );
    assert!(
        batch
            .instance_prim_paths
            .contains(&"/Bolts/Bolt2".to_string())
    );
    assert!(
        batch
            .instance_prim_paths
            .contains(&"/Bolts/Bolt3".to_string())
    );
    assert!(batch.prototype_key.starts_with("toporef_/BoltPrototype"));
    assert_eq!(snap.stats.topology_instance_promoted, 3);
    assert_eq!(snap.stats.instanced_draw_count, 3);
}

#[test]
fn topology_instances_skipped_when_material_differs() {
    let mesh = make_triangle_mesh([0.0, 0.0, 0.0]);

    let mut snap = RenderWorldSnapshot::default();
    snap.meshes.push(make_mesh_instance(
        "/Bolts/Bolt1/Body",
        mesh.clone(),
        glam::Mat4::IDENTITY,
    ));
    // Bolt2/Body has different material fingerprint (simulates a material override)
    let mut bolt2_inst =
        make_mesh_instance("/Bolts/Bolt2/Body", mesh.clone(), glam::Mat4::IDENTITY);
    bolt2_inst.material.fingerprint = 0xdead_beef;
    snap.meshes.push(bolt2_inst);

    let mut groups = HashMap::new();
    groups.insert(
        "/BoltPrototype".to_string(),
        vec!["/Bolts/Bolt1".to_string(), "/Bolts/Bolt2".to_string()],
    );

    promote_topology_instances_from_groups(&mut snap, &groups);

    // Material mismatch: topology promotion should not create a batch
    assert_eq!(
        snap.meshes.len(),
        2,
        "mismatched material instances must remain"
    );
    assert!(snap.instanced_batches.is_empty());
    assert_eq!(snap.stats.topology_instance_promoted, 0);
}

#[test]
fn topology_instances_skipped_when_shadow_state_differs() {
    let mesh = make_triangle_mesh([0.0, 0.0, 0.0]);

    let mut snap = RenderWorldSnapshot::default();
    snap.meshes.push(make_mesh_instance(
        "/Bolts/Bolt1/Body",
        mesh.clone(),
        glam::Mat4::IDENTITY,
    ));
    // Bolt2/Body is geometrically/materially identical but does NOT cast a
    // shadow. Promoting would copy Bolt1's `casts_shadow = true` onto it,
    // making a non-caster cast — so the group must not be promoted.
    let mut bolt2_inst =
        make_mesh_instance("/Bolts/Bolt2/Body", mesh.clone(), glam::Mat4::IDENTITY);
    bolt2_inst.casts_shadow = false;
    snap.meshes.push(bolt2_inst);

    let mut groups = HashMap::new();
    groups.insert(
        "/BoltPrototype".to_string(),
        vec!["/Bolts/Bolt1".to_string(), "/Bolts/Bolt2".to_string()],
    );

    promote_topology_instances_from_groups(&mut snap, &groups);

    assert_eq!(
        snap.meshes.len(),
        2,
        "instances with mismatched shadow-casting state must remain"
    );
    assert!(snap.instanced_batches.is_empty());
    assert_eq!(snap.stats.topology_instance_promoted, 0);
}

#[test]
fn topology_instances_multi_level_produces_two_batches() {
    let mesh = make_triangle_mesh([0.0, 0.0, 0.0]);
    let t1 = glam::Mat4::from_translation(glam::Vec3::new(0.0, 0.0, 0.0));
    let t2 = glam::Mat4::from_translation(glam::Vec3::new(2.0, 0.0, 0.0));

    // Two instances, each with two sub-meshes at different relative paths
    let mut snap = RenderWorldSnapshot::default();
    snap.meshes.push(make_mesh_instance(
        "/Bolts/Bolt1/Assembly/Part1",
        mesh.clone(),
        t1,
    ));
    snap.meshes.push(make_mesh_instance(
        "/Bolts/Bolt1/Assembly/Part2",
        mesh.clone(),
        t1,
    ));
    snap.meshes.push(make_mesh_instance(
        "/Bolts/Bolt2/Assembly/Part1",
        mesh.clone(),
        t2,
    ));
    snap.meshes.push(make_mesh_instance(
        "/Bolts/Bolt2/Assembly/Part2",
        mesh.clone(),
        t2,
    ));

    let mut groups = HashMap::new();
    groups.insert(
        "/BoltPrototype".to_string(),
        vec!["/Bolts/Bolt1".to_string(), "/Bolts/Bolt2".to_string()],
    );

    promote_topology_instances_from_groups(&mut snap, &groups);

    assert!(
        snap.meshes.is_empty(),
        "all meshes should be promoted; {} remain",
        snap.meshes.len()
    );
    assert_eq!(
        snap.instanced_batches.len(),
        2,
        "one batch per sub-mesh path"
    );
    assert_eq!(snap.stats.topology_instance_promoted, 4);
    // Each batch has 2 instances
    for batch in &snap.instanced_batches {
        assert_eq!(batch.instance_transforms.len(), 2);
        assert_eq!(batch.instance_prim_paths.len(), 2);
    }
}

const IDENTITY_16: [f32; 16] = [
    1.0, 0.0, 0.0, 0.0, //
    0.0, 1.0, 0.0, 0.0, //
    0.0, 0.0, 1.0, 0.0, //
    0.0, 0.0, 0.0, 1.0, //
];

#[test]
fn patch_joint_matrices_recomputes_from_ecs_pose_and_preserves_serial() {
    use idtx_usd::{PrimId, PrimState, SkinningMethod, StageGeneration};

    // Fix-3 regression guard: a skinned-controller pose write reprojects the
    // ECS `SkeletonPoseComp` in place; `patch_joint_matrices` must rebuild the
    // GPU joint palette from that pose WITHOUT bumping the mesh serial (which
    // would force a full instanced-buffer rebuild and reintroduce the eye0
    // stall). Meshes with no binding stay untouched.
    let generation = StageGeneration::from_raw(1);
    let skel_id = PrimId::from_raw_parts(7, generation);
    let mesh_id = PrimId::from_raw_parts(8, generation);

    // One posed joint is a pure translation (row-major, translation in the
    // last row); the other is identity. With identity skeleton-world,
    // geom-bind and stage basis the resulting palette equals those matrices
    // converted to glam column-major order.
    let translate_1_2_3: [f32; 16] = [
        1.0, 0.0, 0.0, 0.0, //
        0.0, 1.0, 0.0, 0.0, //
        0.0, 0.0, 1.0, 0.0, //
        1.0, 2.0, 3.0, 1.0, //
    ];
    let pose = SkeletonPoseRecord {
        skeleton: skel_id,
        animation_source: None,
        skeleton_world_transform: IDENTITY_16,
        joint_local_transforms: vec![],
        joint_world_transforms: vec![],
        skinning_transforms: vec![translate_1_2_3, IDENTITY_16],
    };

    let skin = SkinBindingRecord {
        prim: mesh_id,
        skeleton: skel_id,
        method: SkinningMethod::ClassicLinear,
        execution: SkinningExecution::GpuClassicLinear,
        geom_bind_transform: IDENTITY_16,
        influences: vec![],
        cpu_skinned_points: vec![],
        skinned_bounds: None,
    };

    let mut world = World::new();
    world.spawn(SkeletonPoseComp {
        record: pose.clone(),
    });
    world.spawn((
        PrimNode {
            prim: mesh_id,
            path: "/Char/Body".to_owned(),
            parent: None,
            name: "Body".to_owned(),
            type_name: "Mesh".to_owned(),
            state: PrimState {
                active: true,
                defined: true,
                loaded: true,
            },
            is_pseudo_root: false,
            local_layer_identifier: None,
        },
        SkinBindingComp { record: skin },
    ));

    // Snapshot: the skinned body carries a stale palette and a sentinel
    // serial; a second static mesh has no skin binding at all.
    let mut body_mesh = make_triangle_mesh([0.0, 0.0, 0.0]);
    body_mesh.serial = 42;
    body_mesh.skin = Some(SkinData {
        influences: vec![SkinVertex {
            joints: [0; 8],
            weights: [1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        }],
        joint_matrices: vec![[0.0; 16], [0.0; 16]], // stale — must be overwritten
    });

    let mut snap = RenderWorldSnapshot::default();
    snap.meshes
        .push(make_mesh_instance("/Char/Body", body_mesh, Mat4::IDENTITY));
    snap.meshes.push(make_mesh_instance(
        "/Char/Static",
        make_triangle_mesh([5.0, 0.0, 0.0]),
        Mat4::IDENTITY,
    ));

    snap.patch_joint_matrices(&mut world, Mat4::IDENTITY);

    let body = &snap.meshes[0];
    let skin = body
        .mesh
        .skin
        .as_ref()
        .expect("skinned body keeps a palette");
    assert_eq!(skin.joint_matrices.len(), 2);
    // Joint 0 → the translation, converted to glam column-major layout.
    assert_eq!(
        skin.joint_matrices[0],
        [
            1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 1.0, 2.0, 3.0, 1.0
        ]
    );
    assert_eq!(skin.joint_matrices[1], IDENTITY_16);
    // The palette changed but the serial did NOT — the cache stays warm.
    assert_eq!(
        body.mesh.serial, 42,
        "serial must be preserved by the patch"
    );

    // The unbound static mesh is left exactly as extracted.
    assert!(snap.meshes[1].mesh.skin.is_none());
}

#[test]
fn patch_joint_matrices_moves_subset_split_skinned_mesh() {
    use idtx_usd::{PrimId, PrimState, SkinningMethod, StageGeneration};

    // Regression guard for the frozen-controller bug: a skinned mesh with
    // GeomSubset materials is split at extract into per-subset instances whose
    // `prim_path` carries a synthetic `/subset_<mat>` (or `/remainder`) suffix,
    // while `GpuMeshDesc::prim_path` keeps the OWNING prim path. The skin map is
    // keyed by the owning prim (`node.path`), so `patch_joint_matrices` must
    // look up `inst.mesh.prim_path`, not the synthetic `inst.prim_path`.
    // Matching on `inst.prim_path` misses every subset piece → the palette is
    // never rebuilt → the (material-split, skinned) XR controller freezes at
    // its first-frame pose. This exercises exactly that shape.
    let generation = StageGeneration::from_raw(1);
    let skel_id = PrimId::from_raw_parts(7, generation);
    let mesh_id = PrimId::from_raw_parts(8, generation);

    let translate_1_2_3: [f32; 16] = [
        1.0, 0.0, 0.0, 0.0, //
        0.0, 1.0, 0.0, 0.0, //
        0.0, 0.0, 1.0, 0.0, //
        1.0, 2.0, 3.0, 1.0, //
    ];
    let pose = SkeletonPoseRecord {
        skeleton: skel_id,
        animation_source: None,
        skeleton_world_transform: IDENTITY_16,
        joint_local_transforms: vec![],
        joint_world_transforms: vec![],
        skinning_transforms: vec![translate_1_2_3, IDENTITY_16],
    };
    let skin = SkinBindingRecord {
        prim: mesh_id,
        skeleton: skel_id,
        method: SkinningMethod::ClassicLinear,
        execution: SkinningExecution::GpuClassicLinear,
        geom_bind_transform: IDENTITY_16,
        influences: vec![],
        cpu_skinned_points: vec![],
        skinned_bounds: None,
    };

    // ECS is keyed by the OWNING prim path "/M" (what the hydrator inserts).
    let mut world = World::new();
    world.spawn(SkeletonPoseComp {
        record: pose.clone(),
    });
    world.spawn((
        PrimNode {
            prim: mesh_id,
            path: "/M".to_owned(),
            parent: None,
            name: "M".to_owned(),
            type_name: "Mesh".to_owned(),
            state: PrimState {
                active: true,
                defined: true,
                loaded: true,
            },
            is_pseudo_root: false,
            local_layer_identifier: None,
        },
        SkinBindingComp { record: skin },
    ));

    // The snapshot instance is a subset split: synthetic instance path but
    // owning `mesh.prim_path == "/M"`, with a stale sentinel palette.
    let mut subset_mesh = make_triangle_mesh([0.0, 0.0, 0.0]);
    subset_mesh.prim_path = "/M".to_owned();
    subset_mesh.serial = 42;
    subset_mesh.skin = Some(SkinData {
        influences: vec![SkinVertex {
            joints: [0; 8],
            weights: [1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        }],
        joint_matrices: vec![[0.0; 16], [0.0; 16]], // stale — must be overwritten
    });

    let mut snap = RenderWorldSnapshot::default();
    // Instance path is the synthetic subset suffix, NOT "/M".
    snap.meshes.push(make_mesh_instance(
        "/M/subset_red",
        subset_mesh,
        Mat4::IDENTITY,
    ));
    assert!(
        snap.meshes[0].prim_path.starts_with("/M/subset_"),
        "instance carries the synthetic subset suffix, not the owning prim path"
    );

    snap.patch_joint_matrices(&mut world, Mat4::IDENTITY);

    let subset = &snap.meshes[0];
    let skin = subset
        .mesh
        .skin
        .as_ref()
        .expect("subset-split skinned mesh keeps a palette");
    // Pre-fix these stay at the [0.0; 16] sentinel (frozen); post-fix they are
    // rebuilt from the ECS pose.
    assert_eq!(
        skin.joint_matrices[0],
        [
            1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 1.0, 2.0, 3.0, 1.0
        ],
        "subset-split skinned mesh must be re-posed, not frozen at its stale palette"
    );
    assert_eq!(skin.joint_matrices[1], IDENTITY_16);
    assert_eq!(
        subset.mesh.serial, 42,
        "serial must be preserved by the patch"
    );
}

#[test]
fn patch_world_matrices_moves_rigid_subset_split_mesh() {
    // Rigid counterpart: a non-skinned mesh split by GeomSubset materials must
    // also move. `patch_world_matrices` keys the patch map by the authored USD
    // prim path; the subset instance's `prim_path` is a synthetic suffix, so
    // the match must be on `inst.mesh.prim_path` (the owning prim "/M").
    let mut subset_mesh = make_triangle_mesh([0.0, 0.0, 0.0]);
    subset_mesh.prim_path = "/M".to_owned();

    let mut snap = RenderWorldSnapshot::default();
    snap.meshes.push(make_mesh_instance(
        "/M/remainder",
        subset_mesh,
        Mat4::IDENTITY,
    ));
    assert!(snap.meshes[0].prim_path.starts_with("/M/"));

    let translate: [f32; 16] = [
        1.0, 0.0, 0.0, 0.0, //
        0.0, 1.0, 0.0, 0.0, //
        0.0, 0.0, 1.0, 0.0, //
        1.0, 2.0, 3.0, 1.0, //
    ];
    snap.patch_world_matrices(&[("/M".to_owned(), translate)], Mat4::IDENTITY);

    // Pre-fix the world matrix stays IDENTITY (frozen); post-fix it moves.
    assert_eq!(
        snap.meshes[0].world_matrix,
        mat4_from_usd_row_major(&translate),
        "rigid subset-split mesh must follow its owning prim's transform"
    );
}

#[test]
fn patch_world_matrices_moves_light() {
    // A moved light (or a moved ancestor of one) must reposition its
    // illumination in place — the lights uniform is rebuilt every frame from
    // `snapshot.lights`, so this patch is all that is needed. Crucially it
    // must NOT bump `serial` (that would invalidate the warm GPU caches and
    // negate the fast path).
    let mut snap = RenderWorldSnapshot::default();
    snap.lights.push(RenderLightDesc {
        prim_path: "/World/KeyLight".to_owned(),
        kind: RenderLightKind::Sphere { radius: 0.1 },
        world_matrix: Mat4::IDENTITY,
        color: [1.0, 1.0, 1.0],
        power: 100.0,
        diffuse_contribution: 1.0,
        specular_contribution: 1.0,
        shadow_enabled: false,
        cone_angle: None,
        cone_softness: 0.0,
        shadow_view_proj: None,
        shadow_near: 0.0,
        shadow_far: 0.0,
        light_size: 0.0,
        shadow_slot: None,
        dome_texture_path: None,
        dome_texture_format: DomeTextureFormat::default(),
        env_rotation: Mat3::IDENTITY,
        ambient: [0.0; 3],
    });
    let serial_before = snap.serial;

    let translate: [f32; 16] = [
        1.0, 0.0, 0.0, 0.0, //
        0.0, 1.0, 0.0, 0.0, //
        0.0, 0.0, 1.0, 0.0, //
        4.0, 5.0, 6.0, 1.0, //
    ];
    snap.patch_world_matrices(&[("/World/KeyLight".to_owned(), translate)], Mat4::IDENTITY);

    assert_eq!(
        snap.lights[0].world_matrix,
        mat4_from_usd_row_major(&translate),
        "a moved light must have its world matrix patched in place"
    );
    assert_eq!(
        snap.serial, serial_before,
        "patching a light matrix must not bump the snapshot serial"
    );
    // Emission is transform-independent and must be left untouched.
    assert_eq!(snap.lights[0].power, 100.0);
}

#[test]
fn patch_world_matrices_refits_moved_distant_caster_shadow() {
    // The core fix: moving a shadow-casting Distant light on the fast path
    // must refit its shadow frustum so `shadow_slots[*].view_proj` changes —
    // that matrix is what the renderer's shadow-atlas cache is keyed on. If it
    // stayed frozen the depth pre-pass would be skipped and the shadow would
    // not track the light. `serial` must stay put (warm GPU caches survive).
    let mut snap = RenderWorldSnapshot::default();
    snap.shadow = RenderShadowSettings::new(ShadowTier::Soft);
    snap.meshes.push(make_mesh_instance(
        "/M",
        make_triangle_mesh([0.0, 0.0, 0.0]),
        Mat4::IDENTITY,
    ));
    snap.shadow_aabb = crate::camera_fit::compute_scene_aabb(&snap, true);
    assert!(
        snap.shadow_aabb.is_some(),
        "the mesh gives a non-empty AABB"
    );

    // A Distant caster at identity (its +Z column faces straight along Z).
    let mut light = alloc_light("/World/Sun", 100.0, true);
    light.world_matrix = Mat4::IDENTITY;
    let (vp0, n0, f0, s0) = caster_shadow_fit(
        &light.kind,
        light.shadow_enabled,
        light.world_matrix,
        snap.shadow_aabb,
    );
    light.shadow_view_proj = vp0;
    light.shadow_near = n0;
    light.shadow_far = f0;
    light.light_size = s0;
    snap.lights.push(light);
    snap.shadow_slots = ShadowAtlasAllocator::allocate(&mut snap.lights);
    assert_eq!(snap.shadow_slots.len(), 1, "one caster → one slot");

    let vp_before = snap.shadow_slots[0].view_proj;
    let serial_before = snap.serial;
    let aabb_before = snap.shadow_aabb;

    // Rotate the light 90° about X → its +Z column (the shadow direction)
    // changes, so the fitted view-proj must change.
    let rot = Mat4::from_rotation_x(std::f32::consts::FRAC_PI_2).to_cols_array();
    snap.patch_world_matrices(&[("/World/Sun".to_owned(), rot)], Mat4::IDENTITY);

    assert_ne!(
        snap.lights[0].shadow_view_proj,
        Some(vp_before),
        "a moved light must refit its shadow_view_proj"
    );
    assert_ne!(
        snap.shadow_slots[0].view_proj, vp_before,
        "the allocated shadow slot must carry the refit view-projection"
    );
    assert_eq!(
        snap.shadow_slots[0].view_proj,
        snap.lights[0]
            .shadow_view_proj
            .expect("caster keeps a shadow_view_proj"),
        "slot view_proj must stay in sync with the light after the refit"
    );
    assert_eq!(
        snap.shadow_aabb, aabb_before,
        "a light-only move must NOT change the scene AABB (no caster moved)"
    );
    assert_eq!(
        snap.serial, serial_before,
        "the shadow refit must not bump the snapshot serial"
    );
}

#[test]
fn patch_world_matrices_light_move_is_noop_when_shadows_off() {
    // With shadows off the refit branch is skipped entirely: no shadow_slots
    // are produced and the (extract-time-None) shadow_view_proj is untouched.
    let mut snap = RenderWorldSnapshot::default(); // shadow tier defaults to Off
    assert!(!snap.shadow.enabled());
    let mut light = alloc_light("/World/Sun", 100.0, true);
    light.world_matrix = Mat4::IDENTITY;
    // Shadows off → a full extract would leave the caster's projection None.
    light.shadow_view_proj = None;
    light.shadow_slot = None;
    snap.lights.push(light);

    let rot = Mat4::from_rotation_x(std::f32::consts::FRAC_PI_2).to_cols_array();
    snap.patch_world_matrices(&[("/World/Sun".to_owned(), rot)], Mat4::IDENTITY);

    assert!(
        snap.shadow_slots.is_empty(),
        "no shadow slots may be allocated when shadows are off"
    );
    assert!(
        snap.lights[0].shadow_view_proj.is_none(),
        "the refit must not run when shadows are off"
    );
    // The light itself still moved.
    assert_eq!(
        snap.lights[0].world_matrix,
        mat4_from_usd_row_major(&rot),
        "the light world matrix is still patched with shadows off"
    );
}

#[test]
fn patch_world_matrices_caster_move_refits_aabb_and_frustum() {
    // Moving a CASTER (not the light) far away must refresh the cached scene
    // AABB and refit the caster frusta to it — otherwise a caster dragged
    // outside the old frustum would have its shadow clipped on the fast path.
    let mut snap = RenderWorldSnapshot::default();
    snap.shadow = RenderShadowSettings::new(ShadowTier::Soft);
    snap.meshes.push(make_mesh_instance(
        "/M",
        make_triangle_mesh([0.0, 0.0, 0.0]),
        Mat4::IDENTITY,
    ));
    snap.shadow_aabb = crate::camera_fit::compute_scene_aabb(&snap, true);

    let mut light = alloc_light("/World/Sun", 100.0, true);
    light.world_matrix = Mat4::IDENTITY;
    let (vp0, n0, f0, s0) = caster_shadow_fit(
        &light.kind,
        light.shadow_enabled,
        light.world_matrix,
        snap.shadow_aabb,
    );
    light.shadow_view_proj = vp0;
    light.shadow_near = n0;
    light.shadow_far = f0;
    light.light_size = s0;
    snap.lights.push(light);
    snap.shadow_slots = ShadowAtlasAllocator::allocate(&mut snap.lights);

    let aabb_before = snap.shadow_aabb;
    let vp_before = snap.shadow_slots[0].view_proj;
    let serial_before = snap.serial;

    // Translate the mesh far along +X; the light stays put.
    let translate: [f32; 16] = [
        1.0, 0.0, 0.0, 0.0, //
        0.0, 1.0, 0.0, 0.0, //
        0.0, 0.0, 1.0, 0.0, //
        100.0, 0.0, 0.0, 1.0, //
    ];
    snap.patch_world_matrices(&[("/M".to_owned(), translate)], Mat4::IDENTITY);

    assert_ne!(
        snap.shadow_aabb, aabb_before,
        "moving a caster must refresh the cached scene AABB"
    );
    assert_ne!(
        snap.shadow_slots[0].view_proj, vp_before,
        "the caster frustum must refit to the new AABB"
    );
    assert_eq!(
        snap.serial, serial_before,
        "the caster-move refit must not bump the snapshot serial"
    );
}

#[test]
fn patch_world_matrices_in_bounds_move_skips_aabb_recompute() {
    // Shadow-refit fast-path: a caster that moves but stays inside the cached
    // scene AABB must NOT trigger the (now O(meshes)) recompute — the cached
    // conservative bounds stay valid. This is the fix for the head-tracked player
    // rig gizmos, which move every frame but remain inside the room.
    let mut snap = RenderWorldSnapshot::default();
    snap.shadow = RenderShadowSettings::new(ShadowTier::Soft);
    snap.meshes.push(make_mesh_instance(
        "/M",
        make_triangle_mesh([0.0, 0.0, 0.0]),
        Mat4::IDENTITY,
    ));
    // A deliberately large cached AABB (as if larger static geometry defined it).
    let big = (glam::Vec3::splat(-100.0), glam::Vec3::splat(100.0));
    snap.shadow_aabb = Some(big);
    snap.shadow_aabb_sig = crate::camera_fit::scene_aabb_signature(&snap);

    // Move the mesh a small amount — its transformed bounds stay well inside ±100.
    let small: [f32; 16] = [
        1.0, 0.0, 0.0, 0.0, //
        0.0, 1.0, 0.0, 0.0, //
        0.0, 0.0, 1.0, 0.0, //
        0.5, 0.5, 0.0, 1.0, //
    ];
    snap.patch_world_matrices(&[("/M".to_owned(), small)], Mat4::IDENTITY);

    assert_eq!(
        snap.shadow_aabb,
        Some(big),
        "an in-bounds caster move must reuse the cached AABB (no recompute)"
    );
}

#[test]
fn patch_widget_panels_restamps_in_place_and_preserves_serial() {
    // A value-only widget edit (e.g. an OpenExec compute writeback into
    // `idtx:widget:text`) re-stamps the ECS `WidgetPanelComp`; `patch_widget_panels`
    // must splice the fresh content_hash / opacity / size into the warm snapshot in
    // place WITHOUT bumping `serial` (which would invalidate the warm GPU caches and
    // force the whole-stage re-extract this fast path exists to avoid).
    use idtx_scene::{
        PrimNode, WidgetBillboard, WidgetPanelComp, WidgetSpace, WorldTransform,
    };
    use idtx_usd::{PrimId, PrimState, StageGeneration};

    let generation = StageGeneration::from_raw(1);
    let panel_id = PrimId::from_raw_parts(3, generation);

    let mut world = World::new();
    world.spawn((
        PrimNode {
            prim: panel_id,
            path: "/World/Panel".to_owned(),
            parent: None,
            name: "Panel".to_owned(),
            type_name: "IdtxWidgetPanel".to_owned(),
            state: PrimState {
                active: true,
                defined: true,
                loaded: true,
            },
            is_pseudo_root: false,
            local_layer_identifier: None,
        },
        WidgetPanelComp {
            surface: Default::default(),
            size: [1.0, 1.0],
            resolution: [256, 256],
            pixels_per_unit: 256.0,
            billboard: WidgetBillboard::default(),
            double_sided: false,
            opacity: 0.5, // the fresh value the patch must pick up
            space: WidgetSpace::default(),
            content_source: None,
            elements: vec![],
            content_hash: 2, // changed from the snapshot's stale 1
        },
        WorldTransform {
            matrix: Mat4::IDENTITY,
        },
    ));

    // Warm snapshot with a STALE panel (opacity 1.0, hash 1) and a sentinel serial.
    let mut snap = RenderWorldSnapshot {
        serial: 77,
        ..RenderWorldSnapshot::default()
    };
    snap.widget_panels.push(super::WidgetPanel {
        highlight: None,
        surface: Default::default(),
        prim_path: "/World/Panel".to_owned(),
        world_matrix: Mat4::IDENTITY,
        size: [1.0, 1.0],
        resolution: [256, 256],
        billboard: WidgetBillboard::default(),
        double_sided: false,
        opacity: 1.0,
        space: WidgetSpace::default(),
        elements: vec![],
        content_hash: 1,
    });

    snap.patch_widget_panels(&mut world, Mat4::IDENTITY);

    let panel = &snap.widget_panels[0];
    assert_eq!(panel.content_hash, 2, "content_hash must track the ECS value");
    assert_eq!(panel.opacity, 0.5, "opacity must track the ECS value");
    assert_eq!(
        snap.serial, 77,
        "a value-only widget patch must NOT bump the snapshot serial"
    );
}

#[test]
fn splice_subtree_inserts_static_cube_without_serial_bump_and_stays_standalone() {
    use idtx_scene::{CubeComp, PrimNode, WorldTransform};
    use idtx_usd::{PrimId, PrimState, StageGeneration};

    let generation = StageGeneration::from_raw(1);
    let new_id = PrimId::from_raw_parts(7, generation);

    let mut world = World::new();
    world.spawn((
        PrimNode {
            prim: new_id,
            path: "/World/Spawned".to_owned(),
            parent: None,
            name: "Spawned".to_owned(),
            type_name: "Cube".to_owned(),
            state: PrimState {
                active: true,
                defined: true,
                loaded: true,
            },
            is_pseudo_root: false,
            local_layer_identifier: None,
        },
        CubeComp {
            size: 2.0,
            display_colors: vec![],
            material_path: None,
            double_sided: false,
            prototype_only: false,
        },
        WorldTransform {
            matrix: Mat4::IDENTITY,
        },
    ));

    // Warm snapshot with a pre-existing static mesh and a sentinel serial.
    let mut snap = RenderWorldSnapshot {
        serial: 77,
        instance_serial: 77,
        ..RenderWorldSnapshot::default()
    };
    snap.meshes.push(make_mesh_instance(
        "/World/Existing",
        make_triangle_mesh([0.0, 0.0, 0.0]),
        Mat4::IDENTITY,
    ));

    let spliced = snap.splice_subtree(&mut world, Mat4::IDENTITY, &[new_id], &[]);

    assert!(spliced, "a static cube subtree must splice (no fallback)");
    assert_eq!(snap.serial, 77, "splice must NOT bump the snapshot serial");
    assert_eq!(snap.meshes.len(), 2, "the spawned cube is inserted");
    assert!(
        snap.meshes.iter().any(|m| m.prim_path == "/World/Spawned"),
        "spawned cube present in the snapshot"
    );
    assert!(
        snap.standalone_paths.contains("/World/Spawned"),
        "spliced mesh must be standalone so the merge planner skips it"
    );
    assert!(
        snap.meshes.iter().any(|m| m.prim_path == "/World/Existing"),
        "the pre-existing mesh is left untouched"
    );
}

#[test]
fn splice_subtree_falls_back_when_subtree_has_instancing() {
    use idtx_scene::{CubeComp, NativeInstanceComp, PrimNode, WorldTransform};
    use idtx_usd::{PrimId, PrimState, StageGeneration};

    let generation = StageGeneration::from_raw(1);
    let proto = PrimId::from_raw_parts(8, generation);
    let inst_id = PrimId::from_raw_parts(9, generation);

    let mut world = World::new();
    world.spawn((
        PrimNode {
            prim: inst_id,
            path: "/World/Instanced".to_owned(),
            parent: None,
            name: "Instanced".to_owned(),
            type_name: "Cube".to_owned(),
            state: PrimState {
                active: true,
                defined: true,
                loaded: true,
            },
            is_pseudo_root: false,
            local_layer_identifier: None,
        },
        CubeComp {
            size: 1.0,
            display_colors: vec![],
            material_path: None,
            double_sided: false,
            prototype_only: false,
        },
        WorldTransform {
            matrix: Mat4::IDENTITY,
        },
        NativeInstanceComp { prototype: proto },
    ));

    let mut snap = RenderWorldSnapshot {
        serial: 50,
        instance_serial: 50,
        ..RenderWorldSnapshot::default()
    };

    let spliced = snap.splice_subtree(&mut world, Mat4::IDENTITY, &[inst_id], &[]);
    assert!(
        !spliced,
        "an instanced subtree must bail so the caller does a full extract"
    );
    assert!(
        snap.meshes.is_empty(),
        "a bailed splice must not partially mutate the snapshot meshes"
    );
}
