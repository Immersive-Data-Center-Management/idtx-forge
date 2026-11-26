#[cxx::bridge(namespace = "idtx")]
pub mod ffi {
    /// Typed USD defaults used by the live attribute-sync protocol.
    #[derive(Debug, Clone, Default)]
    struct SyncAttributeValueRaw {
        doubles: Vec<f64>,
        ints: Vec<i64>,
        uints: Vec<u64>,
        bools: Vec<bool>,
        strings: Vec<String>,
        is_array: bool,
    }
    #[derive(Debug, Clone, Default)]
    struct SyncAttributeRaw {
        name: String,
        type_name: String,
        value: SyncAttributeValueRaw,
        has_value: bool,
    }
    #[derive(Debug, Clone)]
    struct RootAttributeSnapshotRaw {
        prim_path: String,
        name: String,
        text: String,
    }
    #[derive(Debug, Clone)]
    struct RootAttributeMetadataDeltaRaw {
        expected: String,
        desired: String,
    }
    #[derive(Debug, Clone)]
    struct AuthoredDataRaw { authored: bool, value: String }
    #[derive(Debug, Clone)]
    struct VariantOpinionRaw { authored: bool, value: String }
    #[derive(Debug, Clone)]
    struct StageMetadataRaw {
        root_identifier: String,
        default_prim: String,
        up_axis: String,
        meters_per_unit: f64,
        start_time_code: f64,
        end_time_code: f64,
        time_codes_per_second: f64,
        /// True when `startTimeCode` is explicitly authored in the root layer.
        has_time_code_range: bool,
        /// Resolved root-layer shadow mode: `0` = off, `1` = hard, `2` = smooth,
        /// `3` = soft. Resolved C++-side from `customLayerData["idtx:shadowMode"]`
        /// (token off/hard/smooth/soft), falling back to the legacy
        /// `idtx:softShadows` bool (`true`→soft, `false`→hard), else `0` (off)
        /// when neither is authored.
        shadow_mode: i32,
        /// The root-layer `customLayerData["idtx:stageBehavior"]` string — the
        /// name of the stage-level behavior prim (`"StageBehavior"`), or empty
        /// when no stage behavior is assigned.
        stage_behavior_name: String,
        /// The root-layer `customLayerData["idtx:stageBehaviorRunScope"]` token
        /// (`standalone`/`payload`/`sublayer`/`always`) gating when the stage
        /// behavior ticks, or empty when unauthored (defaults to `standalone`).
        stage_behavior_run_scope: String,
    }

    /// Per-prim stage-behavior resolution carrier (see
    /// `stage_behavior_resolved_for_prim`): the owning-layer run-scope token and
    /// how that layer composes into the open stage.
    #[derive(Debug, Clone)]
    struct StageBehaviorResolvedRaw {
        /// `idtx:stageBehaviorRunScope` token off the owning layer, or empty when
        /// unauthored / no stage behavior at the prim.
        run_scope: String,
        /// `0`=standalone, `1`=payload, `2`=sublayer, `3`=reference, `4`=unknown.
        compose_context: i32,
    }

    /// The IDTX stage-kind tag read from a stage's root-layer `customLayerData`
    /// (`customLayerData.idtx`). Used by the project browser to classify a
    /// `.usd(a/c/z)` file without composing it — see the roadmap Appendix A.1.
    #[derive(Debug, Clone)]
    struct StageKindRaw {
        /// `"level"`, `"asset"`, `"assembly"`, `"player"`, or `""` when the
        /// layer carries no `customLayerData.idtx.stageKind`.
        stage_kind: String,
        /// The authored `displayName` (level/asset), or `""` if absent.
        display_name: String,
        /// The authored idtx `schemaVersion`, or `""` if the tag is absent.
        schema_version: String,
    }

    /// Root-layer composition targets read from a `.usd(a/c/z)` file **without
    /// composing a stage** (opens just the `SdfLayer`). Used by the scene-tree
    /// drop flow to decide what prim a reference/payload arc should point at: a
    /// dropped stage with no `defaultPrim` composes nothing unless the arc names
    /// an explicit target prim.
    #[derive(Debug, Clone)]
    struct StageRootPrimsRaw {
        /// The root layer's authored `defaultPrim` name, or `""` when none is
        /// authored.
        default_prim: String,
        /// Names of the root-level prim specs with the `def` specifier — the
        /// prims a reference/payload arc can actually compose. `over`/`class`
        /// roots are excluded.
        root_def_prims: Vec<String>,
    }

    /// Time-sample metadata for a single attribute (hydration-time only).
    #[derive(Debug, Clone)]
    struct AttrTimeSamplesRaw {
        /// Ascending authored time codes.
        times: Vec<f64>,
        /// `"held"`, `"linear"`, or `""` (default / linear).
        interpolation: String,
    }

    #[derive(Debug, Clone)]
    struct TopologyRecordRaw {
        path: String,
        parent_path: String,
        name: String,
        type_name: String,
        active: bool,
        defined: bool,
        loaded: bool,
        is_pseudo_root: bool,
        local_specifier: String,
        local_layer_identifier: String,
        packed_arc: String,
        packed_asset_path: String,
        internal_ref_target: String,
        is_xformable: bool,
        /// True when this prim is a native instance (`instanceable = true`); its geometry
        /// lives once in the shared prototype rather than under this path.
        is_instance: bool,
        /// True when this prim is (or lives inside) a shared instancing prototype
        /// (`/__Prototype_N`); exposed once for inspection, never drawn directly.
        is_prototype: bool,
    }

    #[derive(Debug, Clone)]
    struct TransformRecordRaw {
        path: String,
        world_matrix: Vec<f32>,
    }

    #[derive(Debug, Clone)]
    struct VisibilityRecordRaw {
        path: String,
        visible: bool,
        purpose: String,
        casts_shadow: bool,
    }

    #[derive(Debug, Clone)]
    struct BoundsRecordRaw {
        path: String,
        minimum: Vec<f32>,
        maximum: Vec<f32>,
        /// Local-space extent min (before the prim's world transform).
        local_minimum: Vec<f32>,
        /// Local-space extent max (before the prim's world transform).
        local_maximum: Vec<f32>,
    }

    #[derive(Debug, Clone)]
    struct CubeRecordRaw {
        path: String,
        size: f32,
        display_color: Vec<f32>,
        material_path: String,
        double_sided: bool,
        prototype_only: bool,
    }

    #[derive(Debug, Clone)]
    struct SphereRecordRaw {
        path: String,
        radius: f32,
        display_color: Vec<f32>,
        material_path: String,
        double_sided: bool,
        prototype_only: bool,
    }

    #[derive(Debug, Clone)]
    struct ExtendedGprimRecordRaw {
        path: String,
        kind: String,
        radius: f32,
        height: f32,
        axis: String,
        display_color: Vec<f32>,
        material_path: String,
        double_sided: bool,
        prototype_only: bool,
    }

    #[derive(Debug, Clone)]
    struct PlaneRecordRaw {
        path: String,
        width: f32,
        length: f32,
        axis: String,
        display_color: Vec<f32>,
        material_path: String,
        double_sided: bool,
        prototype_only: bool,
    }

    #[derive(Debug, Clone)]
    struct MaterialSourceNodeRaw {
        path: String,
        identifier: String,
    }

    #[derive(Debug, Clone)]
    struct MaterialDiagnosticRaw {
        code: String,
        source_path: String,
        message: String,
    }

    #[derive(Debug, Clone)]
    struct TextureInputRaw {
        semantic: String,
        asset_path: String,
        resolved_asset_path: String,
        swizzle: String,
        scale: Vec<f32>,
        bias: Vec<f32>,
        fallback: Vec<f32>,
        uv_name: String,
        uv_transform: Vec<f32>,
        wrap_s: String,
        wrap_t: String,
        min_filter: String,
        mag_filter: String,
        mip_filter: String,
        max_anisotropy: u32,
        source_color_space: String,
        interpretation: String,
        source_node_indices: Vec<u32>,
    }

    #[repr(u32)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum OpacityModeRaw {
        Transparent = 0,
        Presence = 1,
    }

    #[derive(Debug, Clone)]
    struct MaterialRecordRaw {
        path: String,
        base_color: Vec<f32>,
        metallic: f32,
        roughness: f32,
        subsurface: f32,
        emissive: Vec<f32>,
        /// Beer-Lambert transmission tint (glass absorption colour). White
        /// `{1,1,1}` = untinted/unauthored. Length 3 when present.
        transmission_color: Vec<f32>,
        opacity: f32,
        opacity_threshold: f32,
        opacity_mode: OpacityModeRaw,
        diffuse_texture: String,
        normal_texture: String,
        metallic_texture: String,
        roughness_texture: String,
        emissive_texture: String,
        occlusion_texture: String,
        metallic_texture_channel: u32,
        roughness_texture_channel: u32,
        occlusion_texture_channel: u32,
        diffuse_uv_set: String,
        diffuse_uv_transform: Vec<f32>,
        uses_diffuse_texture_alpha: bool,
        diagnostics: Vec<String>,
        material_ir_version: u32,
        material_model: String,
        material_feature_bits: u64,
        alpha_policy: String,
        sidedness: String,
        texture_inputs: Vec<TextureInputRaw>,
        source_nodes: Vec<MaterialSourceNodeRaw>,
        material_diagnostics: Vec<MaterialDiagnosticRaw>,
        material_ir_fingerprint: String,
    }

    #[derive(Debug, Clone)]
    struct BasisCurvesRecordRaw {
        path: String,
        points: Vec<f32>,
        counts: Vec<i32>,
        curve_type: String,
        basis: String,
        wrap: String,
        world_matrix: Vec<f64>,
        metres_per_unit: f64,
        z_up: bool,
        tolerance: f64,
        prototype_only: bool,
        eligible: bool,
        start_group: String,
        end_group: String,
    }

    #[derive(Debug, Clone)]
    struct CurveBakeRaw {
        owner: String,
        source: String,
        source_digest: String,
        settings_digest: String,
        generator: String,
        curve_index: u32,
        spacing: f64,
        start: f64,
        window: f64,
        lateral: Vec<f64>,
        floor_normal: Vec<f64>,
        half_extents: Vec<f64>,
        orient: bool,
        seed: u32,
        tolerance: f64,
        source_revision: u64,
        matrices: Vec<f64>,
    }

    #[derive(Debug, Clone)]
    struct CurveObstacleRaw {
        path: String,
        minimum: Vec<f64>,
        maximum: Vec<f64>,
    }

    #[derive(Debug, Clone)]
    struct MeshRecordRaw {
        path: String,
        points: Vec<f32>,
        /// Per-vertex normals expanded to face-corner order by the C++ shim.
        /// All USD interpolation modes (vertex, uniform, faceVarying) are
        /// handled there; len = face_vertex_indices.len() × 3.
        /// Empty when the mesh has no normals primvar; Rust then computes them.
        normals: Vec<f32>,
        /// Per-vertex tangents (xyz + bitangent sign w) expanded to face-corner
        /// order by the C++ shim. len = face_vertex_indices.len() × 4.
        /// Empty when the mesh has no tangents primvar.
        tangents: Vec<f32>,
        texcoords: Vec<f32>,
        face_vertex_counts: Vec<i32>,
        face_vertex_indices: Vec<i32>,
        display_color: Vec<f32>,
        material_path: String,
        subset_material_paths: Vec<String>,
        subset_face_counts: Vec<u32>,
        subset_face_indices: Vec<i32>,
        left_handed: bool,
        double_sided: bool,
        prototype_only: bool,
        /// `UsdGeomMesh` subdivision scheme token ("none", "catmullClark",
        /// "loop", "bilinear"). Empty when unauthored (USD default is
        /// "catmullClark"). The render layer does not subdivide, but uses this
        /// plus the crease/corner arrays below to decide synthesized hard edges.
        subdivision_scheme: String,
        /// Sharp-edge spec (point-index chains partitioned by `crease_lengths`;
        /// `crease_sharpnesses` is one value per crease or per edge). Empty when
        /// the mesh authors no creases.
        crease_indices: Vec<i32>,
        crease_lengths: Vec<i32>,
        crease_sharpnesses: Vec<f32>,
        /// Sharp-vertex spec (point indices + matching sharpness). Empty when the
        /// mesh authors no corners.
        corner_indices: Vec<i32>,
        corner_sharpnesses: Vec<f32>,
    }

    #[derive(Debug, Clone)]
    struct PointInstancerRecordRaw {
        path: String,
        prototype_paths: Vec<String>,
        proto_indices: Vec<i32>,
        transforms: Vec<f32>,
        ids: Vec<i64>,
        /// Empty means all visible, otherwise one native mask bit per instance.
        mask: Vec<u8>,
        /// Empty means unit coverage; otherwise parallel to ids.
        opacities: Vec<f32>,
    }

    /// Narrow native array transaction. Empty orientation/scale arrays mean identity.
    #[derive(Debug, Clone, Default)]
    struct PointInstancerUpdateRaw {
        positions: Vec<f32>,
        orientations_xyzw: Vec<f32>,
        scales: Vec<f32>,
        proto_indices: Vec<i32>,
        ids: Vec<i64>,
        invisible_ids: Vec<i64>,
    }

    #[derive(Debug, Clone)]
    struct PointInstancerInspectionRaw {
        arrays: PointInstancerUpdateRaw,
        prototype_paths: Vec<String>,
        mask: Vec<u8>,
        inactive_ids: Vec<i64>,
        count: u64,
        truncated: bool,
        diagnostics: Vec<String>,
    }

    #[derive(Debug, Clone)]
    struct PointInstancerSamplesRaw {
        records: Vec<PointInstancerRecordRaw>,
        diagnostics: Vec<String>,
    }

    /// A single native scenegraph instance (`instanceable = true`). The prototype's
    /// geometry is projected once (as `prototype_only`); each instance is recorded here
    /// as a lightweight (instance path → prototype path) pair. The instance's world
    /// placement is carried by the ordinary transform projection for `path`.
    #[derive(Debug, Clone)]
    struct NativeInstanceRecordRaw {
        path: String,
        prototype_path: String,
    }

    #[derive(Debug, Clone)]
    struct CameraRecordRaw {
        path: String,
        projection: String,
        focal_length: f64,
        horizontal_aperture: f64,
        vertical_aperture: f64,
        horizontal_aperture_offset: f64,
        vertical_aperture_offset: f64,
        clipping_near: f64,
        clipping_far: f64,
        clipping_planes: Vec<f64>,
        f_stop: f64,
        focus_distance: f64,
        stereo_role: String,
        shutter_open: f64,
        shutter_close: f64,
        exposure: f64,
        exposure_iso: f64,
        exposure_time: f64,
        exposure_f_stop: f64,
        exposure_responsivity: f64,
    }

    #[derive(Debug, Clone)]
    struct LightRecordRaw {
        path: String,
        kind: String,
        world_matrix: Vec<f32>,
        color: Vec<f32>,
        intensity: f32,
        exposure: f32,
        diffuse: f32,
        specular: f32,
        normalize: bool,
        angle: f32,
        radius: f32,
        width: f32,
        height: f32,
        /// CylinderLight axis length (local X). 0.0 for non-cylinder kinds.
        length: f32,
        /// True when the prim has a UsdLuxShapingAPI cone authored (spotlight).
        has_shaping: bool,
        /// Shaping cone half-angle in degrees (180 = no cone). Valid when has_shaping.
        cone_angle: f32,
        /// Shaping cone softness in [0, 1]. Valid when has_shaping.
        cone_softness: f32,
        shadow_enabled: bool,
        /// Non-empty for "dome" and "dome_procedural" kinds.
        dome_texture_path: String,
        /// True when kind == "dome_procedural" (IdtxProceduralDomeLight).
        dome_is_procedural: bool,
        /// `UsdLuxDomeLight` `texture:format` token ("automatic"/"latlong"/
        /// "mirroredBall"/"angular"/"cubeMapVerticalCross"). Empty when unset.
        dome_texture_format: String,
        /// `poleAxis` token ("scene"/"Y"/"Z"). Empty when unset (⇒ scene default).
        dome_pole_axis: String,
        /// Raw authored `colorTemperature` in Kelvin (default 6500). The blackbody
        /// multiplier is already folded into `color`; this is surfaced for display.
        color_temperature: f32,
        /// True when `enableColorTemperature` is authored on the light.
        enable_color_temperature: bool,
    }

    #[derive(Debug, Clone)]
    struct SkeletonTopologyRecordRaw {
        skeleton_path: String,
        joint_paths: Vec<String>,
        parent_indices: Vec<i32>,
        bind_transforms: Vec<f32>,
        rest_transforms: Vec<f32>,
    }

    #[derive(Debug, Clone)]
    struct SkeletonPoseRecordRaw {
        skeleton_path: String,
        animation_path: String,
        skeleton_world_transform: Vec<f32>,
        joint_local_transforms: Vec<f32>,
        joint_world_transforms: Vec<f32>,
        skinning_transforms: Vec<f32>,
    }

    #[derive(Debug, Clone)]
    struct SkinBindingRecordRaw {
        path: String,
        skeleton_path: String,
        skinning_method: String,
        execution: String,
        geom_bind_transform: Vec<f32>,
        joint_indices: Vec<u16>,
        joint_weights: Vec<f32>,
        cpu_skinned_points: Vec<f32>,
        skinned_bounds_minimum: Vec<f32>,
        skinned_bounds_maximum: Vec<f32>,
    }

    #[derive(Debug, Clone)]
    struct SkeletonDependencyRecordRaw {
        skeleton_path: String,
        animation_path: String,
        skinned_paths: Vec<String>,
    }

    #[derive(Debug, Clone)]
    struct AnimationClipRaw {
        name: String,
        source: String,
        targets: Vec<String>,
        start: f64,
        end: f64,
        speed: f64,
        looping: bool,
        autoplay: bool,
        channel: String,
        error: String,
    }

    #[derive(Debug, Clone)]
    struct ViewportProjectionRaw {
        schema_version: u32,
        diagnostics: Vec<String>,
        transforms: Vec<TransformRecordRaw>,
        visibility: Vec<VisibilityRecordRaw>,
        bounds: Vec<BoundsRecordRaw>,
        cubes: Vec<CubeRecordRaw>,
        spheres: Vec<SphereRecordRaw>,
        extended_gprims: Vec<ExtendedGprimRecordRaw>,
        planes: Vec<PlaneRecordRaw>,
        meshes: Vec<MeshRecordRaw>,
        curves: Vec<BasisCurvesRecordRaw>,
        point_instancers: Vec<PointInstancerRecordRaw>,
        native_instances: Vec<NativeInstanceRecordRaw>,
        materials: Vec<MaterialRecordRaw>,
        cameras: Vec<CameraRecordRaw>,
        lights: Vec<LightRecordRaw>,
        skeleton_topologies: Vec<SkeletonTopologyRecordRaw>,
        skeleton_poses: Vec<SkeletonPoseRecordRaw>,
        skin_bindings: Vec<SkinBindingRecordRaw>,
        skeleton_dependencies: Vec<SkeletonDependencyRecordRaw>,
    }

    #[derive(Debug, Clone)]
    struct PropertyRecordRaw {
        name: String,
        kind: String,
        type_name: String,
        value_summary: String,
        authored: bool,
        value_source: String,
        time_sample_count: u64,
        provenance: Vec<String>,
        /// Per-opinion authored value summary, parallel to `provenance`
        /// (same order, strongest-first). Empty string for opinions with no
        /// authored default (e.g. relationships, or time-sampled-only specs
        /// which carry `"(time samples)"` instead).
        provenance_values: Vec<String>,
        relationship_targets: Vec<String>,
        /// `true` when this property has an authored spec in the stage's **root
        /// layer** (the edit target). Drives the inspector's "remove root-layer
        /// opinion" affordance — only a root-layer opinion is removable there.
        root_authored: bool,
    }

    #[derive(Debug, Clone)]
    struct MaterialNodeRaw {
        path: String,
        identifier: String,
        input_names: Vec<String>,
        input_values: Vec<String>,
        connections: Vec<String>,
    }

    #[derive(Debug, Clone)]
    struct MaterialGraphRaw {
        material_path: String,
        material_paths: Vec<String>,
        nodes: Vec<MaterialNodeRaw>,
        diagnostics: Vec<String>,
        /// Parallel arrays: material prim's `outputs:*` attribute names and
        /// their first connection target (full SdfPath incl. `outputs:` ns).
        material_output_attr_names: Vec<String>,
        material_output_targets: Vec<String>,
    }

    /// One compute/datasource/consumer prim in a compute graph. Socket arrays
    /// are parallel (cxx shared structs cannot nest `Vec<struct>`): input socket
    /// `i` is `(input_names[i], input_types[i], input_values[i],
    /// input_connections[i])`, where `input_connections[i]` is the `\n`-joined
    /// list of that input's authored connection targets (empty if none).
    #[derive(Debug, Clone)]
    struct ComputeNodeRaw {
        path: String,
        type_name: String,
        /// `"consumer"` / `"datasource"` / `"compute"` / `"other"`.
        role: String,
        input_names: Vec<String>,
        input_types: Vec<String>,
        input_values: Vec<String>,
        input_connections: Vec<String>,
        output_names: Vec<String>,
        output_types: Vec<String>,
    }

    #[derive(Debug, Clone)]
    struct ComputeGraphRaw {
        /// Empty when the selected prim has no connected attribute (no graph).
        root_path: String,
        nodes: Vec<ComputeNodeRaw>,
        diagnostics: Vec<String>,
    }

    #[derive(Debug, Clone)]
    struct VariantSetRaw {
        name: String,
        choices: Vec<String>,
        selection: String,
        provenance: Vec<String>,
        defined_on_local_layer: bool,
    }

    #[cfg(feature = "authoring")]
    #[derive(Debug, Clone)]
    struct AuthoredXformOpRaw {
        name: String,
        op_type: String,
        precision: String,
        inverse: bool,
        time_sample_count: u64,
        has_default: bool,
        value: Vec<f64>,
    }

    #[cfg(feature = "authoring")]
    #[derive(Debug, Clone)]
    struct AuthoredXformStackRaw {
        path: String,
        reset_stack: bool,
        editable: bool,
        diagnostic: String,
        ops: Vec<AuthoredXformOpRaw>,
    }

    #[cfg(feature = "authoring")]
    #[derive(Debug, Clone)]
    struct VariantLayerStateRaw {
        root_selection: String,
        session_selection: String,
    }

    #[cfg(feature = "authoring")]
    #[derive(Debug, Clone)]
    struct VariantSelectionEditRaw {
        path: String,
        set_name: String,
        selection: String,
    }

    #[cfg(feature = "authoring")]
    #[derive(Debug, Clone)]
    struct VariantLayerEditRaw {
        path: String,
        set_name: String,
        root_selection: String,
        session_selection: String,
    }

    #[cfg(feature = "authoring")]
    #[derive(Debug, Clone)]
    struct PersistenceResultRaw {
        destination: String,
        file_format: String,
        package_members: Vec<String>,
        dependencies: Vec<String>,
        warnings: Vec<String>,
    }

    /// Shared packaging policy; runtime cooks opt into source stripping.
    #[derive(Debug, Clone)]
    struct PackagingOptionsRaw {
        strip_behavior_sources: bool,
    }

    #[derive(Debug, Clone)]
    struct PackageResultRaw {
        destination: String,
        dependencies: Vec<String>,
        unresolved: Vec<String>,
        /// Remote (`http(s)://`) references deliberately kept EXTERNAL: left
        /// authored in the produced package and never fetched/embedded. Only
        /// populated by `package_stage_keeping_remote`; empty for the other
        /// package/dependency calls.
        external: Vec<String>,
        /// Diagnostics (errors/warnings/status) emitted by `OpenUSD` during
        /// packaging, captured via a scoped `TfDiagnosticMgr` delegate. Empty
        /// for `compute_dependencies` (no packaging is performed there).
        warnings: Vec<String>,
    }

    /// Datasource configuration read off an IDTX datasource prim, handed to the
    /// host driver. `kind` is `"rest"` (fetch over HTTP) or `"mock"` (generate
    /// random JSON locally); both feed the same enqueue path off the stage thread.
    #[cfg(feature = "openexec")]
    #[derive(Debug, Clone)]
    struct ExecDataSourceRaw {
        prim_path: String,
        kind: String,
        endpoint: String,
        query: String,
        method: String,
        authorization: String,
        json_body: String,
        interval: f32,
    }

    /// One resolved datasource INPUT value (a connected `authorization` /
    /// `jsonBody` driven by a `Compute_*` node), delivered to the host poller
    /// after each `tick_exec` WITHOUT authoring it onto the stage. This is how an
    /// environment-provider value (e.g. an auth token) reaches the REST request:
    /// the connected input is a time-dependent value key, recomputed every tick.
    #[cfg(feature = "openexec")]
    #[derive(Debug, Clone)]
    struct ExecDataSourceInputRaw {
        prim_path: String,
        attribute_name: String,
        value: String,
    }

    #[derive(Debug, Clone)]
    struct ChangeRecordRaw {
        prim_path: String,
        property_name: String,
        domains: u32,
        resync: bool,
        /// True when the authored opinion for this change lives in the stage's
        /// root layer (vs. the session layer or a sublayer). Used by the
        /// collaboration sync to distinguish root-layer edits from session-only
        /// changes (variant selection, visibility overrides) which must not be
        /// broadcast.
        authored_in_root_layer: bool,
    }

    /// Generic attribute value extracted by the escape-hatch eval path.
    /// `value_json` is an `OpenUSD`-formatted string representation of the
    /// `VtValue`; callers may feed it into a JSON/string parser. Empty when
    /// `has_value` is false (attribute has no authored or fallback value at
    /// the requested time).
    #[derive(Debug, Clone)]
    struct AttributeValueRaw {
        type_name: String,
        value_json: String,
        has_value: bool,
        is_time_varying: bool,
        time_sample_count: u64,
    }

    // ── skel_summaries bulk structs ───────────────────────────────────

    /// Compact skeleton identity returned at hydration time without full pose
    /// sampling. Used to pre-allocate GPU palettes and establish binding maps.
    #[derive(Debug, Clone)]
    struct SkeletonSummaryRaw {
        path: String,
        joint_count: u32,
        has_anim_query: bool,
        animation_path: String,
    }

    /// Compact skin-binding summary for a single skinned prim.
    #[derive(Debug, Clone)]
    struct SkinBindingSummaryRaw {
        path: String,
        skeleton_path: String,
        point_count: u64,
        skinning_method: String,
        influences_per_point: u32,
    }

    /// Compact animation clip summary for determining time ranges before full
    /// UsdSkel sampling.
    #[derive(Debug, Clone)]
    struct AnimationSummaryRaw {
        path: String,
        skeleton_path: String,
        start_time_code: f64,
        end_time_code: f64,
        has_translations: bool,
        has_rotations: bool,
        has_scales: bool,
        has_blend_shapes: bool,
    }

    /// All three summary arrays returned by a single `skel_summaries()` call.
    #[derive(Debug, Clone)]
    struct SkelSummariesRaw {
        skeletons: Vec<SkeletonSummaryRaw>,
        skin_bindings: Vec<SkinBindingSummaryRaw>,
        animations: Vec<AnimationSummaryRaw>,
    }

    // ── layer-stack visualization struct ─────────────────────────────

    /// One layer in the stage's composed layer stack, returned strongest-first
    /// by `layer_stack()`. Read-only, requested on demand (never per frame).
    ///
    /// The list is a pre-order walk of the sublayer tree: the session layer and
    /// the root layer sit at `depth == 0`; each layer's own sublayers follow it
    /// at `depth + 1` in strength order (earlier sublayer = stronger). `offset`
    /// and `scale` are the [`SdfLayerOffset`] applied by the *parent* sublayer
    /// arc (identity `0.0` / `1.0` for the session and root layers themselves).
    #[derive(Debug, Clone)]
    struct LayerRecordRaw {
        /// Full layer identifier (file path, URI, or `anon:0x…` for anonymous).
        identifier: String,
        /// Human-readable display name (`SdfLayer::GetDisplayName`).
        display_name: String,
        /// Resolved real filesystem path; empty for anonymous/session layers.
        real_path: String,
        /// Which arc introduced this layer: `session`, `root`, `sublayer`,
        /// `reference`, or `payload`.
        arc: String,
        /// For `reference`/`payload` rows: the introducing prim path in the
        /// referencing layer stack (e.g. `/World/Pawn`). Empty for local rows.
        arc_source_path: String,
        /// For `reference`/`payload` rows: the target prim path inside the
        /// referenced layer (e.g. `/Pawn`). Empty for local rows.
        arc_target_path: String,
        /// Whether this arc's node contributes prim specs (`HasSpecs`). Always
        /// true for local session/root/sublayer rows.
        has_specs: bool,
        /// Whether the arc's content is loaded. Always true for references and
        /// local layers; false for a deferred (unloaded) payload.
        is_loaded: bool,
        /// Nesting depth for indentation (session/root are 0).
        depth: u32,
        /// `SdfLayerOffset` offset from the parent sublayer arc.
        offset: f64,
        /// `SdfLayerOffset` time scale from the parent sublayer arc.
        scale: f64,
        /// Number of prim specs this layer contributes (the `Δ` badge).
        delta_count: u32,
        is_anonymous: bool,
        is_dirty: bool,
        is_muted: bool,
        is_session: bool,
        is_root: bool,
        /// True when this layer is the stage's current edit target.
        is_edit_target: bool,
        /// Identifier of the layer that directly introduced this layer (empty
        /// for the session and root layers which have no parent arc).
        /// For sublayers: the containing layer's identifier.
        /// For reference/payload arcs: the layer that authors the arc
        /// (`SdfPrimSpec::GetLayer()`), i.e. the anchor layer.
        parent_identifier: String,
    }

    // ── material WGSL dual-path struct ────────────────────────────────

    /// WGSL source for a single material surface, produced either by the C++
    /// `WgslShaderGenerator` (when `IDTX_HAVE_MATERIALX_WGSL` is defined at
    /// build time) or as a GLSL stub for the naga transpile fallback. An empty
    /// `wgsl_source` means neither path was available; use the Material IR
    /// scalar factors instead.
    #[derive(Debug, Clone)]
    struct WgslMaterialRaw {
        material_path: String,
        /// Phase-1 WGSL source (evaluate_surface entry point only).
        wgsl_source: String,
        /// True when phase-2 (evaluate_displacement) requires a separate call.
        requires_phase2: bool,
        /// `(wgsl_source_hash, pipeline_descriptor_hash)` for pipeline caching.
        fingerprint: String,
        diagnostics: Vec<String>,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct RelationshipBindingRaw {
        name: String,
        resolved: bool,
        targets: Vec<String>,
        statuses: Vec<u8>,
        dependencies: Vec<String>,
    }

    extern "Rust" {
        /// Bulk-copy a byte slice into a `Vec<u8>` via a single `memcpy`.
        ///
        /// Called from C++ `read_asset` / `read_asset_global` to avoid the
        /// `rust::Vec::push_back` loop (O(n) function calls in debug builds).
        fn idtx_copy_bytes(src: &[u8]) -> Vec<u8>;
    }

    unsafe extern "C++" {
        include!("idtx-usd-sys/include/usd_shim.h");

        #[allow(missing_debug_implementations)]
        type StageHandle;

        fn relationship_bindings(stage: &StageHandle, path: &str) -> Result<Vec<RelationshipBindingRaw>>;
        #[cfg(feature = "authoring")]
        fn set_relationship_targets(stage: Pin<&mut StageHandle>, path: &str, name: &str, targets: &[String]) -> Result<()>;

        fn register_plugins(install_dir: &str) -> Result<()>;
        /// Process-wide count of live stage handles. Incremented on open,
        /// decremented at the end of teardown (after observer revocation +
        /// stage reset under the lifecycle lock). Lets a test assert that
        /// opens are balanced by full teardowns — i.e. no handle leaks.
        #[must_use]
        fn live_stage_handle_count() -> u64;
        fn compute_dependencies(source: &str) -> Result<PackageResultRaw>;
        fn package_stage(
            source: &str,
            destination: &str,
            options: &PackagingOptionsRaw,
        ) -> Result<PackageResultRaw>;
        /// "Import anyway" packaging: create a verified `.usdz` from `source`
        /// while DROPPING references that cannot be resolved (missing textures,
        /// references, payloads, shader-source URIs), instead of refusing like
        /// `package_stage`. The on-disk source files are never modified — the
        /// unresolved asset paths are cleared from in-memory copies of the
        /// dependency-closure layers before packaging. `unresolved` reports the
        /// references that were dropped.
        fn package_stage_dropping_unresolved(
            source: &str,
            destination: &str,
            options: &PackagingOptionsRaw,
        ) -> Result<PackageResultRaw>;
        /// Package `source` into a verified `.usdz` at `destination`, LOCALIZING
        /// every local reference (search-path / absolute / relative) to a
        /// package-internal path — exactly as `package_stage` does — while
        /// PRESERVING remote (`http(s)://`) references verbatim. The preserved
        /// remote URLs are returned in `external`. This is the cook's packager for
        /// stages that mix local assets (which must be package-internalized to
        /// resolve on another machine) with remote assets (which must stay URLs).
        fn package_stage_localize_keeping_remote(
            source: &str,
            destination: &str,
            options: &PackagingOptionsRaw,
        ) -> Result<PackageResultRaw>;
        fn open_stage(path: &str, load_payloads: bool) -> Result<UniquePtr<StageHandle>>;
        /// Opens `path` as the root layer with `session_layer_text` composed in
        /// as the stage's strongest (session) layer, via the 3-arg
        /// `UsdStage::Open(root, session, loadset)`. Empty text opens with an
        /// empty session layer. The session layer is anchored to the root
        /// layer's directory, so relative asset paths (`@./controller.usdz@`)
        /// inside the text resolve exactly as they would from the root layer.
        fn open_stage_with_session(
            path: &str,
            session_layer_text: &str,
            load_payloads: bool,
        ) -> Result<UniquePtr<StageHandle>>;
        /// Creates a brand-new stage on disk at `path`, authoring a default root
        /// prim (`default_prim_name`, of type `default_prim_type` — empty type ⇒
        /// typeless; empty name ⇒ no root prim / no `defaultPrim`), setting the
        /// stage up-axis (`"Y"`/`"Z"`, defaulting to `Y`) and `metersPerUnit`
        /// (when positive), saving it, and returning an opened handle over it.
        fn create_new_stage(
            path: &str,
            up_axis: &str,
            meters_per_unit: f64,
            default_prim_name: &str,
            default_prim_type: &str,
        ) -> Result<UniquePtr<StageHandle>>;
        fn stage_metadata(stage: &StageHandle) -> Result<StageMetadataRaw>;
        fn validate_runtime_stage(stage: &StageHandle) -> Result<()>;
        /// Reads the IDTX stage-kind tag from the **root layer alone** of the
        /// `.usd(a/c/z)` file at `path`, without composing a stage or loading
        /// payloads (opens just the `SdfLayer`). Returns an all-empty
        /// [`StageKindRaw`] when the file has no `customLayerData.idtx` tag.
        fn read_stage_kind(path: &str) -> Result<StageKindRaw>;
        /// Reads the `defaultPrim` and root-level `def` prim names from the
        /// **root layer alone** of the `.usd(a/c/z)` file at `path`, without
        /// composing a stage or loading payloads (opens just the `SdfLayer`).
        /// Returns an all-empty [`StageRootPrimsRaw`] when the file cannot be
        /// opened as a layer.
        fn read_stage_root_prims(path: &str) -> Result<StageRootPrimsRaw>;
        /// Overrides the process-wide default asset-resolver search path with
        /// the newline-separated absolute directories in `search_path` (empty
        /// segments ignored). This is the `ArDefaultResolver` fallback used to
        /// resolve bare/project-relative asset references (e.g. `@prop.usda@`)
        /// against a project's asset roots. Static and process-global — it must
        /// be serialized against stage opens/mutations (the shim holds the
        /// exclusive lifecycle lock) and overrides `PXR_AR_DEFAULT_SEARCH_PATH`.
        fn set_default_asset_search_path(search_path: &str) -> Result<()>;
        fn project_topology(
            stage: &StageHandle,
            roots: &[String],
        ) -> Result<Vec<TopologyRecordRaw>>;
        fn project_viewport(
            stage: &StageHandle,
            time_code: f64,
            use_default_time: bool,
            roots: &[String],
        ) -> Result<ViewportProjectionRaw>;

        fn inspect_point_instancer(stage: &StageHandle, path: &str, time_code: f64,
            use_default_time: bool) -> Result<PointInstancerInspectionRaw>;
        fn session_layer_text(stage: &StageHandle) -> Result<String>;
        #[cfg(feature = "test-support")]
        fn conformance_fail_instancer_after(stage: Pin<&mut StageHandle>, writes: u32) -> Result<()>;
        fn sample_point_instancers(stage: &StageHandle, paths: &[String], time_code: f64,
            use_default_time: bool, mask_only: bool) -> Result<PointInstancerSamplesRaw>;
        #[cfg(feature = "authoring")]
        fn update_point_instancer(stage: Pin<&mut StageHandle>, path: &str,
            update: &PointInstancerUpdateRaw, mask_only: bool, session_layer: bool) -> Result<()>;

        fn read_animation_clips(stage: &StageHandle, owner: &str) -> Result<Vec<AnimationClipRaw>>;
        fn prepare_animation_binding(stage: &StageHandle, key: &str, owner: &str,
            source: &str, roots: &[String]) -> Result<()>;
        fn release_animation_binding(stage: &StageHandle, key: &str) -> Result<()>;
        fn project_animation_binding(stage: &StageHandle, key: &str,
            time_code: f64, placement_time_code: f64, mode: u32) -> Result<ViewportProjectionRaw>;

        #[cfg(feature = "authoring")]
        fn bake_curve_placements(stage: Pin<&mut StageHandle>, bake: &CurveBakeRaw) -> Result<String>;
        fn curve_scene_complete(stage: &StageHandle) -> Result<bool>;
        fn read_curve_bake(stage: &StageHandle, owner: &str) -> Result<CurveBakeRaw>;
        fn curve_obstacles(stage: &StageHandle) -> Result<Vec<CurveObstacleRaw>>;
        fn project_skeleton_poses(
            stage: &StageHandle,
            time_code: f64,
            use_default_time: bool,
            roots: &[String],
        ) -> Result<ViewportProjectionRaw>;
        fn project_transforms_at(
            stage: &StageHandle,
            time_code: f64,
            use_default_time: bool,
            prim_paths: &[String],
        ) -> Result<Vec<TransformRecordRaw>>;
        fn inspect_prim(stage: &StageHandle, path: &str) -> Result<Vec<PropertyRecordRaw>>;
        /// Resolve the `asset`-typed attribute `attr_name` on the prim at
        /// `prim_path` to an absolute filesystem path, using USD's own asset
        /// resolution (`SdfAssetPath::GetResolvedPath`) and falling back to
        /// anchoring the authored path against the strongest opinion's layer.
        /// Returns an empty string when the prim/attribute is absent, not an
        /// asset, or unset — so a relative source path resolves the same way
        /// regardless of whether the prim is reached directly or through a
        /// composed (payloaded/referenced) assembly.
        fn resolve_prim_asset(
            stage: &StageHandle,
            prim_path: &str,
            attr_name: &str,
        ) -> Result<String>;
        fn material_graph(stage: &StageHandle, path: &str) -> Result<MaterialGraphRaw>;
        fn compute_graph(stage: &StageHandle, path: &str) -> Result<ComputeGraphRaw>;
        fn variant_sets(stage: &StageHandle, path: &str) -> Result<Vec<VariantSetRaw>>;
        fn get_local_transform(stage: &StageHandle, path: &str) -> Result<Vec<f32>>;
        fn get_world_transform(stage: &StageHandle, path: &str) -> Result<Vec<f32>>;
        fn get_transform_matrix(stage: &StageHandle, path: &str, world: bool) -> Result<Vec<f64>>;
        fn set_session_variant(
            stage: Pin<&mut StageHandle>,
            path: &str,
            set_name: &str,
            selection: &str,
        ) -> Result<()>;
        #[cfg(feature = "authoring")]
        fn authored_xform_stack(stage: &StageHandle, path: &str) -> Result<AuthoredXformStackRaw>;
        #[cfg(feature = "authoring")]
        fn apply_transform_edit(
            stage: Pin<&mut StageHandle>,
            path: &str,
            translate: &[f64],
            rotate_xyz: &[f64],
            scale: &[f64],
        ) -> Result<()>;
        #[cfg(feature = "authoring")]
        fn apply_matrix_transform(stage: Pin<&mut StageHandle>, path: &str, matrix: &[f64]) -> Result<()>;
        /// Authors a full 4×4 pose matrix onto `path`'s `xformOp:transform` in
        /// the stage's **session** layer. `matrix` is 16 f64 in glam
        /// column-major order (see the safe wrapper). Used by the XR controller
        /// pose channel to drive the pose Xform each frame.
        #[cfg(feature = "authoring")]
        fn author_session_transform(
            stage: Pin<&mut StageHandle>,
            path: &str,
            matrix: &[f64],
        ) -> Result<()>;
        /// Bulk variant of `author_session_transform`: authors `paths.len()` pose
        /// matrices onto `paths.len()` prim paths under ONE stage-lifecycle lock,
        /// ONE `SdfChangeBlock`, and ONE edit context — the per-frame physics
        /// write-back path (one notice batch + one reprojection, honouring the
        /// "bulk, not per-prim FFI" invariant). `matrices` is `16 * paths.len()`
        /// f64 in glam column-major order; path `i` consumes
        /// `matrices[16*i .. 16*i + 16]`. Checkpoint/rollback as a unit on any
        /// failure (the change block closes before any throw — the toolchain
        /// skips destructors on unwind).
        #[cfg(feature = "authoring")]
        fn author_session_transforms(
            stage: Pin<&mut StageHandle>,
            paths: &[String],
            matrices: &[f64],
        ) -> Result<()>;
        /// Overrides `path`'s `visibility` in the stage's **session** layer
        /// (`inherited` when visible, else `invisible`). The prim must already
        /// compose from a weaker layer. Used to reveal the invisible controller
        /// proxy for the normal-hydration XR path without touching the root.
        ///
        /// A path that does not compose, or composes to a non-imageable
        /// (typeless) prim, is a **soft no-op** (returns `Ok`): a session
        /// visibility opinion cannot attach to such a prim, and the ECS write
        /// is authoritative for rendering (see `set_controller_hydrated`). Only
        /// genuine authoring failures (missing session layer, failed write)
        /// return `Err`.
        #[cfg(feature = "authoring")]
        fn author_session_visibility(
            stage: Pin<&mut StageHandle>,
            path: &str,
            visible: bool,
        ) -> Result<()>;
        /// Defines a brand-new typed prim into the stage's **session** layer
        /// (empty `type_name` = typeless). Errors if the path already composes.
        /// Runtime-only prims (player camera, dynamic overlays, spawned actors)
        /// are authored here so they never dirty the saved assembly. The prim's
        /// `PrimId` is minted by the next topology projection, not returned here.
        #[cfg(feature = "authoring")]
        fn define_session_prim(
            stage: Pin<&mut StageHandle>,
            path: &str,
            type_name: &str,
        ) -> Result<()>;
        /// Authors a scalar/asset/token/string/bool/int2 property onto an
        /// existing prim in the stage's **session** layer. `value` is
        /// string-encoded exactly like the structural `set-property-*` path
        /// (int2 as `"x y"`, float as its decimal form, bool as
        /// `"true"`/`"false"`); `kind` ∈ {`"asset"`, `"token"`, `"string"`,
        /// `"bool"`, `"float"`, `"int2"`}. The prim must already compose (define
        /// it first with `define_session_prim`). Used by behaviors to author
        /// HUD / overlay / widget parameters that must never dirty the saved
        /// assembly.
        #[cfg(feature = "authoring")]
        fn author_session_property(
            stage: Pin<&mut StageHandle>,
            path: &str,
            attr: &str,
            kind: &str,
            value: &str,
        ) -> Result<()>;
        /// Authors the stage shadow mode into the **root** layer's
        /// `customLayerData["idtx:shadowMode"]` token (`0`→off, `1`→hard,
        /// `2`→smooth, `3`→soft — see `ShadowMode`). Erases the legacy
        /// `idtx:softShadows` bool so there is a single source of truth.
        /// Checkpoint/rollback on failure. Read back by `stage_metadata`.
        #[cfg(feature = "authoring")]
        fn author_root_shadow_mode(stage: Pin<&mut StageHandle>, mode: i32) -> Result<()>;
        /// Authors the stage-level behavior assignment into the **root** layer's
        /// `customLayerData`: `idtx:stageBehavior` (the prim name, e.g.
        /// `"StageBehavior"`) and `idtx:stageBehaviorRunScope` (the run-condition
        /// token). Merges into existing `customLayerData`, leaving other keys
        /// intact. Checkpoint/rollback on failure. Read back by `stage_metadata`.
        #[cfg(feature = "authoring")]
        fn author_root_stage_behavior(
            stage: Pin<&mut StageHandle>,
            name: &str,
            run_scope: &str,
        ) -> Result<()>;
        /// Erases both `idtx:stageBehavior` and `idtx:stageBehaviorRunScope` from
        /// the root layer's `customLayerData`. The counterpart of
        /// `author_root_stage_behavior`; leaves other keys intact.
        #[cfg(feature = "authoring")]
        fn clear_root_stage_behavior(stage: Pin<&mut StageHandle>) -> Result<()>;
        /// Classify how the layer carrying `idtx:stageBehavior` is composed into
        /// the open stage, for the runtime run-condition gate:
        /// `0`=standalone (that layer is the stage root), `1`=payload, `2`=sublayer,
        /// `3`=reference, `4`=unknown/none. See `StageComposeContext`.
        fn stage_behavior_compose_context(stage: &StageHandle) -> Result<i32>;
        /// Resolve the `idtx:stageBehaviorRunScope` token from the layer that
        /// carries `idtx:stageBehavior` — i.e. the layer (usually a payloaded
        /// level) that actually *owns* the stage behavior, found with the same
        /// carrying-layer walk as `stage_behavior_compose_context`. This is what a
        /// composed assembly needs: its own root layer has no run-scope opinion,
        /// so reading run scope off the open stage's metadata would always see the
        /// default. Empty string when no stage behavior is assigned. Parsed by
        /// `StageBehaviorRunScope::from_token`.
        fn stage_behavior_run_scope_resolved(stage: &StageHandle) -> Result<String>;
        /// Per-prim stage-behavior resolution: resolve the owning-layer run scope
        /// and compose context for the stage behavior at `prim_path` specifically,
        /// rather than the first carrying layer in the stage. An assembly can
        /// surface several levels' stage behaviors, each owned by a different layer
        /// with its own run scope; this lets the attach gate decide per behavior
        /// instead of applying one global permit to all of them. `run_scope` is the
        /// owning layer's `idtx:stageBehaviorRunScope` token (empty when unauthored
        /// — caller falls back to the cached metadata value); `compose_context` is
        /// the carrier (`0`=standalone, `1`=payload, `2`=sublayer, `3`=reference,
        /// `4`=unknown). Parsed by `StageBehaviorRunScope::from_token` /
        /// `StageComposeContext::from_i32`.
        fn stage_behavior_resolved_for_prim(
            stage: &StageHandle,
            prim_path: &str,
        ) -> Result<StageBehaviorResolvedRaw>;
        // cxx-build receives hyphenated Cargo feature names with underscores,
        // so generate the internal bridge whenever authoring is enabled. The
        // safe Rust injection seam remains gated by `test-support`.
        #[cfg(feature = "authoring")]
        fn inject_transform_edit_failure_after_writes(
            stage: Pin<&mut StageHandle>,
            successful_writes: u32,
        ) -> Result<()>;
        #[cfg(feature = "authoring")]
        fn variant_layer_state(
            stage: &StageHandle,
            path: &str,
            set_name: &str,
        ) -> Result<VariantLayerStateRaw>;
        #[cfg(feature = "authoring")]
        fn set_variant_layer_state(
            stage: Pin<&mut StageHandle>,
            path: &str,
            set_name: &str,
            root_selection: &str,
            session_selection: &str,
        ) -> Result<()>;
        #[cfg(feature = "authoring")]
        fn manifest_variant(
            stage: Pin<&mut StageHandle>,
            path: &str,
            set_name: &str,
            selection: &str,
        ) -> Result<()>;
        #[cfg(feature = "authoring")]
        fn manifest_variants(
            stage: Pin<&mut StageHandle>,
            edits: &[VariantSelectionEditRaw],
        ) -> Result<()>;
        #[cfg(feature = "authoring")]
        fn set_variant_layer_states(
            stage: Pin<&mut StageHandle>,
            edits: &[VariantLayerEditRaw],
        ) -> Result<()>;
        #[cfg(feature = "authoring")]
        fn root_layer_text(stage: &StageHandle) -> Result<String>;
        #[cfg(feature = "authoring")]
        fn root_prim_text(stage: &StageHandle, path: &str) -> Result<String>;
        #[cfg(feature = "authoring")]
        fn session_variant_opinion(stage: &StageHandle, path: &str, set: &str) -> Result<VariantOpinionRaw>;
        #[cfg(feature = "authoring")]
        fn restore_session_variant_opinion(stage: Pin<&mut StageHandle>, path: &str, set: &str, authored: bool, value: &str) -> Result<()>;
        #[cfg(feature = "authoring")]
        fn root_transform_text(stage: &StageHandle, path: &str) -> Result<String>;
        #[cfg(feature = "authoring")]
        fn mutate_session_prim(stage: Pin<&mut StageHandle>, path: &str, type_name: &str, remove: bool) -> Result<()>;
        /// Erase the session spec of a runtime-owned subtree, including its
        /// external reference, rather than masking it with active=false.
        #[cfg(feature = "authoring")]
        fn erase_session_prim(stage: Pin<&mut StageHandle>, path: &str) -> Result<()>;
        /// Transactional ephemeral reference composition. All property writes
        /// and connections are rolled back together; notices remain queued.
        #[cfg(feature = "authoring")]
        fn spawn_session_reference(
            stage: Pin<&mut StageHandle>, path: &str, asset: &str, target: &str,
            matrix: &[f64], context: &str, children: &[String],
            properties: &[String], sources: &[String],
        ) -> Result<()>;
        #[cfg(feature = "authoring")]
        fn test_stage_access_stress(stage: Pin<&mut StageHandle>, peer: Pin<&mut StageHandle>, iterations: u32) -> Result<()>;
        #[cfg(feature = "authoring")]
        fn replace_root_prim_text(stage: Pin<&mut StageHandle>, path: &str, source: &str) -> Result<()>;
        #[cfg(feature = "authoring")]
        fn reconcile_root_prim(stage: Pin<&mut StageHandle>, path: &str, type_name: &str, asset: &str, payload: bool) -> Result<()>;
        /// Authors the IDTX stage-kind tag into the stage's **root-layer**
        /// `customLayerData` under the `idtx` dictionary (`stageKind`,
        /// `schemaVersion`, and — when non-empty — `displayName`). Replaces any
        /// existing `idtx` entry; leaves other `customLayerData` keys intact.
        #[cfg(feature = "authoring")]
        fn set_root_stage_kind(
            stage: Pin<&mut StageHandle>,
            stage_kind: &str,
            display_name: &str,
        ) -> Result<()>;
        #[cfg(feature = "authoring")]
        fn replace_root_layer_text(stage: Pin<&mut StageHandle>, source: &str) -> Result<()>;
        #[cfg(feature = "authoring")]
        fn apply_root_layer_delta(stage: Pin<&mut StageHandle>, expected: &str, desired: &str) -> Result<()>;
        #[cfg(feature = "authoring")]
        fn discard_pending_changes(stage: Pin<&mut StageHandle>) -> Result<()>;
        #[cfg(feature = "authoring")]
        fn root_property_text(stage: &StageHandle, path: &str, property: &str) -> Result<String>;
        #[cfg(feature = "authoring")]
        fn root_attribute_metadata_delta(
            path: &str,
            property: &str,
            expected: &str,
            desired: &str,
        ) -> Result<RootAttributeMetadataDeltaRaw>;
        #[cfg(feature = "authoring")]
        fn read_sync_attribute(stage: &StageHandle, path: &str, property: &str) -> Result<SyncAttributeRaw>;
        #[cfg(feature = "authoring")]
        fn root_attribute_snapshots(stage: &StageHandle, transforms_only: bool) -> Result<Vec<RootAttributeSnapshotRaw>>;
        #[cfg(feature = "authoring")]
        fn apply_sync_attributes(
            stage: Pin<&mut StageHandle>,
            path: &str,
            attributes: &[SyncAttributeRaw],
        ) -> Result<()>;
        #[cfg(feature = "authoring")]
        fn replace_root_property_text(
            stage: Pin<&mut StageHandle>,
            path: &str,
            property: &str,
            source: &str,
        ) -> Result<()>;
        #[cfg(feature = "authoring")]
        fn apply_structural_edit(
            stage: Pin<&mut StageHandle>,
            kind: &str,
            path: &str,
            destination: &str,
            type_name: &str,
            asset_path: &str,
        ) -> Result<()>;
        #[cfg(feature = "authoring")]
        fn author_targeted_property(
            stage: Pin<&mut StageHandle>,
            path: &str,
            edit_target: &str,
            set_name: &str,
            variant_name: &str,
            kind: &str,
            property: &str,
            value: &str,
        ) -> Result<()>;
        #[cfg(feature = "authoring")]
        fn apply_layer_edit(
            stage: Pin<&mut StageHandle>,
            kind: &str,
            identifier: &str,
            parent_identifier: &str,
            direction: i32,
        ) -> Result<()>;
        #[cfg(feature = "authoring")]
        fn save_stage(stage: &StageHandle, destination: &str) -> Result<PersistenceResultRaw>;
        #[cfg(feature = "authoring")]
        fn export_root_layer(
            stage: &StageHandle,
            destination: &str,
        ) -> Result<PersistenceResultRaw>;
        #[cfg(feature = "authoring")]
        fn export_flattened_stage(
            stage: &StageHandle,
            destination: &str,
            add_source_file_comment: bool,
        ) -> Result<PersistenceResultRaw>;
        #[cfg(feature = "authoring")]
        fn verify_persistence(
            stage: &StageHandle,
            persisted: &StageHandle,
            mode: &str,
        ) -> Result<()>;
        fn set_payload_loaded(stage: Pin<&mut StageHandle>, path: &str, loaded: bool)
        -> Result<()>;
        fn read_asset(stage: &StageHandle, resolved_path: &str) -> Result<Vec<u8>>;
        fn read_asset_global(resolved_path: &str) -> Result<Vec<u8>>;
        fn read_asset_global_bounded(resolved_path: &str, max_bytes: u64) -> Result<Vec<u8>>;
        fn get_prim_authored_visibility(stage: &StageHandle, path: &str) -> Result<bool>;
        fn drain_changes(stage: Pin<&mut StageHandle>) -> Result<Vec<ChangeRecordRaw>>;
        fn conformance_author_double(
            stage: Pin<&mut StageHandle>,
            property_path: &str,
            value: f64,
        ) -> Result<()>;
        fn conformance_author_double_at_time(
            stage: Pin<&mut StageHandle>,
            property_path: &str,
            value: f64,
            time_code: f64,
        ) -> Result<()>;
        fn conformance_author_vec3f_array_at_time(
            stage: Pin<&mut StageHandle>,
            property_path: &str,
            values: &[f32],
            time_code: f64,
        ) -> Result<()>;
        fn conformance_set_relationship_target(
            stage: Pin<&mut StageHandle>,
            relationship_path: &str,
            target_path: &str,
        ) -> Result<()>;
        fn conformance_set_custom_data(
            stage: Pin<&mut StageHandle>,
            prim_path: &str,
            key: &str,
            value: &str,
        ) -> Result<()>;
        fn conformance_define_xform(stage: Pin<&mut StageHandle>, path: &str) -> Result<()>;

        // ── Uber-implementation additions ─────────────────────────────────────

        /// Registers additional `OpenUSD` plugin paths at runtime without
        /// requiring an application restart. Enables pre-built schema libraries
        /// (e.g. custom IsA / HasAPI schemas) to be discovered after the initial
        /// `configure_plugin_root` call.
        fn register_plugin_path(path: &str) -> Result<()>;

        /// Reloads a specific `SdfLayer` from disk (or the root layer when
        /// `layer_path` is empty) and lets the `TfNotice::ObjectsChanged`
        /// system cascade normally. This is the correct trigger for external
        /// file-system or network change events.
        fn reload_layer(stage: Pin<&mut StageHandle>, layer_path: &str) -> Result<()>;

        /// Returns compact skeleton / skin-binding / animation summaries in a
        /// single `OpenUSD` cache traversal. Called once at hydration time;
        /// the full per-frame skeletal data is provided by `project_viewport`.
        fn skel_summaries(stage: &StageHandle) -> Result<SkelSummariesRaw>;

        /// Returns the stage's composed layer stack (session layer + root layer
        /// + recursive sublayers), strongest-first, for the Layer Stack panel.
        /// Read-only and requested on demand — never on the per-frame path.
        fn layer_stack(stage: &StageHandle) -> Result<Vec<LayerRecordRaw>>;

        /// Whether the stage's **root layer** currently holds unsaved edits
        /// (`SdfLayer::IsDirty`). Cheap — a single bool getter, no traversal and
        /// no allocation — so it is safe to poll on authoring change batches to
        /// drive the unsaved-changes ("dirty stage") indicator. Session-layer
        /// edits never dirty the root layer, so this stays `false` for runtime
        /// write-back, visibility overrides, and variant selection.
        fn root_layer_is_dirty(stage: &StageHandle) -> Result<bool>;

        /// A content signature (hash of the serialized usda text) of the stage's
        /// **root layer** authored opinions. `SdfLayer::IsDirty` stays `true` after
        /// an `Export`-based Save As (it only clears on `Save()`/`Reload()`), so the
        /// session pairs the cheap [`root_layer_is_dirty`] pre-filter with a baseline
        /// captured at save time: an equal hash means no unsaved root-layer edits
        /// since the last persist. Only the root layer's own deltas are serialized,
        /// not the composed stage, so this stays inexpensive.
        fn root_layer_content_hash(stage: &StageHandle) -> Result<u64>;
        fn root_non_transform_text(stage: &StageHandle) -> Result<String>;
        fn root_non_sync_text(stage: &StageHandle) -> Result<String>;

        /// Records the current pending-change queue length as a transactional
        /// checkpoint. Pass the returned token to `observer_rollback_to` to
        /// revert all change records accumulated since that point — useful for
        /// multi-step authoring transactions.
        fn observer_checkpoint(stage: &StageHandle) -> Result<u64>;
        /// Exact authored dictionary-key read, serialized from VtValue types.
        fn authored_custom_data_json(stage: &StageHandle, path: &str, key: &str, max_bytes: u32) -> Result<AuthoredDataRaw>;
        /// Raw authored string read (no printable-value conversion or fallback).
        fn authored_string(stage: &StageHandle, path: &str, property: &str, max_bytes: u32) -> Result<AuthoredDataRaw>;

        /// Discards all pending change records accumulated after `checkpoint`.
        /// Does not undo stage edits; only prevents spurious re-projection of
        /// changes that were deliberately rolled back at the authoring layer.
        fn observer_rollback_to(stage: Pin<&mut StageHandle>, checkpoint: u64) -> Result<()>;

        /// (Re)build the per-stage `OpenExec` program: scan prims for authored
        /// connections, register value keys, compile + activate the request.
        /// Returns the registered value-key count — `0` means the stage has no
        /// IDTX compute graph, so the host skips the whole subsystem and spawns
        /// no REST poller. Call again on topology resync to rebuild.
        #[cfg(feature = "openexec")]
        fn exec_build_program(stage: Pin<&mut StageHandle>) -> Result<u64>;

        /// REST datasource configs authored in the stage, for the host poller.
        #[cfg(feature = "openexec")]
        fn exec_data_sources(stage: &StageHandle) -> Result<Vec<ExecDataSourceRaw>>;

        /// Latest resolved datasource-input values (connected `authorization` /
        /// `jsonBody`) cached by the most recent `tick_exec`. The host reads these
        /// after each tick and feeds them into its live REST poller so an
        /// env-provider value (auth token, …) reaches the request without being
        /// authored onto the stage. Empty when no datasource input has a connection.
        #[cfg(feature = "openexec")]
        fn exec_data_source_inputs(stage: &StageHandle) -> Result<Vec<ExecDataSourceInputRaw>>;

        /// Coalescing enqueue of an external JSON `body` for `prim_path`'s
        /// `outputs:data`. No stage mutation (own queue); the next `tick_exec`
        /// applies it. Returns `false` if dropped (no active bridge / queue full).
        #[cfg(feature = "openexec")]
        fn exec_enqueue(stage: &StageHandle, prim_path: &str, body: &str) -> Result<bool>;

        /// Whether this stage's compute graph carries a time/host-state provider
        /// (`Compute_Environment`), cached at `exec_build_program`. `true` => every
        /// tick must recompute (idcm clocks / env / auth tokens change independent
        /// of datasource payloads); `false` => a purely datasource-driven graph the
        /// host may skip on frames with no pending payload — so it can avoid
        /// entering `tick_exec` (and acquiring the exclusive stage lock) on idle
        /// frames. Reads bridge state only; call once when building the subsystem.
        #[cfg(feature = "openexec")]
        fn exec_has_live_providers(stage: &StageHandle) -> Result<bool>;

        /// Apply pending external updates, run the `OpenExec` compute, and author
        /// every changed computed value into the SESSION layer. Returns the
        /// number of attributes authored this tick — including the datasource
        /// `outputs:data` payload writes, so a tick that only refreshes a payload
        /// (computed result unchanged) still reports non-zero and the caller
        /// drains the resulting notices (`0` => nothing changed, skip the drain).
        /// Holds the exclusive lifecycle lock.
        #[cfg(feature = "openexec")]
        fn tick_exec(stage: Pin<&mut StageHandle>) -> Result<u64>;

        /// Drop the per-stage exec bridge (stage teardown / topology resync).
        #[cfg(feature = "openexec")]
        fn exec_release(stage: Pin<&mut StageHandle>) -> Result<()>;

        /// Returns phase-1 WGSL source for the material at `path`. Tries the
        /// C++ `WgslShaderGenerator` path first; falls back to a GLSL stub
        /// when `IDTX_HAVE_MATERIALX_WGSL` is not defined so the naga
        /// transpile path can be used in Rust. An empty `wgsl_source` means
        /// the material uses flat PBR factors only.
        fn material_surface_wgsl(stage: &StageHandle, path: &str) -> Result<WgslMaterialRaw>;

        /// Evaluates any typed attribute at the given time by `VtValue`
        /// inspection. This is the escape hatch for custom IsA / HasAPI schemas
        /// that have no dedicated bridge function — it is intentionally generic
        /// and carries a string-serialised value that callers parse.
        fn eval_attribute(
            stage: &StageHandle,
            prim_path: &str,
            attr_name: &str,
            time_code: f64,
            use_default_time: bool,
        ) -> Result<AttributeValueRaw>;

        /// Typed read of a `point3f[]` / `float3[]` / `vector3f[]` attribute as a
        /// flat `[x,y,z, x,y,z, …]` float buffer (len = 3 × element count). The
        /// escape hatch for custom-schema array geometry (e.g. the
        /// `IdtxCollisionAPI` collision hull) that has no dedicated bridge path.
        /// Returns an empty vec when the prim/attribute is missing or unset.
        fn eval_point3f_array(
            stage: &StageHandle,
            prim_path: &str,
            attr_name: &str,
            time_code: f64,
            use_default_time: bool,
        ) -> Result<Vec<f32>>;
        fn eval_float2_array(
            handle: &StageHandle,
            prim_path: &str,
            attr_name: &str,
            time_code: f64,
            use_default_time: bool,
        ) -> Result<Vec<f32>>;

        /// Typed read of an `int[]` attribute (e.g. a flat triangle index list).
        /// Returns an empty vec when the prim/attribute is missing or unset.
        fn eval_int_array(
            stage: &StageHandle,
            prim_path: &str,
            attr_name: &str,
            time_code: f64,
            use_default_time: bool,
        ) -> Result<Vec<i32>>;

        /// Typed read of a `token[]` attribute as owned strings (e.g. the
        /// `IdtxCollisionAPI` channel list). Returns an empty vec when the
        /// prim/attribute is missing or unset.
        fn eval_token_array(
            stage: &StageHandle,
            prim_path: &str,
            attr_name: &str,
            time_code: f64,
            use_default_time: bool,
        ) -> Result<Vec<String>>;

        /// Returns `true` when the prim at `prim_path` has the named API schema
        /// applied. Resolves `schema_name` through `TfType::FindDerivedByName`
        /// so that both the C++ type name and the USD schema name work.
        fn prim_has_api(stage: &StageHandle, prim_path: &str, schema_name: &str) -> Result<bool>;

        /// Returns `true` when the prim at `prim_path` is-a instance of the
        /// named schema type. Resolves through `TfType::FindDerivedByName`.
        fn prim_is_a(stage: &StageHandle, prim_path: &str, type_name: &str) -> Result<bool>;

        // ── Animation discovery ───────────────────────────────────────────────

        /// Returns the absolute SdfPaths of all prims that have at least one
        /// time-sampled attribute. Performs a single stage traversal; call once
        /// at hydration time to build the animated-prim set.
        fn animated_prim_paths(stage: &StageHandle) -> Result<Vec<String>>;

        /// Returns the attribute names on `prim_path` that have authored time
        /// samples (`GetNumTimeSamples() > 0`).
        fn animated_attr_names(stage: &StageHandle, prim_path: &str) -> Result<Vec<String>>;

        /// Returns time-sample metadata for `attr_name` on `prim_path`.
        /// Returns an empty `AttrTimeSamplesRaw` when the attribute or prim is
        /// not found.
        fn attr_time_samples(
            stage: &StageHandle,
            prim_path: &str,
            attr_name: &str,
        ) -> Result<AttrTimeSamplesRaw>;
    }
}

// SAFETY: `StageHandle` is only exposed through `cxx::UniquePtr`, and the safe
// `idtx-usd` wrapper never aliases that ownership while it is transferred to
// another thread. OpenUSD synchronizes its process-global registries; all
// stage-local access remains exclusive through `StageService`. This permits
// ownership transfer, but deliberately does not make the handle `Sync`.
unsafe impl Send for ffi::StageHandle {}

/// Called from C++ `read_asset` / `read_asset_global` to bulk-copy an asset
/// buffer into a `Vec<u8>` via a single `memcpy`.  Exposed to C++ through the
/// `extern "Rust"` block in the cxx bridge above.
#[must_use]
pub fn idtx_copy_bytes(src: &[u8]) -> Vec<u8> {
    src.to_vec()
}
