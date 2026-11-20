#pragma once

#include "rust/cxx.h"

#include <cstdint>
#include <memory>

namespace idtx {

struct AuthoredDataRaw;
struct StageMetadataRaw;
struct VariantOpinionRaw;
struct StageBehaviorResolvedRaw;
struct StageKindRaw;
struct StageRootPrimsRaw;
struct TopologyRecordRaw;
struct TransformRecordRaw;
struct ViewportProjectionRaw;
struct PropertyRecordRaw;
struct MaterialGraphRaw;
struct ComputeGraphRaw;
struct VariantSetRaw;
#ifdef IDTX_AUTHORING
struct AuthoredXformStackRaw;
struct VariantLayerStateRaw;
struct VariantSelectionEditRaw;
struct VariantLayerEditRaw;
struct PersistenceResultRaw;
#endif
struct ChangeRecordRaw;
struct PackageResultRaw;
struct PackagingOptionsRaw;
#ifdef IDTX_OPENEXEC
struct ExecDataSourceRaw;
struct ExecDataSourceInputRaw;
#endif

struct AttributeValueRaw;
struct SkeletonSummaryRaw;
struct SkinBindingSummaryRaw;
struct AnimationSummaryRaw;
struct SkelSummariesRaw;
struct LayerRecordRaw;
struct WgslMaterialRaw;
struct AttrTimeSamplesRaw;

class StageHandle {
public:
    StageHandle();
    ~StageHandle();
    StageHandle(const StageHandle&) = delete;
    StageHandle& operator=(const StageHandle&) = delete;

private:
    struct Impl;
    std::unique_ptr<Impl> impl_;

    friend std::unique_ptr<StageHandle> open_stage(
        rust::Str path,
        bool load_payloads);
    friend std::unique_ptr<StageHandle> open_stage_with_session(
        rust::Str path,
        rust::Str session_layer_text,
        bool load_payloads);
    friend std::unique_ptr<StageHandle> create_new_stage(
        rust::Str path,
        rust::Str up_axis,
        double meters_per_unit,
        rust::Str default_prim_name,
        rust::Str default_prim_type);
    friend StageMetadataRaw stage_metadata(const StageHandle& stage);
    friend void validate_runtime_stage(const StageHandle& stage);
    friend VariantOpinionRaw session_variant_opinion(const StageHandle& stage, rust::Str path, rust::Str set);
    friend void restore_session_variant_opinion(StageHandle& stage, rust::Str path, rust::Str set, bool authored, rust::Str value);
    friend rust::Vec<TopologyRecordRaw> project_topology(
        const StageHandle& stage, rust::Slice<const rust::String> roots);
    friend ViewportProjectionRaw project_viewport(
        const StageHandle& stage, double time_code, bool use_default_time,
        rust::Slice<const rust::String> roots);
    friend ViewportProjectionRaw project_skeleton_poses(
        const StageHandle& stage, double time_code, bool use_default_time,
        rust::Slice<const rust::String> roots);
    friend rust::Vec<TransformRecordRaw> project_transforms_at(
        const StageHandle& stage, double time_code, bool use_default_time,
        rust::Slice<const rust::String> prim_paths);
    friend rust::Vec<PropertyRecordRaw> inspect_prim(const StageHandle& stage, rust::Str path);
    friend rust::String resolve_prim_asset(
        const StageHandle& stage, rust::Str prim_path, rust::Str attr_name);
    friend MaterialGraphRaw material_graph(const StageHandle& stage, rust::Str path);
    friend ComputeGraphRaw compute_graph(const StageHandle& stage, rust::Str path);
    friend rust::Vec<VariantSetRaw> variant_sets(
        const StageHandle& stage,
        rust::Str path);
    friend rust::Vec<float> get_local_transform(const StageHandle& stage, rust::Str path);
    friend rust::Vec<float> get_world_transform(const StageHandle& stage, rust::Str path);
    friend rust::Vec<double> get_transform_matrix(const StageHandle& stage, rust::Str path, bool world);
    friend void set_session_variant(
        StageHandle& stage,
        rust::Str path,
        rust::Str set_name,
        rust::Str selection);
#ifdef IDTX_AUTHORING
    friend void inject_transform_edit_failure_after_writes(
        StageHandle& stage, std::uint32_t successful_writes);
    friend AuthoredXformStackRaw authored_xform_stack(
        const StageHandle& stage, rust::Str path);
    friend void apply_transform_edit(
        StageHandle& stage, rust::Str path, rust::Slice<const double> translate,
        rust::Slice<const double> rotate_xyz, rust::Slice<const double> scale);
    friend void apply_matrix_transform(StageHandle& stage, rust::Str path, rust::Slice<const double> matrix);
    friend void author_session_transform(
        StageHandle& stage, rust::Str path, rust::Slice<const double> matrix);
    friend void author_session_transforms(
        StageHandle& stage, rust::Slice<const rust::String> paths,
        rust::Slice<const double> matrices);
    friend void author_session_visibility(
        StageHandle& stage, rust::Str path, bool visible);
    friend void define_session_prim(
        StageHandle& stage, rust::Str path, rust::Str type_name);
    friend void author_session_property(
        StageHandle& stage, rust::Str path, rust::Str attr, rust::Str kind,
        rust::Str value);
    friend void author_root_shadow_mode(StageHandle& stage, std::int32_t mode);
    friend void author_root_stage_behavior(
        StageHandle& stage, rust::Str name, rust::Str run_scope);
    friend void clear_root_stage_behavior(StageHandle& stage);
    friend VariantLayerStateRaw variant_layer_state(
        const StageHandle& stage, rust::Str path, rust::Str set_name);
    friend void set_variant_layer_state(
        StageHandle& stage, rust::Str path, rust::Str set_name,
        rust::Str root_selection, rust::Str session_selection);
    friend void manifest_variant(
        StageHandle& stage, rust::Str path, rust::Str set_name,
        rust::Str selection);
    friend void manifest_variants(
        StageHandle& stage, rust::Slice<const VariantSelectionEditRaw> edits);
    friend void set_variant_layer_states(
        StageHandle& stage, rust::Slice<const VariantLayerEditRaw> edits);
    friend rust::String root_layer_text(const StageHandle& stage);
    friend rust::String root_prim_text(const StageHandle& stage, rust::Str path);
    friend rust::String root_transform_text(const StageHandle& stage, rust::Str path);
    friend void mutate_session_prim(StageHandle& stage, rust::Str path, rust::Str type_name, bool remove);
    friend void erase_session_prim(StageHandle& stage, rust::Str path);
    friend void replace_root_prim_text(StageHandle& stage, rust::Str path, rust::Str source);
    friend void reconcile_root_prim(StageHandle& stage, rust::Str path, rust::Str type_name, rust::Str asset, bool payload);
    friend void set_root_stage_kind(
        StageHandle& stage, rust::Str stage_kind, rust::Str display_name);
    friend void replace_root_layer_text(StageHandle& stage, rust::Str source);
    friend void apply_root_layer_delta(StageHandle& stage, rust::Str expected, rust::Str desired);
    friend void discard_pending_changes(StageHandle& stage);
    friend rust::String root_property_text(
        const StageHandle& stage, rust::Str path, rust::Str property);
    friend void replace_root_property_text(
        StageHandle& stage, rust::Str path, rust::Str property, rust::Str source);
    friend void apply_structural_edit(
        StageHandle& stage, rust::Str kind, rust::Str path,
        rust::Str destination, rust::Str type_name, rust::Str asset_path);
    friend void apply_layer_edit(
        StageHandle& stage, rust::Str kind, rust::Str identifier,
        rust::Str parent_identifier, std::int32_t direction);
    friend PersistenceResultRaw save_stage(
        const StageHandle& stage, rust::Str destination);
    friend PersistenceResultRaw export_root_layer(
        const StageHandle& stage, rust::Str destination);
    friend PersistenceResultRaw export_flattened_stage(
        const StageHandle& stage, rust::Str destination, bool add_source_file_comment);
    friend void verify_persistence(
        const StageHandle& stage, const StageHandle& persisted, rust::Str mode);
#endif
    friend std::int32_t stage_behavior_compose_context(const StageHandle& stage);
    friend rust::String stage_behavior_run_scope_resolved(const StageHandle& stage);
    friend StageBehaviorResolvedRaw stage_behavior_resolved_for_prim(
        const StageHandle& stage, rust::Str prim_path);
    friend void set_payload_loaded(
        StageHandle& stage,
        rust::Str path,
        bool loaded);
    friend bool get_prim_authored_visibility(
        const StageHandle& stage,
        rust::Str path);
    friend rust::Vec<std::uint8_t> read_asset(
        const StageHandle& stage,
        rust::Str resolved_path);
    friend rust::Vec<ChangeRecordRaw> drain_changes(StageHandle& stage);
    friend void conformance_author_double(
        StageHandle& stage,
        rust::Str property_path,
        double value);
    friend void conformance_author_double_at_time(
        StageHandle& stage, rust::Str property_path, double value,
        double time_code);
    friend void conformance_author_vec3f_array_at_time(
        StageHandle& stage, rust::Str property_path,
        rust::Slice<const float> values, double time_code);
    friend void conformance_set_relationship_target(
        StageHandle& stage, rust::Str relationship_path,
        rust::Str target_path);
    friend void conformance_set_custom_data(
        StageHandle& stage, rust::Str prim_path, rust::Str key,
        rust::Str value);
    friend void conformance_define_xform(StageHandle& stage, rust::Str path);
    friend SkelSummariesRaw skel_summaries(const StageHandle& stage);
    friend rust::Vec<LayerRecordRaw> layer_stack(const StageHandle& stage);
    friend bool root_layer_is_dirty(const StageHandle& stage);
    friend std::uint64_t root_layer_content_hash(const StageHandle& stage);
    friend rust::String root_non_transform_text(const StageHandle& stage);
    friend std::uint64_t observer_checkpoint(const StageHandle& stage);
    friend AuthoredDataRaw authored_custom_data_json(const StageHandle& stage, rust::Str path, rust::Str key, std::uint32_t max_bytes);
    friend AuthoredDataRaw authored_string(const StageHandle& stage, rust::Str path, rust::Str property, std::uint32_t max_bytes);
#ifdef IDTX_AUTHORING
    friend void spawn_session_reference(StageHandle& stage, rust::Str path, rust::Str asset, rust::Str target, rust::Slice<const double> matrix, rust::Str context, rust::Slice<const rust::String> children, rust::Slice<const rust::String> properties, rust::Slice<const rust::String> sources);
#endif
    friend void observer_rollback_to(StageHandle& stage, std::uint64_t checkpoint);
#ifdef IDTX_OPENEXEC
    friend std::uint64_t exec_build_program(StageHandle& stage);
    friend rust::Vec<ExecDataSourceRaw> exec_data_sources(const StageHandle& stage);
    friend rust::Vec<ExecDataSourceInputRaw> exec_data_source_inputs(const StageHandle& stage);
    friend bool exec_enqueue(const StageHandle& stage, rust::Str prim_path, rust::Str body);
    friend bool exec_has_live_providers(const StageHandle& stage);
    friend std::uint64_t tick_exec(StageHandle& stage);
    friend void exec_release(StageHandle& stage);
#endif
    friend WgslMaterialRaw material_surface_wgsl(const StageHandle& stage, rust::Str path);
    friend AttributeValueRaw eval_attribute(
        const StageHandle& stage, rust::Str prim_path, rust::Str attr_name,
        double time_code, bool use_default_time);
    friend rust::Vec<float> eval_point3f_array(
        const StageHandle& stage, rust::Str prim_path, rust::Str attr_name,
        double time_code, bool use_default_time);
    friend rust::Vec<std::int32_t> eval_int_array(
        const StageHandle& stage, rust::Str prim_path, rust::Str attr_name,
        double time_code, bool use_default_time);
    friend rust::Vec<rust::String> eval_token_array(
        const StageHandle& stage, rust::Str prim_path, rust::Str attr_name,
        double time_code, bool use_default_time);
    friend bool prim_has_api(const StageHandle& stage, rust::Str prim_path, rust::Str schema_name);
    friend bool prim_is_a(const StageHandle& stage, rust::Str prim_path, rust::Str type_name);
    friend rust::Vec<rust::String> animated_prim_paths(const StageHandle& stage);
    friend rust::Vec<rust::String> animated_attr_names(const StageHandle& stage, rust::Str prim_path);
    friend AttrTimeSamplesRaw attr_time_samples(
        const StageHandle& stage, rust::Str prim_path, rust::Str attr_name);
};

void register_plugins(rust::Str install_dir);
// Number of live stage handles process-wide (see the definition in usd_shim.cpp
// for the increment/decrement discipline). For teardown leak-regression tests.
std::uint64_t live_stage_handle_count();
PackageResultRaw compute_dependencies(rust::Str source);
PackageResultRaw package_stage(
    rust::Str source, rust::Str destination, const PackagingOptionsRaw& options);
PackageResultRaw package_stage_dropping_unresolved(
    rust::Str source, rust::Str destination, const PackagingOptionsRaw& options);
PackageResultRaw package_stage_localize_keeping_remote(
    rust::Str source, rust::Str destination, const PackagingOptionsRaw& options);
std::unique_ptr<StageHandle> open_stage(rust::Str path, bool load_payloads);
std::unique_ptr<StageHandle> open_stage_with_session(
    rust::Str path, rust::Str session_layer_text, bool load_payloads);
std::unique_ptr<StageHandle> create_new_stage(
    rust::Str path, rust::Str up_axis, double meters_per_unit,
    rust::Str default_prim_name, rust::Str default_prim_type);
StageMetadataRaw stage_metadata(const StageHandle& stage);
void validate_runtime_stage(const StageHandle& stage);
VariantOpinionRaw session_variant_opinion(const StageHandle& stage, rust::Str path, rust::Str set);
void restore_session_variant_opinion(StageHandle& stage, rust::Str path, rust::Str set, bool authored, rust::Str value);
StageKindRaw read_stage_kind(rust::Str path);
StageRootPrimsRaw read_stage_root_prims(rust::Str path);
void set_default_asset_search_path(rust::Str search_path);
rust::Vec<TopologyRecordRaw> project_topology(
    const StageHandle& stage, rust::Slice<const rust::String> roots);
ViewportProjectionRaw project_viewport(
    const StageHandle& stage, double time_code, bool use_default_time,
    rust::Slice<const rust::String> roots);
ViewportProjectionRaw project_skeleton_poses(
    const StageHandle& stage, double time_code, bool use_default_time,
    rust::Slice<const rust::String> roots);
rust::Vec<TransformRecordRaw> project_transforms_at(
    const StageHandle& stage, double time_code, bool use_default_time,
    rust::Slice<const rust::String> prim_paths);
rust::Vec<PropertyRecordRaw> inspect_prim(const StageHandle& stage, rust::Str path);
rust::String resolve_prim_asset(
    const StageHandle& stage, rust::Str prim_path, rust::Str attr_name);
MaterialGraphRaw material_graph(const StageHandle& stage, rust::Str path);
ComputeGraphRaw compute_graph(const StageHandle& stage, rust::Str path);
rust::Vec<VariantSetRaw> variant_sets(const StageHandle& stage, rust::Str path);
rust::Vec<float> get_local_transform(const StageHandle& stage, rust::Str path);
rust::Vec<float> get_world_transform(const StageHandle& stage, rust::Str path);
rust::Vec<double> get_transform_matrix(const StageHandle& stage, rust::Str path, bool world);
void set_session_variant(
    StageHandle& stage,
    rust::Str path,
    rust::Str set_name,
    rust::Str selection);
#ifdef IDTX_AUTHORING
void inject_transform_edit_failure_after_writes(
    StageHandle& stage, std::uint32_t successful_writes);
AuthoredXformStackRaw authored_xform_stack(
    const StageHandle& stage, rust::Str path);
void apply_transform_edit(
    StageHandle& stage, rust::Str path, rust::Slice<const double> translate,
    rust::Slice<const double> rotate_xyz, rust::Slice<const double> scale);
void apply_matrix_transform(StageHandle& stage, rust::Str path, rust::Slice<const double> matrix);
void author_session_transform(
    StageHandle& stage, rust::Str path, rust::Slice<const double> matrix);
void author_session_transforms(
    StageHandle& stage, rust::Slice<const rust::String> paths,
    rust::Slice<const double> matrices);
void author_session_visibility(
    StageHandle& stage, rust::Str path, bool visible);
// Define a new typed prim into the session layer (empty type_name = typeless).
void define_session_prim(
    StageHandle& stage, rust::Str path, rust::Str type_name);
// Author a scalar/asset/token/string/int2 property onto an existing prim in the
// session layer. Value is string-encoded like the structural set-property path.
void author_session_property(
    StageHandle& stage, rust::Str path, rust::Str attr, rust::Str kind,
    rust::Str value);
void author_root_shadow_mode(StageHandle& stage, std::int32_t mode);
void author_root_stage_behavior(
    StageHandle& stage, rust::Str name, rust::Str run_scope);
void clear_root_stage_behavior(StageHandle& stage);
VariantLayerStateRaw variant_layer_state(
    const StageHandle& stage, rust::Str path, rust::Str set_name);
void set_variant_layer_state(
    StageHandle& stage, rust::Str path, rust::Str set_name,
    rust::Str root_selection, rust::Str session_selection);
void manifest_variant(
    StageHandle& stage, rust::Str path, rust::Str set_name,
    rust::Str selection);
void manifest_variants(
    StageHandle& stage, rust::Slice<const VariantSelectionEditRaw> edits);
void set_variant_layer_states(
    StageHandle& stage, rust::Slice<const VariantLayerEditRaw> edits);
rust::String root_layer_text(const StageHandle& stage);
rust::String root_prim_text(const StageHandle& stage, rust::Str path);
rust::String root_transform_text(const StageHandle& stage, rust::Str path);
void mutate_session_prim(StageHandle& stage, rust::Str path, rust::Str type_name, bool remove);
void erase_session_prim(StageHandle& stage, rust::Str path);
#ifdef IDTX_AUTHORING
void test_stage_access_stress(StageHandle& stage, StageHandle& peer, std::uint32_t iterations);
#endif
void replace_root_prim_text(StageHandle& stage, rust::Str path, rust::Str source);
void reconcile_root_prim(StageHandle& stage, rust::Str path, rust::Str type_name, rust::Str asset, bool payload);
void set_root_stage_kind(
    StageHandle& stage, rust::Str stage_kind, rust::Str display_name);
void replace_root_layer_text(StageHandle& stage, rust::Str source);
void apply_root_layer_delta(StageHandle& stage, rust::Str expected, rust::Str desired);
void discard_pending_changes(StageHandle& stage);
rust::String root_property_text(
    const StageHandle& stage, rust::Str path, rust::Str property);
void replace_root_property_text(
    StageHandle& stage, rust::Str path, rust::Str property, rust::Str source);
void apply_structural_edit(
    StageHandle& stage, rust::Str kind, rust::Str path,
    rust::Str destination, rust::Str type_name, rust::Str asset_path);
void apply_layer_edit(
    StageHandle& stage, rust::Str kind, rust::Str identifier,
    rust::Str parent_identifier, std::int32_t direction);
PersistenceResultRaw save_stage(const StageHandle& stage, rust::Str destination);
PersistenceResultRaw export_root_layer(
    const StageHandle& stage, rust::Str destination);
PersistenceResultRaw export_flattened_stage(
    const StageHandle& stage, rust::Str destination, bool add_source_file_comment);
void verify_persistence(
    const StageHandle& stage, const StageHandle& persisted, rust::Str mode);
#endif
std::int32_t stage_behavior_compose_context(const StageHandle& stage);
rust::String stage_behavior_run_scope_resolved(const StageHandle& stage);
StageBehaviorResolvedRaw stage_behavior_resolved_for_prim(
    const StageHandle& stage, rust::Str prim_path);
void set_payload_loaded(StageHandle& stage, rust::Str path, bool loaded);
bool get_prim_authored_visibility(const StageHandle& stage, rust::Str path);
rust::Vec<std::uint8_t> read_asset(const StageHandle& stage, rust::Str resolved_path);
rust::Vec<std::uint8_t> read_asset_global(rust::Str resolved_path);
rust::Vec<std::uint8_t> read_asset_global_bounded(rust::Str resolved_path, std::uint64_t max_bytes);
rust::Vec<ChangeRecordRaw> drain_changes(StageHandle& stage);
void conformance_author_double(StageHandle& stage, rust::Str property_path, double value);
void conformance_author_double_at_time(
    StageHandle& stage, rust::Str property_path, double value,
    double time_code);
void conformance_author_vec3f_array_at_time(
    StageHandle& stage, rust::Str property_path,
    rust::Slice<const float> values, double time_code);
void conformance_set_relationship_target(
    StageHandle& stage, rust::Str relationship_path, rust::Str target_path);
void conformance_set_custom_data(
    StageHandle& stage, rust::Str prim_path, rust::Str key, rust::Str value);
void conformance_define_xform(StageHandle& stage, rust::Str path);

// Uber-implementation additions
void register_plugin_path(rust::Str path);
void reload_layer(StageHandle& stage, rust::Str layer_path);
SkelSummariesRaw skel_summaries(const StageHandle& stage);
rust::Vec<LayerRecordRaw> layer_stack(const StageHandle& stage);
bool root_layer_is_dirty(const StageHandle& stage);
std::uint64_t root_layer_content_hash(const StageHandle& stage);
rust::String root_non_transform_text(const StageHandle& stage);
std::uint64_t observer_checkpoint(const StageHandle& stage);
AuthoredDataRaw authored_custom_data_json(const StageHandle& stage, rust::Str path, rust::Str key, std::uint32_t max_bytes);
AuthoredDataRaw authored_string(const StageHandle& stage, rust::Str path, rust::Str property, std::uint32_t max_bytes);
#ifdef IDTX_AUTHORING
void spawn_session_reference(StageHandle& stage, rust::Str path, rust::Str asset, rust::Str target, rust::Slice<const double> matrix, rust::Str context, rust::Slice<const rust::String> children, rust::Slice<const rust::String> properties, rust::Slice<const rust::String> sources);
#endif
void observer_rollback_to(StageHandle& stage, std::uint64_t checkpoint);
#ifdef IDTX_OPENEXEC
std::uint64_t exec_build_program(StageHandle& stage);
rust::Vec<ExecDataSourceRaw> exec_data_sources(const StageHandle& stage);
rust::Vec<ExecDataSourceInputRaw> exec_data_source_inputs(const StageHandle& stage);
bool exec_enqueue(const StageHandle& stage, rust::Str prim_path, rust::Str body);
bool exec_has_live_providers(const StageHandle& stage);
std::uint64_t tick_exec(StageHandle& stage);
void exec_release(StageHandle& stage);
#endif
WgslMaterialRaw material_surface_wgsl(const StageHandle& stage, rust::Str path);
AttributeValueRaw eval_attribute(
    const StageHandle& stage, rust::Str prim_path, rust::Str attr_name,
    double time_code, bool use_default_time);
rust::Vec<float> eval_point3f_array(
    const StageHandle& stage, rust::Str prim_path, rust::Str attr_name,
    double time_code, bool use_default_time);
rust::Vec<std::int32_t> eval_int_array(
    const StageHandle& stage, rust::Str prim_path, rust::Str attr_name,
    double time_code, bool use_default_time);
rust::Vec<rust::String> eval_token_array(
    const StageHandle& stage, rust::Str prim_path, rust::Str attr_name,
    double time_code, bool use_default_time);
bool prim_has_api(const StageHandle& stage, rust::Str prim_path, rust::Str schema_name);
bool prim_is_a(const StageHandle& stage, rust::Str prim_path, rust::Str type_name);
rust::Vec<rust::String> animated_prim_paths(const StageHandle& stage);
rust::Vec<rust::String> animated_attr_names(const StageHandle& stage, rust::Str prim_path);
AttrTimeSamplesRaw attr_time_samples(
    const StageHandle& stage, rust::Str prim_path, rust::Str attr_name);

}  // namespace idtx
