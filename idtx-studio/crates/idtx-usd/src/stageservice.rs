//! Safe Rust facade over the `idtx_usd_sys` C++/OpenUSD FFI bridge.
//!
//! [`StageService`] owns an open `UsdStage` handle and exposes it to the rest
//! of IDTX Studio. It opens or creates a USD stage, projects topology and
//! viewport geometry (meshes, gprims, cameras, lights, materials, skeletons,
//! and skin bindings), authors transforms and structural/layer/variant edits,
//! drives the optional `OpenExec` dataflow, and persists/exports/packages the
//! stage. It maintains stable, generation-scoped [`PrimId`] identities — minted
//! during topology projection and retained as bounded tombstones across change
//! drains — so callers can hold prim references across edits.
//!
//! The write-path (structural authoring) methods live in the `authoring`
//! submodule; everything else is implemented directly on [`StageService`] here.

use idtx_usd_sys::bridge::ffi;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::Ordering;
use std::time::Instant;
use std::path::Path;

use crate::*;

mod authoring;
mod projection;
mod structural;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RetiredIdentity {
    prim: PrimId,
    path: PathId,
    retired_at: StageRevision,
}

/// Owns one open `UsdStage` (via the FFI `StageHandle`) together with the
/// Rust-side bookkeeping that maps interned paths to stable, generation-scoped
/// [`PrimId`]s. Holds the retained topology snapshot, the path/prim lookup
/// tables, a bounded tombstone archive of retired identities, cached stage
/// metadata, cumulative metrics, and a persistent `MaterialX`->WGSL transpile
/// cache. All stage access happens on the owning thread.
pub struct StageService {
    pub(crate) handle: cxx::UniquePtr<ffi::StageHandle>,
    pub(crate) generation: StageGeneration,
    pub(crate) revision: StageRevision,
    pub(crate) metadata: StageMetadata,
    pub(crate) paths: PathTable,
    pub(crate) next_prim_slot: u32,
    pub(crate) known_prim_by_path: HashMap<PathId, PrimId>,
    pub(crate) prim_by_path: HashMap<PathId, PrimId>,
    pub(crate) path_by_prim: HashMap<PrimId, PathId>,
    pub(crate) retired_identities: VecDeque<RetiredIdentity>,
    pub(crate) identity_retention: IdentityRetentionPolicy,
    pub(crate) topology: Vec<TopologyRecord>,
    pub(crate) metrics: StageMetrics,
    /// Persistent cache of `MaterialX` WGSL transpile results, keyed by material prim path.
    /// `Some(wgsl)` = successful transpile; `None` = known failure (use PBR fallback).
    /// Populated on first projection; subsequent projections skip the FFI + transpile for
    /// already-seen paths, eliminating redundant work and log spam.
    pub(crate) materialx_wgsl_cache: HashMap<String, Option<String>>,
}

impl std::fmt::Debug for StageService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StageService")
            .field("generation", &self.generation)
            .field("revision", &self.revision)
            .field("metadata", &self.metadata)
            .field("topology_records", &self.topology.len())
            .field("metrics", &self.metrics)
            .finish_non_exhaustive()
    }
}

impl StageService {
    /// Opens an existing stage at `path`, loading all payloads. Registers USD
    /// plugins on first use and performs the initial topology projection before
    /// returning.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StageError> {
        Self::open_with_payloads(path, true)
    }

    /// Opens composition and topology without loading payload contents.
    pub fn open_topology_only(path: impl AsRef<Path>) -> Result<Self, StageError> {
        Self::open_with_payloads(path, false)
    }

    fn open_with_payloads(path: impl AsRef<Path>, load_payloads: bool) -> Result<Self, StageError> {
        let path = path.as_ref();
        let text = path
            .to_str()
            .ok_or_else(|| StageError::NonUtf8Path(path.to_path_buf()))?;
        register_plugins_once()?;
        let started = Instant::now();
        let handle = ffi::open_stage(text, load_payloads)?;
        Self::finish_open(handle, started)
    }

    /// Opens `path` as the root layer with `session_layer_text` (USDA) composed
    /// in as the stage's strongest **session** layer, via the 3-arg
    /// `UsdStage::Open(root, session, loadset)`. The session layer is anonymous
    /// and in-memory: content authored there composes over the root without
    /// touching it on disk.
    ///
    /// Asset paths inside `session_layer_text` must be **absolute** — the
    /// anonymous session layer has no directory to anchor relative references.
    /// Pass an empty string to open with an empty session layer.
    pub fn open_with_session(
        path: impl AsRef<Path>,
        session_layer_text: &str,
        load_payloads: bool,
    ) -> Result<Self, StageError> {
        let path = path.as_ref();
        let text = path
            .to_str()
            .ok_or_else(|| StageError::NonUtf8Path(path.to_path_buf()))?;
        register_plugins_once()?;
        let started = Instant::now();
        let handle = ffi::open_stage_with_session(text, session_layer_text, load_payloads)?;
        Self::finish_open(handle, started)
    }

    /// Creates a brand-new USD stage on disk at `path` with a default `/World`
    /// [`Xform`](https://openusd.org/release/api/class_usd_geom_xform.html) root
    /// and the given up-axis + metres-per-unit, then opens it — the U1.1
    /// "File → New" seam.
    ///
    /// `up_axis` is `"Y"` (default) or `"Z"`; `meters_per_unit` is applied only
    /// when positive. The stage is saved to disk before it is opened, so `path`
    /// exists on return. See [`create_new_with_root`](Self::create_new_with_root)
    /// to control the default prim name/type.
    pub fn create_new(
        path: impl AsRef<Path>,
        up_axis: &str,
        meters_per_unit: f64,
    ) -> Result<Self, StageError> {
        Self::create_new_with_root(path, up_axis, meters_per_unit, "World", "Xform")
    }

    /// Like [`create_new`](Self::create_new) but with an explicit default-root
    /// prim name and type. An empty `default_prim_name` authors no root prim (and
    /// sets no `defaultPrim`); an empty `default_prim_type` makes the root prim
    /// typeless.
    pub fn create_new_with_root(
        path: impl AsRef<Path>,
        up_axis: &str,
        meters_per_unit: f64,
        default_prim_name: &str,
        default_prim_type: &str,
    ) -> Result<Self, StageError> {
        let path = path.as_ref();
        let text = path
            .to_str()
            .ok_or_else(|| StageError::NonUtf8Path(path.to_path_buf()))?;
        register_plugins_once()?;
        let started = Instant::now();
        let handle = ffi::create_new_stage(
            text,
            up_axis,
            meters_per_unit,
            default_prim_name,
            default_prim_type,
        )?;
        Self::finish_open(handle, started)
    }

    /// Shared construction from an already-opened bridge handle: reads metadata,
    /// assigns a fresh generation, and performs the initial topology projection.
    fn finish_open(
        handle: cxx::UniquePtr<ffi::StageHandle>,
        started: Instant,
    ) -> Result<Self, StageError> {
        let raw_metadata = ffi::stage_metadata(handle.as_ref().ok_or(StageError::NullHandle)?)?;
        let generation = StageGeneration(NEXT_GENERATION.fetch_add(1, Ordering::Relaxed));
        let mut service = Self {
            handle,
            generation,
            revision: StageRevision(1),
            metadata: StageMetadata {
                root_identifier: raw_metadata.root_identifier,
                default_prim: (!raw_metadata.default_prim.is_empty())
                    .then_some(raw_metadata.default_prim),
                up_axis: raw_metadata.up_axis,
                meters_per_unit: raw_metadata.meters_per_unit,
                start_time_code: raw_metadata.start_time_code,
                end_time_code: raw_metadata.end_time_code,
                time_codes_per_second: raw_metadata.time_codes_per_second,
                has_time_code_range: raw_metadata.has_time_code_range,
                shadow_mode: ShadowMode::from_i32(raw_metadata.shadow_mode),
                stage_behavior_name: (!raw_metadata.stage_behavior_name.is_empty())
                    .then_some(raw_metadata.stage_behavior_name),
                stage_behavior_run_scope: (!raw_metadata.stage_behavior_run_scope.is_empty())
                    .then(|| {
                        StageBehaviorRunScope::from_token(&raw_metadata.stage_behavior_run_scope)
                    }),
            },
            paths: PathTable::default(),
            next_prim_slot: 0,
            known_prim_by_path: HashMap::new(),
            prim_by_path: HashMap::new(),
            path_by_prim: HashMap::new(),
            retired_identities: VecDeque::new(),
            identity_retention: IdentityRetentionPolicy::default(),
            topology: Vec::new(),
            metrics: StageMetrics::default(),
            materialx_wgsl_cache: HashMap::new(),
        };
        service.metrics.open.calls = 1;
        service.metrics.open.records = 1;
        service.metrics.open.bytes = logical_metadata_bytes(&service.metadata);
        service.metrics.open.elapsed = started.elapsed();
        service.project_topology()?;
        Ok(service)
    }

    /// The stage's identity generation, assigned at open. All [`PrimId`]s minted
    /// by this service carry this generation; it changes only when a new stage is
    /// opened, invalidating prior ids.
    #[must_use]
    pub const fn generation(&self) -> StageGeneration {
        self.generation
    }

    /// The current stage revision, incremented each time [`Self::drain_changes`]
    /// reports a non-empty change batch.
    #[must_use]
    pub const fn revision(&self) -> StageRevision {
        self.revision
    }

    /// Stage-level metadata (root identifier, default prim, up-axis,
    /// metres-per-unit, time-code range, shadow mode, stage-behavior binding) as
    /// captured at open time. Not refreshed by later root-layer edits.
    #[must_use]
    pub const fn metadata(&self) -> &StageMetadata {
        &self.metadata
    }

    /// Stage-level animation range and playback rate.
    #[must_use]
    pub fn animation(&self) -> StageAnimation {
        StageAnimation {
            has_time_code_range: self.metadata.has_time_code_range,
            start_time_code: self.metadata.start_time_code,
            end_time_code: self.metadata.end_time_code,
            time_codes_per_second: self.metadata.time_codes_per_second,
        }
    }

    /// Returns the [`PrimId`]s of all prims that have at least one time-sampled
    /// attribute. Performs a single stage traversal; call once at hydration time.
    pub fn animated_prim_ids(&self) -> Result<Vec<PrimId>, StageError> {
        let handle = self.handle.as_ref().ok_or(StageError::NullHandle)?;
        let paths = ffi::animated_prim_paths(handle)?;
        let ids: Vec<PrimId> = paths
            .iter()
            .filter_map(|p| self.prim_at_path(p.as_str()))
            .collect();
        Ok(ids)
    }

    /// Returns the attribute names on `prim` that have authored time samples.
    pub fn animated_attr_names(&self, prim: PrimId) -> Result<Vec<String>, StageError> {
        let handle = self.handle.as_ref().ok_or(StageError::NullHandle)?;
        let path = self.path_for(prim).ok_or(StageError::NullHandle)?;
        let names = ffi::animated_attr_names(handle, path)?;
        Ok(names.into_iter().collect())
    }

    /// Returns time-sample metadata for `attr_name` on `prim`.
    /// Returns `None` when the prim or attribute is not found.
    pub fn attr_time_samples(
        &self,
        prim: PrimId,
        attr_name: &str,
    ) -> Result<Option<TimeSamples>, StageError> {
        let handle = self.handle.as_ref().ok_or(StageError::NullHandle)?;
        let path = self.path_for(prim).ok_or(StageError::NullHandle)?;
        let raw = ffi::attr_time_samples(handle, path, attr_name)?;
        if raw.times.is_empty() {
            return Ok(None);
        }
        let interpolation = match raw.interpolation.as_str() {
            "held" => Some(Interpolation::Held),
            "linear" => Some(Interpolation::Linear),
            _ => None,
        };
        let count = raw.times.len();
        Ok(Some(TimeSamples {
            times: raw.times.into_iter().collect(),
            count,
            interpolation,
        }))
    }

    /// The retained full topology snapshot (one [`TopologyRecord`] per composed
    /// prim), as last produced by [`Self::project_topology`] /
    /// [`Self::project_topology_paths`].
    #[must_use]
    pub fn topology(&self) -> &[TopologyRecord] {
        &self.topology
    }

    /// Resolves an interned [`PathId`] back to its path string, or `None` if the
    /// id is not interned in this service.
    #[must_use]
    pub fn path(&self, id: PathId) -> Option<&str> {
        self.paths.get(id)
    }

    /// Looks up the [`PrimId`] currently projected at `path`, or `None` if no
    /// prim is projected there. Reads the projected topology, not the live stage.
    #[must_use]
    pub fn prim_at_path(&self, path: &str) -> Option<PrimId> {
        self.prim_for_path(path).ok()
    }

    /// Returns the path string for `prim`, or `None` if `prim` is stale or no
    /// longer live.
    #[must_use]
    pub fn path_for(&self, prim: PrimId) -> Option<&str> {
        self.path_for_prim(prim).ok()
    }

    /// Returns `true` when `prim` still maps to a live projected path (i.e. it
    /// has not been retired into the tombstone archive).
    #[must_use]
    pub fn is_live_prim(&self, prim: PrimId) -> bool {
        self.path_by_prim
            .get(&prim)
            .is_some_and(|path| self.prim_by_path.get(path) == Some(&prim))
    }

    /// Cumulative per-operation metrics (call counts, record counts, logical
    /// bytes, elapsed time) accumulated across this service's lifetime.
    #[must_use]
    pub const fn metrics(&self) -> &StageMetrics {
        &self.metrics
    }

    /// Updates the bounded tombstone policy and immediately reclaims entries
    /// outside it. This never changes IDs for live prims.
    pub fn set_identity_retention_policy(&mut self, policy: IdentityRetentionPolicy) {
        self.identity_retention = policy;
        self.compact_identity_archive();
    }

    /// Snapshot of identity bookkeeping: live prim count, retained tombstones,
    /// interned path count and byte total, next path/prim slot, the active
    /// retention policy, and the oldest retained tombstone revision.
    #[must_use]
    pub fn identity_retention_stats(&self) -> IdentityRetentionStats {
        IdentityRetentionStats {
            generation: self.generation,
            live_prims: self.prim_by_path.len(),
            retained_tombstones: self.retired_identities.len(),
            interned_paths: self.paths.strings.len(),
            path_string_bytes: self.paths.string_bytes(),
            next_path_slot: u64::from(self.paths.next_slot),
            next_prim_slot: u64::from(self.next_prim_slot),
            policy: self.identity_retention,
            oldest_tombstone_revision: self
                .retired_identities
                .front()
                .map(|entry| entry.retired_at),
        }
    }

    /// Validates that the open stage is suitable for runtime use, delegating to
    /// the bridge's runtime-stage check. Errors with [`StageError`] if validation
    /// fails.
    pub fn validate_runtime_readiness(&self) -> Result<(), StageError> {
        ffi::validate_runtime_stage(self.handle_ref()?)?;
        Ok(())
    }


    /// Returns every authored and resolved property (attributes, relationships,
    /// metadata) on `prim`, with type, value summary, authoring source, time
    /// sample count, provenance, and relationship targets. Errors if `prim` is
    /// stale.
    pub fn inspect_prim(&mut self, prim: PrimId) -> Result<Vec<PropertyRecord>, StageError> {
        self.validate(prim)?;
        let path = self.path_for_prim(prim)?.to_owned();
        let started = Instant::now();
        let raw = ffi::inspect_prim(self.handle_ref()?, &path)?;
        let records = raw
            .into_iter()
            .map(|record| PropertyRecord {
                name: record.name,
                kind: match record.kind.as_str() {
                    "relationship" => PropertyKind::Relationship,
                    "metadata" => PropertyKind::Metadata,
                    _ => PropertyKind::Attribute,
                },
                type_name: record.type_name,
                value_summary: record.value_summary,
                authored: record.authored,
                value_source: record.value_source,
                time_sample_count: record.time_sample_count,
                provenance: record.provenance,
                provenance_values: record.provenance_values,
                relationship_targets: record.relationship_targets,
                root_authored: record.root_authored,
            })
            .collect::<Vec<_>>();
        self.metrics.inspection.calls += 1;
        self.metrics.inspection.records += u64::try_from(records.len()).unwrap_or(u64::MAX);
        self.metrics.inspection.bytes += records
            .iter()
            .map(|record| {
                record.name.len()
                    + record.type_name.len()
                    + record.value_summary.len()
                    + record.value_source.len()
                    + record.provenance.iter().map(String::len).sum::<usize>()
                    + record
                        .provenance_values
                        .iter()
                        .map(String::len)
                        .sum::<usize>()
                    + record
                        .relationship_targets
                        .iter()
                        .map(String::len)
                        .sum::<usize>()
                    + 2
            })
            .map(|bytes| u64::try_from(bytes).unwrap_or(u64::MAX))
            .sum::<u64>();
        self.metrics.inspection.elapsed += started.elapsed();
        Ok(records)
    }

    /// Resolve an `asset`-typed attribute (`attr`) on `prim` to an absolute path
    /// via USD's own asset resolution.
    ///
    /// Unlike [`inspect_prim`](Self::inspect_prim) — which returns the *authored*
    /// (layer-relative) asset string — this anchors the value against its
    /// authoring layer, so a relative path resolves identically whether `prim`
    /// is opened directly or reached through a composed (payloaded/referenced)
    /// assembly. Returns `Ok(None)` when the attribute is absent, not an asset,
    /// or unset.
    pub fn resolve_prim_asset(
        &self,
        prim: PrimId,
        attr: &str,
    ) -> Result<Option<String>, StageError> {
        self.validate(prim)?;
        let path = self.path_for_prim(prim)?.to_owned();
        let resolved = ffi::resolve_prim_asset(self.handle_ref()?, &path, attr)?;
        Ok(if resolved.is_empty() {
            None
        } else {
            Some(resolved)
        })
    }

    /// Builds the shading-network graph bound to `prim`: the material path(s),
    /// its nodes with input names/value summaries/connections, the material's
    /// output connections, and any bridge diagnostics. Errors if `prim` is stale.
    pub fn material_graph(&mut self, prim: PrimId) -> Result<MaterialGraph, StageError> {
        self.validate(prim)?;
        let path = self.path_for_prim(prim)?.to_owned();
        let started = Instant::now();
        let raw = ffi::material_graph(self.handle_ref()?, &path)?;
        let nodes = raw
            .nodes
            .into_iter()
            .map(|node| MaterialNode {
                path: node.path,
                identifier: node.identifier,
                inputs: node
                    .input_names
                    .into_iter()
                    .zip(node.input_values)
                    .zip(node.connections)
                    .map(|((name, value_summary), connection)| MaterialInput {
                        name,
                        value_summary,
                        connections: connection
                            .lines()
                            .filter(|source| !source.is_empty())
                            .map(str::to_owned)
                            .collect(),
                    })
                    .collect(),
            })
            .collect::<Vec<_>>();
        self.metrics.inspection.calls += 1;
        self.metrics.inspection.records += u64::try_from(nodes.len()).unwrap_or(u64::MAX);
        self.metrics.inspection.bytes += nodes
            .iter()
            .map(|node| {
                node.path.len()
                    + node.identifier.len()
                    + node
                        .inputs
                        .iter()
                        .map(|input| {
                            input.name.len()
                                + input.value_summary.len()
                                + input.connections.iter().map(String::len).sum::<usize>()
                        })
                        .sum::<usize>()
            })
            .map(|bytes| u64::try_from(bytes).unwrap_or(u64::MAX))
            .sum::<u64>();
        self.metrics.inspection.elapsed += started.elapsed();
        Ok(MaterialGraph {
            material_path: (!raw.material_path.is_empty()).then_some(raw.material_path),
            material_paths: raw.material_paths,
            nodes,
            diagnostics: raw.diagnostics,
            material_output_connections: raw
                .material_output_attr_names
                .into_iter()
                .zip(raw.material_output_targets)
                .collect(),
        })
    }

    /// Build the `OpenExec` compute graph rooted at `prim` by walking authored
    /// attribute connections backward (see the C++ `compute_graph` shim).
    /// Returns a graph with `root_path == None` when `prim` has no connected
    /// attribute.
    pub fn compute_graph(&mut self, prim: PrimId) -> Result<ComputeGraph, StageError> {
        self.validate(prim)?;
        let path = self.path_for_prim(prim)?.to_owned();
        let started = Instant::now();
        let raw = ffi::compute_graph(self.handle_ref()?, &path)?;
        let split_targets = |joined: String| -> Vec<String> {
            joined
                .lines()
                .filter(|target| !target.is_empty())
                .map(str::to_owned)
                .collect()
        };
        let nodes = raw
            .nodes
            .into_iter()
            .map(|node| {
                let inputs = node
                    .input_names
                    .into_iter()
                    .zip(node.input_types)
                    .zip(node.input_values)
                    .zip(node.input_connections)
                    .map(
                        |(((name, type_name), value_summary), connections)| ComputeSocket {
                            name,
                            type_name,
                            value_summary,
                            connections: split_targets(connections),
                        },
                    )
                    .collect();
                let outputs = node
                    .output_names
                    .into_iter()
                    .zip(node.output_types)
                    .map(|(name, type_name)| ComputeSocket {
                        name,
                        type_name,
                        value_summary: String::new(),
                        connections: Vec::new(),
                    })
                    .collect();
                let role = match node.role.as_str() {
                    "datasource" => ComputeNodeRole::Datasource,
                    "compute" => ComputeNodeRole::Compute,
                    "consumer" => ComputeNodeRole::Consumer,
                    _ => ComputeNodeRole::Other,
                };
                ComputeNode {
                    path: node.path,
                    type_name: node.type_name,
                    role,
                    inputs,
                    outputs,
                }
            })
            .collect::<Vec<_>>();
        self.metrics.inspection.calls += 1;
        self.metrics.inspection.records += u64::try_from(nodes.len()).unwrap_or(u64::MAX);
        self.metrics.inspection.elapsed += started.elapsed();
        Ok(ComputeGraph {
            root_path: (!raw.root_path.is_empty()).then_some(raw.root_path),
            nodes,
            diagnostics: raw.diagnostics,
        })
    }

    /// Returns the variant sets authored on `prim`: each set's name, choices,
    /// current selection, and provenance. Errors if `prim` is stale.
    pub fn variant_sets(&mut self, prim: PrimId) -> Result<Vec<VariantSetRecord>, StageError> {
        self.validate(prim)?;
        let path = self.path_for_prim(prim)?.to_owned();
        let started = Instant::now();
        let records = ffi::variant_sets(self.handle_ref()?, &path)?
            .into_iter()
            .map(|record| VariantSetRecord {
                name: record.name,
                choices: record.choices,
                selection: record.selection,
                provenance: record.provenance,
            })
            .collect::<Vec<_>>();
        self.metrics.inspection.calls += 1;
        self.metrics.inspection.records += u64::try_from(records.len()).unwrap_or(u64::MAX);
        self.metrics.inspection.bytes += records
            .iter()
            .map(|record| {
                record.name.len()
                    + record.selection.len()
                    + record.choices.iter().map(String::len).sum::<usize>()
                    + record.provenance.iter().map(String::len).sum::<usize>()
            })
            .map(|bytes| u64::try_from(bytes).unwrap_or(u64::MAX))
            .sum::<u64>();
        self.metrics.inspection.elapsed += started.elapsed();
        Ok(records)
    }

    /// Returns the local-to-parent transform matrix for `prim` at the default time code.
    ///
    /// Uses `UsdGeomXformable::GetLocalTransformation()`. Returns the 4×4 matrix in
    /// row-major order (row-vector convention: `p' = p · M`, translation in last row).
    /// Returns the identity matrix when the prim is not xformable.
    pub fn get_local_transform(&mut self, prim: PrimId) -> Result<[[f32; 4]; 4], StageError> {
        self.validate(prim)?;
        let path = self.path_for_prim(prim)?.to_owned();
        let floats = ffi::get_local_transform(self.handle_ref()?, &path)?;
        if floats.len() < 16 {
            return Ok([
                [1., 0., 0., 0.],
                [0., 1., 0., 0.],
                [0., 0., 1., 0.],
                [0., 0., 0., 1.],
            ]);
        }
        Ok([
            [floats[0], floats[1], floats[2], floats[3]],
            [floats[4], floats[5], floats[6], floats[7]],
            [floats[8], floats[9], floats[10], floats[11]],
            [floats[12], floats[13], floats[14], floats[15]],
        ])
    }

    /// Double-precision local transform, flat USD row-major layout.
    pub fn get_local_transform_f64(&mut self, prim: PrimId) -> Result<[f64; 16], StageError> {
        self.transform_matrix_f64(prim, false)
    }

    /// Double-precision composed world transform, flat USD row-major layout.
    pub fn get_world_transform_f64(&mut self, prim: PrimId) -> Result<[f64; 16], StageError> {
        self.transform_matrix_f64(prim, true)
    }

    fn transform_matrix_f64(&mut self, prim: PrimId, world: bool) -> Result<[f64; 16], StageError> {
        self.validate(prim)?;
        let path = self.path_for_prim(prim)?.to_owned();
        let values = ffi::get_transform_matrix(self.handle_ref()?, &path, world)?;
        let matrix: [f64; 16] = values.try_into().map_err(|_| StageError::InvalidTransform("expected 16 matrix components".into()))?;
        if !matrix.iter().all(|v| v.is_finite()) {
            return Err(StageError::InvalidTransform("composed matrix is not finite".into()));
        }
        Ok(matrix)
    }

    /// Returns the world-space transform via `UsdGeomXformCache::GetLocalToWorldTransform()`.
    /// Row-major order (row-vector convention), identity when the prim is not xformable.
    pub fn get_world_transform(&mut self, prim: PrimId) -> Result<[[f32; 4]; 4], StageError> {
        self.validate(prim)?;
        let path = self.path_for_prim(prim)?.to_owned();
        let floats = ffi::get_world_transform(self.handle_ref()?, &path)?;
        if floats.len() < 16 {
            return Ok([
                [1., 0., 0., 0.],
                [0., 1., 0., 0.],
                [0., 0., 1., 0.],
                [0., 0., 0., 1.],
            ]);
        }
        Ok([
            [floats[0], floats[1], floats[2], floats[3]],
            [floats[4], floats[5], floats[6], floats[7]],
            [floats[8], floats[9], floats[10], floats[11]],
            [floats[12], floats[13], floats[14], floats[15]],
        ])
    }

    /// Authors a variant `selection` for `set_name` on `prim` into the stage's
    /// session layer, leaving the root layer untouched. Errors if `prim` is stale.
    pub fn set_session_variant(
        &mut self,
        prim: PrimId,
        set_name: &str,
        selection: &str,
    ) -> Result<(), StageError> {
        self.validate(prim)?;
        let path = self.path_for_prim(prim)?.to_owned();
        ffi::set_session_variant(self.handle.pin_mut(), &path, set_name, selection)?;
        Ok(())
    }

    
    /// Returns the authored xformOp stack on `prim`: each op's name, type,
    /// precision, inverse flag, time-sample/default presence and value, plus
    /// whether the stack resets inherited transforms and whether it is editable.
    /// Errors if `prim` is stale or an op has unsupported precision.
    pub fn authored_xform_stack(&self, prim: PrimId) -> Result<AuthoredXformStack, StageError> {
        self.validate(prim)?;
        let path = self.path_for_prim(prim)?;
        let raw = ffi::authored_xform_stack(self.handle_ref()?, path)?;
        let ops = raw
            .ops
            .into_iter()
            .map(|op| {
                let op_type = match op.op_type.as_str() {
                    "translate" => XformOpType::Translate,
                    "rotateXYZ" => XformOpType::RotateXyz,
                    "scale" => XformOpType::Scale,
                    _ => XformOpType::Unsupported,
                };
                let precision = match op.precision.as_str() {
                    "double" => XformPrecision::Double,
                    "float" => XformPrecision::Float,
                    "half" => XformPrecision::Half,
                    other => {
                        return Err(StageError::InvalidProjection {
                            path: raw.path.clone(),
                            detail: format!("unsupported xform precision {other}"),
                        });
                    }
                };
                let value = if op_type == XformOpType::Unsupported && op.value.is_empty() {
                    [0.0; 3]
                } else {
                    op.value.try_into().map_err(|value: Vec<f64>| {
                        StageError::InvalidProjection {
                            path: raw.path.clone(),
                            detail: format!(
                                "xform op {} returned {} values instead of three",
                                op.name,
                                value.len()
                            ),
                        }
                    })?
                };
                Ok(AuthoredXformOp {
                    name: op.name,
                    op_type,
                    op_type_name: op.op_type,
                    precision,
                    inverse: op.inverse,
                    time_sample_count: op.time_sample_count,
                    has_default: op.has_default,
                    value,
                })
            })
            .collect::<Result<Vec<_>, StageError>>()?;
        Ok(AuthoredXformStack {
            prim,
            reset_stack: raw.reset_stack,
            editable: raw.editable,
            diagnostic: (!raw.diagnostic.is_empty()).then_some(raw.diagnostic),
            ops,
        })
    }

    
    /// Authors translate/rotateXYZ(degrees)/scale values onto `prim` via the
    /// bridge transform-edit path. Errors if `prim` is stale or the native edit
    /// fails.
    pub fn apply_transform_edit(
        &mut self,
        prim: PrimId,
        values: TransformValues,
    ) -> Result<(), StageError> {
        self.validate(prim)?;
        let path = self.path_for_prim(prim)?.to_owned();
        ffi::apply_transform_edit(
            self.handle.pin_mut(),
            &path,
            &values.translation,
            &values.rotation_xyz_degrees,
            &values.scale,
        )?;
        Ok(())
    }

    /// Author an exact double-precision local affine matrix in the root layer.
    /// Replaces the effective op order, preserves reset-stack and dormant ops,
    /// and rolls back both property opinions and notices if authoring fails.
    
    pub fn apply_matrix_transform(&mut self, prim: PrimId, matrix: [f64; 16]) -> Result<(), StageError> {
        self.validate(prim)?;
        validate_usd_affine(&matrix)?;
        let path = self.path_for_prim(prim)?.to_owned();
        ffi::apply_matrix_transform(self.handle.pin_mut(), &path, &matrix)?;
        Ok(())
    }

    

    

    

    /// Defines a brand-new typed prim into the stage's **session** layer and
    /// returns its freshly-minted [`PrimId`]. An empty `type_name` defines a
    /// typeless prim; a non-empty one (e.g. `"Camera"`, `"DistantLight"`,
    /// `"Scope"`) defines that schema type.
    ///
    /// Runtime-only prims — the player camera, dynamic overlays, spawned actors
    /// — are authored here so they live purely in the session sublayer and
    /// never dirty the saved assembly (the change surfaces through
    /// [`drain_changes`](Self::drain_changes) tagged as *not* authored in the
    /// root layer). Errors with [`StageError`] if the path already composes
    /// (this defines a new prim, it does not override an existing one).
    ///
    /// Because a `PrimId` is a stage-generation-scoped identity minted only
    /// during topology projection, this authors the spec, retains the resulting
    /// notice, re-projects the new path's subtree, and then resolves the
    /// `PrimId` — so the returned id is immediately usable for selection,
    /// transform authoring, and behavior binding.
    
    pub fn define_session_prim(
        &mut self,
        path: &str,
        type_name: &str,
    ) -> Result<PrimId, StageError> {
        ffi::define_session_prim(self.handle.pin_mut(), path, type_name)?;
        // Mint the PrimId without consuming notices belonging to the stage
        // owner, including unrelated earlier edits in the same guest batch.
        self.project_topology_paths(&[path.to_owned()])?;
        self.prim_for_path(path)
    }

    /// Runtime structural authoring is confined to this stage's session layer.
    /// Removal deactivates weaker-layer prims and erases wholly transient prims.
    /// Notices remain queued for the stage owner's normal projection pass.
    
    pub fn mutate_session_prim(&mut self, path: &str, type_name: &str, remove: bool) -> Result<(), StageError> {
        ffi::mutate_session_prim(self.handle.pin_mut(), path, type_name, remove)?;
        Ok(())
    }

    /// Exercise same-handle projections/session writes and shared-root reloads
    /// inside C++, without weakening the Rust handle's non-Sync contract.
    #[cfg(feature = "test-support")]
    pub fn test_stage_access_stress(&mut self, peer: &mut Self, iterations: u32) -> Result<(), StageError> {
        ffi::test_stage_access_stress(self.handle.pin_mut(), peer.handle.pin_mut(), iterations)?;
        Ok(())
    }

    /// Classify how the layer carrying `idtx:stageBehavior` is composed into the
    /// open stage, for the runtime [`StageBehaviorRunScope`] gate. Returns
    /// [`StageComposeContext::Unknown`] when no stage behavior is assigned or the
    /// context is indeterminate.
    #[must_use]
    pub fn stage_behavior_compose_context(&self) -> StageComposeContext {
        match ffi::stage_behavior_compose_context(&self.handle) {
            Ok(raw) => StageComposeContext::from_i32(raw),
            Err(_) => StageComposeContext::Unknown,
        }
    }

    /// The stage behavior's run scope, read from the layer that actually *owns*
    /// it (the one carrying `idtx:stageBehavior`) — not the open stage's root
    /// layer. For a composed assembly this is the payloaded level, whose authored
    /// `idtx:stageBehaviorRunScope` the root layer does not carry. Returns `None`
    /// when no stage behavior is assigned / the run scope is unauthored, so the
    /// caller can fall back to the open stage's [`StageMetadata`] value (which is
    /// correct for a directly-opened stage).
    #[must_use]
    pub fn stage_behavior_run_scope_resolved(&self) -> Option<StageBehaviorRunScope> {
        match ffi::stage_behavior_run_scope_resolved(&self.handle) {
            Ok(token) if !token.is_empty() => Some(StageBehaviorRunScope::from_token(&token)),
            _ => None,
        }
    }

    /// Live re-read of the root-layer `idtx:stageBehaviorRunScope` token, straight
    /// from the current `customLayerData` rather than the cached [`metadata()`]
    /// (which is seeded at open and **not** refreshed by root-layer edits). Returns
    /// `None` when the key is unauthored (treat as
    /// [`StageBehaviorRunScope::Standalone`]).
    ///
    /// For a caller that just mutated the root layer (e.g. an undo/redo snapshot
    /// restore) and needs the fresh value, not the stale open-time one.
    #[must_use]
    pub fn reread_stage_behavior_run_scope(&self) -> Option<StageBehaviorRunScope> {
        let raw = self
            .handle
            .as_ref()
            .and_then(|h| ffi::stage_metadata(h).ok())?;
        (!raw.stage_behavior_run_scope.is_empty())
            .then(|| StageBehaviorRunScope::from_token(&raw.stage_behavior_run_scope))
    }

    /// Whether the stage behavior **at `prim_path` specifically** may attach/tick
    /// in this stage's composition: its owning-layer run scope
    /// [`permits`](StageBehaviorRunScope::permits) that layer's compose context.
    /// Resolved per prim so an assembly surfacing several levels' stage behaviors
    /// (each owned by a different layer with its own run scope) is gated per
    /// behavior, not by one global permit. Falls back to the cached root-layer
    /// [`StageMetadata`] run scope when the owning layer authored no scope token
    /// (correct for a directly-opened stage). Fail-closed on FFI error.
    #[must_use]
    pub fn stage_behavior_permitted_for_prim(&self, prim_path: &str) -> bool {
        match ffi::stage_behavior_resolved_for_prim(&self.handle, prim_path) {
            Ok(raw) => {
                let scope = if raw.run_scope.is_empty() {
                    self.metadata.stage_behavior_run_scope.unwrap_or_default()
                } else {
                    StageBehaviorRunScope::from_token(&raw.run_scope)
                };
                scope.permits(StageComposeContext::from_i32(raw.compose_context))
            }
            Err(_) => false,
        }
    }

    // ── OpenExec dataflow ────────────────────────────────────────────────────
    //
    // Safe wrappers over the idtx-usd-sys exec bridge. All stage mutation
    // (apply + compute + author) happens on the calling (main/session) thread
    // under the exclusive stage-lifecycle lock inside the FFI — never from a
    // background thread. See the exec bridge TU in idtx-usd-sys.

    /// (Re)build the per-stage OpenExec program: register every attribute with an
    /// authored connection as a value key, compile + activate the request.
    /// Returns the registered value-key count — `0` means the stage carries no
    /// IDTX compute graph, so the caller skips the whole subsystem (no REST
    /// poller, no per-frame tick). Call again after a topology resync.
    ///
    /// # Errors
    /// Returns [`StageError`] if the bridge build fails in the C++ layer.
    #[cfg(feature = "openexec")]
    pub fn exec_build(&mut self) -> Result<u64, StageError> {
        Ok(ffi::exec_build_program(self.handle.pin_mut())?)
    }

    /// REST datasource configurations authored in the stage, for the host poller.
    ///
    /// # Errors
    /// Returns [`StageError`] if the stage handle is null or the read fails.
    #[cfg(feature = "openexec")]
    pub fn exec_data_sources(&self) -> Result<Vec<ExecDataSource>, StageError> {
        Ok(ffi::exec_data_sources(self.handle_ref()?)?
            .into_iter()
            .map(ExecDataSource::from_raw)
            .collect())
    }

    /// Latest resolved datasource-INPUT values (a connected `authorization` /
    /// `jsonBody` driven by a `Compute_*` node), cached by the most recent
    /// [`StageService::exec_tick`]. The host reads these after each tick and feeds
    /// them into the live REST poller, so an environment-provider value (e.g. an
    /// auth token) reaches the request without being authored onto the stage.
    /// Empty when no datasource input carries a connection.
    ///
    /// # Errors
    /// Returns [`StageError`] if the stage handle is null or the read fails.
    #[cfg(feature = "openexec")]
    pub fn exec_data_source_inputs(&self) -> Result<Vec<ExecDataSourceInput>, StageError> {
        Ok(ffi::exec_data_source_inputs(self.handle_ref()?)?
            .into_iter()
            .map(|raw| ExecDataSourceInput {
                prim_path: raw.prim_path,
                attribute_name: raw.attribute_name,
                value: raw.value,
            })
            .collect())
    }

    /// Enqueue an external JSON `body` for `prim_path`'s `outputs:data` attribute
    /// (coalescing). No stage mutation here — the next [`StageService::exec_tick`]
    /// applies it. Returns `false` if the update was dropped (no active bridge,
    /// empty body, or the queue is full).
    ///
    /// # Errors
    /// Returns [`StageError`] if the stage handle is null.
    #[cfg(feature = "openexec")]
    pub fn exec_enqueue(&self, prim_path: &str, body: &str) -> Result<bool, StageError> {
        Ok(ffi::exec_enqueue(self.handle_ref()?, prim_path, body)?)
    }

    /// Whether the stage's compute graph carries a time/host-state provider
    /// (`Compute_Environment`): `true` => every exec tick must recompute; `false`
    /// => a purely datasource-driven graph whose computed values change only when
    /// a REST/mock payload arrives, so the host may skip [`StageService::exec_tick`]
    /// (and the exclusive stage lock) on frames with no pending payload. Read once
    /// when building the exec subsystem and cache it.
    ///
    /// # Errors
    /// Returns [`StageError`] if the stage handle is null.
    #[cfg(feature = "openexec")]
    pub fn exec_has_live_providers(&self) -> Result<bool, StageError> {
        Ok(ffi::exec_has_live_providers(self.handle_ref()?)?)
    }

    /// Apply pending external updates, run the OpenExec compute, and author every
    /// changed computed value into the **session layer**. Returns the number of
    /// attributes authored this tick (`0` => nothing changed, so the caller can
    /// skip reprojection). Holds the exclusive stage-lifecycle lock for the whole
    /// apply+compute+author region.
    ///
    /// # Errors
    /// Returns [`StageError`] if the stage has no session layer or a compute
    /// author fails (the provoked change-notices are rolled back on failure).
    #[cfg(feature = "openexec")]
    pub fn exec_tick(&mut self) -> Result<u64, StageError> {
        Ok(ffi::tick_exec(self.handle.pin_mut())?)
    }

    /// Drop the per-stage exec bridge (on stage teardown / topology resync).
    ///
    /// # Errors
    /// Returns [`StageError`] if the stage handle is null.
    #[cfg(feature = "openexec")]
    pub fn exec_release(&mut self) -> Result<(), StageError> {
        ffi::exec_release(self.handle.pin_mut())?;
        Ok(())
    }

    /// Arms a deterministic one-shot failure in the next native transform
    /// edit after the requested number of successful property writes.
    #[cfg(feature = "test-support")]
    #[doc(hidden)]
    pub fn inject_transform_edit_failure_after_writes(
        &mut self,
        successful_writes: u32,
    ) -> Result<(), StageError> {
        ffi::inject_transform_edit_failure_after_writes(self.handle.pin_mut(), successful_writes)?;
        Ok(())
    }

    
    /// Returns the variant selection for `set_name` on `prim` split by layer:
    /// the root-layer selection and the session-layer selection (each `None`
    /// when unauthored in that layer). Errors if `prim` is stale.
    pub fn variant_layer_state(
        &self,
        prim: PrimId,
        set_name: &str,
    ) -> Result<VariantLayerState, StageError> {
        self.validate(prim)?;
        let raw =
            ffi::variant_layer_state(self.handle_ref()?, self.path_for_prim(prim)?, set_name)?;
        Ok(VariantLayerState {
            root_selection: (!raw.root_selection.is_empty()).then_some(raw.root_selection),
            session_selection: (!raw.session_selection.is_empty()).then_some(raw.session_selection),
        })
    }

    
    /// Authors the root- and session-layer variant selections for `set_name` on
    /// `prim`; an absent selection in `state` clears that layer's opinion. Errors
    /// if `prim` is stale.
    pub fn set_variant_layer_state(
        &mut self,
        prim: PrimId,
        set_name: &str,
        state: &VariantLayerState,
    ) -> Result<(), StageError> {
        self.validate(prim)?;
        let path = self.path_for_prim(prim)?.to_owned();
        ffi::set_variant_layer_state(
            self.handle.pin_mut(),
            &path,
            set_name,
            state.root_selection.as_deref().unwrap_or_default(),
            state.session_selection.as_deref().unwrap_or_default(),
        )?;
        Ok(())
    }

    
    /// Manifests (bakes into the root layer) the `selection` of `set_name` on
    /// `prim`. Errors if `prim` is stale.
    pub fn manifest_variant(
        &mut self,
        prim: PrimId,
        set_name: &str,
        selection: &str,
    ) -> Result<(), StageError> {
        self.validate(prim)?;
        let path = self.path_for_prim(prim)?.to_owned();
        ffi::manifest_variant(self.handle.pin_mut(), &path, set_name, selection)?;
        Ok(())
    }

    
    /// Batch form of [`Self::manifest_variant`]: manifests each
    /// [`VariantManifestEdit`] in one bridge call. Errors if any edit's prim is
    /// stale.
    pub fn manifest_variants(&mut self, edits: &[VariantManifestEdit]) -> Result<(), StageError> {
        let raw = edits
            .iter()
            .map(|edit| {
                self.validate(edit.prim)?;
                Ok(ffi::VariantSelectionEditRaw {
                    path: self.path_for_prim(edit.prim)?.to_owned(),
                    set_name: edit.set_name.clone(),
                    selection: edit.selection.clone(),
                })
            })
            .collect::<Result<Vec<_>, StageError>>()?;
        ffi::manifest_variants(self.handle.pin_mut(), &raw)?;
        Ok(())
    }

    
    /// Batch form of [`Self::set_variant_layer_state`]: applies each
    /// [`VariantLayerEdit`]'s root/session selections in one bridge call. Errors
    /// if any edit's prim is stale.
    pub fn set_variant_layer_states(
        &mut self,
        edits: &[VariantLayerEdit],
    ) -> Result<(), StageError> {
        let raw = edits
            .iter()
            .map(|edit| {
                self.validate(edit.prim)?;
                Ok(ffi::VariantLayerEditRaw {
                    path: self.path_for_prim(edit.prim)?.to_owned(),
                    set_name: edit.set_name.clone(),
                    root_selection: edit.state.root_selection.clone().unwrap_or_default(),
                    session_selection: edit.state.session_selection.clone().unwrap_or_default(),
                })
            })
            .collect::<Result<Vec<_>, StageError>>()?;
        ffi::set_variant_layer_states(self.handle.pin_mut(), &raw)?;
        Ok(())
    }

    

    
    /// Saves the stage to `destination` and returns a [`PersistenceResult`]
    /// describing what was written. Errors with [`StageError`] if the save fails.
    pub fn save_stage(
        &self,
        destination: impl AsRef<Path>,
    ) -> Result<PersistenceResult, StageError> {
        let destination_path = destination.as_ref();
        #[cfg(feature = "persist-diagnostics")]
        log_save_dependencies("save_stage entry", destination_path);
        let destination = path_text(destination_path)?;
        let raw = match ffi::save_stage(self.handle_ref()?, destination) {
            Ok(raw) => raw,
            Err(err) => {
                #[cfg(feature = "persist-diagnostics")]
                {
                    tracing::warn!("save_stage failed: {err}");
                    log_save_dependencies("save_stage error", destination_path);
                }
                return Err(err.into());
            }
        };
        Ok(persistence_result(raw))
    }

    
    /// Exports only the root layer to `destination`, returning a
    /// [`PersistenceResult`]. Errors with [`StageError`] on failure.
    pub fn export_root_layer(
        &self,
        destination: impl AsRef<Path>,
    ) -> Result<PersistenceResult, StageError> {
        let destination = path_text(destination.as_ref())?;
        let raw = ffi::export_root_layer(self.handle_ref()?, destination)?;
        Ok(persistence_result(raw))
    }

    
    /// Exports the fully composed (flattened) stage to `destination`, returning a
    /// [`PersistenceResult`]. Errors with [`StageError`] on failure.
    pub fn export_flattened_stage(
        &self,
        destination: impl AsRef<Path>,
    ) -> Result<PersistenceResult, StageError> {
        let destination = path_text(destination.as_ref())?;
        let raw = ffi::export_flattened_stage(self.handle_ref()?, destination)?;
        Ok(persistence_result(raw))
    }

    /// Packages a loose (`.usda`/`.usdc`) stage and all of its referenced assets
    /// into a self-contained `.usdz` at `destination`.
    ///
    /// The current in-memory root layer is exported to a temporary sibling of
    /// `source_path` (i.e. into the same directory) so that relative asset
    /// references still resolve during dependency localization — exporting to an
    /// unrelated directory such as the system temp dir would re-base every
    /// relative reference and leave them unresolved. The temporary file is
    /// removed after packaging, regardless of success.
    ///
    /// `source_path` is used only to determine that sibling directory; it does
    /// not need to reflect unsaved edits (those come from the live stage).
    ///
    /// Returns an error (rather than a silent success) when any referenced asset
    /// cannot be resolved, since [`package_stage`] cannot build a package in that
    /// case.
    
    pub fn package_loose_as_usdz(
        &self,
        source_path: &Path,
        destination: &Path,
    ) -> Result<PackageResult, StageError> {
        let directory = source_path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .map_or_else(std::env::temp_dir, Path::to_path_buf);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos();
        let temp = directory.join(format!(".idtx-usdz-{}-{nanos}.usdc", std::process::id()));

        self.export_root_layer(&temp)?;
        let packaged = package_stage(&temp, destination);
        let _ = std::fs::remove_file(&temp);
        let packaged = packaged?;

        if !packaged.unresolved.is_empty() {
            return Err(StageError::UnresolvedAssets(packaged.unresolved));
        }
        Ok(packaged)
    }

    
    /// Verifies that `persisted` faithfully round-trips this stage under the
    /// given [`PersistenceVerification`] mode (save / root-export /
    /// flattened-export). Errors with [`StageError`] if verification fails.
    pub fn verify_persistence(
        &self,
        persisted: &Self,
        verification: PersistenceVerification,
    ) -> Result<(), StageError> {
        let mode = match verification {
            PersistenceVerification::Save => "save",
            PersistenceVerification::RootExport => "root-export",
            PersistenceVerification::FlattenedExport => "flattened-export",
        };
        ffi::verify_persistence(self.handle_ref()?, persisted.handle_ref()?, mode)?;
        Ok(())
    }

    /// Loads or unloads the payload(s) on `prim` by toggling its load state.
    /// Errors if `prim` is stale.
    pub fn set_payload_loaded(&mut self, prim: PrimId, loaded: bool) -> Result<(), StageError> {
        self.validate(prim)?;
        let path = self.path_for_prim(prim)?.to_owned();
        ffi::set_payload_loaded(self.handle.pin_mut(), &path, loaded)?;
        Ok(())
    }

    /// Returns `true` when the prim has no `"invisible"` authored directly on
    /// it (i.e., its authored visibility attribute is `"inherited"` or unset).
    ///
    /// Used during initial hydration to seed the ECS `VisibilityOverride`.
    pub fn prim_authored_visible(&self, prim: PrimId) -> Result<bool, StageError> {
        self.validate(prim)?;
        let path = self.path_for_prim(prim)?;
        Ok(ffi::get_prim_authored_visibility(self.handle_ref()?, path)?)
    }

    /// Reads the bytes of an already-resolved asset path through USD's asset
    /// resolver. Errors with [`StageError::OpenUsd`] if the asset cannot be read.
    pub fn read_asset(&self, resolved_path: &str) -> Result<Vec<u8>, StageError> {
        ffi::read_asset(self.handle_ref()?, resolved_path)
            .map_err(|error| StageError::OpenUsd(error.what().to_owned()))
    }

    /// Drains queued stage change notices into a [`StageChangeBatch`], bumping
    /// the revision when any change is present (`Ok(None)` when none). Maps each
    /// changed path to its [`PrimId`] (resync changes resolve to the nearest live
    /// ancestor) and records the from/to revisions. Errors with
    /// [`StageError::RevisionOverflow`] if the revision counter overflows.
    pub fn drain_changes(&mut self) -> Result<Option<StageChangeBatch>, StageError> {
        let started = Instant::now();
        let raw = ffi::drain_changes(self.handle.pin_mut())?;
        self.metrics.notice_drain.calls += 1;
        self.metrics.notice_drain.records += u64::try_from(raw.len()).unwrap_or(u64::MAX);
        self.metrics.notice_drain.bytes += raw
            .iter()
            .map(|record| record.prim_path.len() + record.property_name.len() + 5)
            .map(|bytes| u64::try_from(bytes).unwrap_or(u64::MAX))
            .sum::<u64>();
        self.metrics.notice_drain.elapsed += started.elapsed();
        if raw.is_empty() {
            return Ok(None);
        }
        let from_revision = self.revision;
        self.revision = StageRevision(
            self.revision
                .0
                .checked_add(1)
                .ok_or(StageError::RevisionOverflow)?,
        );
        let changes = raw
            .into_iter()
            .map(|record| {
                Ok(ChangedPrim {
                    prim: self.prim_for_change_path(&record.prim_path, record.resync)?,
                    property_name: (!record.property_name.is_empty())
                        .then_some(record.property_name),
                    domains: DirtyDomains(record.domains),
                    resync: record.resync,
                    authored_in_root_layer: record.authored_in_root_layer,
                })
            })
            .collect::<Result<Vec<_>, StageError>>()?;
        self.compact_identity_archive();
        Ok(Some(StageChangeBatch {
            generation: self.generation,
            from_revision,
            to_revision: self.revision,
            changes,
        }))
    }

    pub(crate) fn prim_for_path(&self, path: &str) -> Result<PrimId, StageError> {
        self.paths
            .get_id(path)
            .and_then(|id| self.prim_by_path.get(&id).copied())
            .ok_or_else(|| StageError::UnknownPath(path.to_owned()))
    }

    fn compact_identity_archive(&mut self) {
        self.retired_identities.retain(|entry| {
            self.known_prim_by_path.get(&entry.path) == Some(&entry.prim)
                && self.prim_by_path.get(&entry.path) != Some(&entry.prim)
        });
        loop {
            let over_capacity =
                self.retired_identities.len() > self.identity_retention.max_tombstones;
            let expired = self.retired_identities.front().is_some_and(|entry| {
                self.revision.0.saturating_sub(entry.retired_at.0)
                    > self.identity_retention.revision_window
            });
            if !over_capacity && !expired {
                break;
            }
            let Some(entry) = self.retired_identities.pop_front() else {
                break;
            };
            if self.known_prim_by_path.get(&entry.path) == Some(&entry.prim)
                && self.prim_by_path.get(&entry.path) != Some(&entry.prim)
            {
                self.known_prim_by_path.remove(&entry.path);
                self.path_by_prim.remove(&entry.prim);
                self.paths.remove(entry.path);
            }
        }
    }

    fn prim_for_change_path(&self, path: &str, resync: bool) -> Result<PrimId, StageError> {
        if let Ok(prim) = self.prim_for_path(path) {
            return Ok(prim);
        }
        if resync {
            let mut ancestor = path;
            while let Some(separator) = ancestor.rfind('/') {
                ancestor = if separator == 0 {
                    "/"
                } else {
                    &ancestor[..separator]
                };
                if let Ok(prim) = self.prim_for_path(ancestor) {
                    return Ok(prim);
                }
                if ancestor == "/" {
                    break;
                }
            }
        }
        Err(StageError::UnknownPath(path.to_owned()))
    }

    fn handle_ref(&self) -> Result<&ffi::StageHandle, StageError> {
        self.handle.as_ref().ok_or(StageError::NullHandle)
    }

    pub(crate) fn path_for_prim(&self, prim: PrimId) -> Result<&str, StageError> {
        self.validate(prim)?;
        self.path_by_prim
            .get(&prim)
            .and_then(|path| self.paths.get(*path))
            .ok_or_else(|| StageError::UnknownPath(format!("prim slot {}", prim.slot)))
    }

    fn validate(&self, prim: PrimId) -> Result<(), StageError> {
        if prim.generation != self.generation {
            return Err(StageError::InvalidProjection {
                path: format!("prim slot {}", prim.slot),
                detail: "stale stage generation".to_owned(),
            });
        }
        Ok(())
    }

    /// Reloads the layer identified by `layer_path` from disk. Errors with
    /// [`StageError::OpenUsd`] if the reload fails.
    pub fn reload_layer(&mut self, layer_path: &str) -> Result<(), StageError> {
        ffi::reload_layer(self.handle.pin_mut(), layer_path)
            .map_err(|e| StageError::OpenUsd(e.what().to_owned()))
    }

    /// Returns lightweight summaries of the stage's `UsdSkel` data: skeletons
    /// (joint count, anim-query presence, bound animation), skin bindings (point
    /// count, method, influences-per-point), and animations (time range and which
    /// channels are authored). Errors with [`StageError::OpenUsd`] on failure.
    pub fn skel_summaries(&self) -> Result<SkelSummaries, StageError> {
        let raw = ffi::skel_summaries(self.handle_ref()?)
            .map_err(|e| StageError::OpenUsd(e.what().to_owned()))?;
        Ok(SkelSummaries {
            skeletons: raw
                .skeletons
                .into_iter()
                .map(|r| SkeletonSummary {
                    path: r.path.clone(),
                    joint_count: r.joint_count,
                    has_anim_query: r.has_anim_query,
                    animation_path: if r.animation_path.is_empty() {
                        None
                    } else {
                        Some(r.animation_path.clone())
                    },
                })
                .collect(),
            skin_bindings: raw
                .skin_bindings
                .into_iter()
                .map(|r| SkinBindingSummary {
                    path: r.path.clone(),
                    skeleton_path: if r.skeleton_path.is_empty() {
                        None
                    } else {
                        Some(r.skeleton_path.clone())
                    },
                    point_count: r.point_count,
                    skinning_method: r.skinning_method.clone(),
                    influences_per_point: r.influences_per_point,
                })
                .collect(),
            animations: raw
                .animations
                .into_iter()
                .map(|r| AnimationSummary {
                    path: r.path.clone(),
                    skeleton_path: if r.skeleton_path.is_empty() {
                        None
                    } else {
                        Some(r.skeleton_path.clone())
                    },
                    start_time_code: r.start_time_code,
                    end_time_code: r.end_time_code,
                    has_translations: r.has_translations,
                    has_rotations: r.has_rotations,
                    has_scales: r.has_scales,
                    has_blend_shapes: r.has_blend_shapes,
                })
                .collect(),
        })
    }

    /// Captures a change-observer checkpoint token that can later be passed to
    /// [`Self::observer_rollback_to`] to undo intervening edits. Errors with
    /// [`StageError::OpenUsd`] on failure.
    pub fn observer_checkpoint(&self) -> Result<u64, StageError> {
        ffi::observer_checkpoint(self.handle_ref()?)
            .map_err(|e| StageError::OpenUsd(e.what().to_owned()))
    }

    /// Returns the stage's composed layer stack (session layer + root layer +
    /// recursive sublayers), strongest-first, for the Layer Stack panel.
    ///
    /// Read-only and requested on demand — the consumer is expected to cache the
    /// result and refresh only when topology changes, so this never runs on the
    /// per-frame render path.
    pub fn layer_stack(&self) -> Result<Vec<LayerRecord>, StageError> {
        let raw = ffi::layer_stack(self.handle_ref()?)
            .map_err(|e| StageError::OpenUsd(e.what().to_owned()))?;
        Ok(raw
            .into_iter()
            .map(|r| LayerRecord {
                identifier: r.identifier.clone(),
                display_name: r.display_name.clone(),
                real_path: (!r.real_path.is_empty()).then(|| r.real_path.clone()),
                arc: match r.arc.as_str() {
                    "session" => LayerArcKind::Session,
                    "root" => LayerArcKind::Root,
                    "reference" => LayerArcKind::Reference,
                    "payload" => LayerArcKind::Payload,
                    _ => LayerArcKind::SubLayer,
                },
                arc_source_path: (!r.arc_source_path.is_empty()).then(|| r.arc_source_path.clone()),
                arc_target_path: (!r.arc_target_path.is_empty()).then(|| r.arc_target_path.clone()),
                has_specs: r.has_specs,
                is_loaded: r.is_loaded,
                depth: r.depth,
                offset: r.offset,
                scale: r.scale,
                delta_count: r.delta_count,
                is_anonymous: r.is_anonymous,
                is_dirty: r.is_dirty,
                is_muted: r.is_muted,
                is_edit_target: r.is_edit_target,
                parent_identifier: r.parent_identifier.clone(),
            })
            .collect())
    }

    /// Rolls the stage back to a `checkpoint` previously returned by
    /// [`Self::observer_checkpoint`], undoing edits made since. Errors with
    /// [`StageError::OpenUsd`] on failure.
    pub fn observer_rollback_to(&mut self, checkpoint: u64) -> Result<(), StageError> {
        ffi::observer_rollback_to(self.handle.pin_mut(), checkpoint)
            .map_err(|e| StageError::OpenUsd(e.what().to_owned()))
    }

    /// Whether the stage's root layer currently holds unsaved edits
    /// (`SdfLayer::IsDirty`). A single cheap bool getter — safe to poll on
    /// authoring change batches. Session-layer edits never dirty the root layer.
    pub fn root_layer_is_dirty(&self) -> Result<bool, StageError> {
        ffi::root_layer_is_dirty(self.handle_ref()?)
            .map_err(|e| StageError::OpenUsd(e.what().to_owned()))
    }

    /// Content signature (hash of the serialized root-layer text) used to confirm
    /// a dirty root layer against a baseline captured at save time. Because
    /// `SdfLayer::IsDirty` stays `true` after an `Export`-based Save As, comparing
    /// this hash to the saved baseline tells the session whether the root layer
    /// actually holds unsaved edits. Pair it with [`Self::root_layer_is_dirty`] as
    /// a cheap pre-filter — only hash when the dirty bit is set.
    pub fn root_layer_content_hash(&self) -> Result<u64, StageError> {
        ffi::root_layer_content_hash(self.handle_ref()?)
            .map_err(|e| StageError::OpenUsd(e.what().to_owned()))
    }

    /// Transpiles the surface shader bound to `prim` to WGSL, returning the
    /// material path, WGSL source, a `requires_phase2` flag (GLSL needing a
    /// second naga pass), a fingerprint, and diagnostics. Errors if `prim` is
    /// stale or the bridge call fails.
    pub fn material_surface_wgsl(&self, prim: PrimId) -> Result<WgslMaterial, StageError> {
        self.validate(prim)?;
        let path = self.path_for_prim(prim)?.to_owned();
        let raw = ffi::material_surface_wgsl(self.handle_ref()?, &path)
            .map_err(|e| StageError::OpenUsd(e.what().to_owned()))?;
        Ok(WgslMaterial {
            material_path: raw.material_path.clone(),
            wgsl_source: raw.wgsl_source.clone(),
            requires_phase2: raw.requires_phase2,
            fingerprint: raw.fingerprint.clone(),
            diagnostics: raw.diagnostics.clone(),
        })
    }

    /// Evaluates attribute `attr_name` on `prim` at `time_code` (or the default
    /// time when `use_default_time`), returning its type, value representation,
    /// time-varying flag, and sample count, or `None` when unset. Errors if
    /// `prim` is stale or the read fails.
    pub fn eval_attribute(
        &self,
        prim: PrimId,
        attr_name: &str,
        time_code: f64,
        use_default_time: bool,
    ) -> Result<Option<AttributeValue>, StageError> {
        self.validate(prim)?;
        let path = self.path_for_prim(prim)?.to_owned();
        let raw = ffi::eval_attribute(
            self.handle_ref()?,
            &path,
            attr_name,
            time_code,
            use_default_time,
        )
        .map_err(|e| StageError::OpenUsd(e.what().to_owned()))?;
        if !raw.has_value {
            return Ok(None);
        }
        Ok(Some(AttributeValue {
            type_name: raw.type_name.clone(),
            value_repr: raw.value_json.clone(),
            is_time_varying: raw.is_time_varying,
            time_sample_count: raw.time_sample_count,
        }))
    }

    /// Typed read of a `point3f[]`-family attribute as `Vec<[f32; 3]>` (the
    /// escape hatch for custom-schema array geometry such as the
    /// `IdtxCollisionAPI` collision hull). Returns `None` when the attribute is
    /// absent/unset, `Some(vec)` (possibly empty) when it resolves.
    ///
    /// # Errors
    /// [`StageError`] when `prim` is stale or the bridge read fails.
    pub fn eval_point3f_array(
        &self,
        prim: PrimId,
        attr_name: &str,
        time_code: f64,
        use_default_time: bool,
    ) -> Result<Option<Vec<[f32; 3]>>, StageError> {
        self.validate(prim)?;
        let path = self.path_for_prim(prim)?.to_owned();
        let flat =
            ffi::eval_point3f_array(self.handle_ref()?, &path, attr_name, time_code, use_default_time)
                .map_err(|e| StageError::OpenUsd(e.what().to_owned()))?;
        if flat.is_empty() {
            // Distinguish "absent" from "authored empty" is not needed by any
            // caller; an empty buffer maps to an empty Vec so hydration can treat
            // it uniformly.
            return Ok(Some(Vec::new()));
        }
        let points = flat
            .chunks_exact(3)
            .map(|c| [c[0], c[1], c[2]])
            .collect::<Vec<_>>();
        Ok(Some(points))
    }

    /// Typed read of an `int[]` attribute (e.g. a flat triangle index list).
    ///
    /// # Errors
    /// [`StageError`] when `prim` is stale or the bridge read fails.
    pub fn eval_int_array(
        &self,
        prim: PrimId,
        attr_name: &str,
        time_code: f64,
        use_default_time: bool,
    ) -> Result<Vec<i32>, StageError> {
        self.validate(prim)?;
        let path = self.path_for_prim(prim)?.to_owned();
        ffi::eval_int_array(self.handle_ref()?, &path, attr_name, time_code, use_default_time)
            .map_err(|e| StageError::OpenUsd(e.what().to_owned()))
    }

    /// Typed read of a `token[]` attribute as owned strings (e.g. the
    /// `IdtxCollisionAPI` channel list).
    ///
    /// # Errors
    /// [`StageError`] when `prim` is stale or the bridge read fails.
    pub fn eval_token_array(
        &self,
        prim: PrimId,
        attr_name: &str,
        time_code: f64,
        use_default_time: bool,
    ) -> Result<Vec<String>, StageError> {
        self.validate(prim)?;
        let path = self.path_for_prim(prim)?.to_owned();
        ffi::eval_token_array(self.handle_ref()?, &path, attr_name, time_code, use_default_time)
            .map_err(|e| StageError::OpenUsd(e.what().to_owned()))
    }

    /// Read the authored `idtx:playerRole` token on `prim` at the default time
    /// (plan §2.1), or `None` when the attribute is absent or resolves to an
    /// empty token. The raw token (`local`/`remote`/`server`/`unowned`) is
    /// returned verbatim; higher layers map it to their `NetRole` taxonomy.
    ///
    /// # Errors
    /// [`StageError`] when `prim` is stale or the bridge read fails.
    pub fn read_player_role(&self, prim: PrimId) -> Result<Option<String>, StageError> {
        Ok(self
            .eval_attribute(prim, PLAYER_ROLE_ATTR, 0.0, true)?
            .and_then(|v| trim_token_repr(&v.value_repr)))
    }

    /// Read the authored `idtx:playerId` string on `prim` at the default time,
    /// or `None` when absent/empty.
    ///
    /// # Errors
    /// [`StageError`] when `prim` is stale or the bridge read fails.
    pub fn read_player_id(&self, prim: PrimId) -> Result<Option<String>, StageError> {
        Ok(self
            .eval_attribute(prim, PLAYER_ID_ATTR, 0.0, true)?
            .and_then(|v| trim_token_repr(&v.value_repr)))
    }

    /// Author the `idtx:playerRole` token on `prim` (plan §2.1) — the authority
    /// seam consumed by the behavior-host gate. Reuses the existing
    /// `SetProperty` token path (`PropertyValue::Token`), so no bespoke bridge
    /// call is needed. Pass a `NetRole` token such as
    /// `"local"` / `"remote"` / `"server"`.
    ///
    /// # Errors
    /// [`StageError`] when `prim` is stale or the authoring write fails.
    
    pub fn set_player_role(&mut self, prim: PrimId, role: &str) -> Result<(), StageError> {
        self.apply_structural_command(&StructuralCommand::SetProperty {
            prim,
            property: PLAYER_ROLE_ATTR.to_owned(),
            value: PropertyValue::Token(role.to_owned()),
        })
    }

    /// Author the `idtx:playerId` string on `prim`. Reuses the `SetProperty`
    /// string path (`PropertyValue::String`).
    ///
    /// # Errors
    /// [`StageError`] when `prim` is stale or the authoring write fails.
    
    pub fn set_player_id(&mut self, prim: PrimId, id: &str) -> Result<(), StageError> {
        self.apply_structural_command(&StructuralCommand::SetProperty {
            prim,
            property: PLAYER_ID_ATTR.to_owned(),
            value: PropertyValue::String(id.to_owned()),
        })
    }

    /// Returns whether `prim` has the applied API schema `schema_name`. Errors
    /// if `prim` is stale or the query fails.
    pub fn prim_has_api(&self, prim: PrimId, schema_name: &str) -> Result<bool, StageError> {
        self.validate(prim)?;
        let path = self.path_for_prim(prim)?.to_owned();
        ffi::prim_has_api(self.handle_ref()?, &path, schema_name)
            .map_err(|e| StageError::OpenUsd(e.what().to_owned()))
    }

    /// Returns whether `prim` is (or derives from) the schema type `type_name`.
    /// Errors if `prim` is stale or the query fails.
    pub fn prim_is_a(&self, prim: PrimId, type_name: &str) -> Result<bool, StageError> {
        self.validate(prim)?;
        let path = self.path_for_prim(prim)?.to_owned();
        ffi::prim_is_a(self.handle_ref()?, &path, type_name)
            .map_err(|e| StageError::OpenUsd(e.what().to_owned()))
    }
}