use super::StageService;
use super::{
    PROJECTION_SCHEMA_VERSION, RetiredIdentity, affine_uv_transform, color, flat_to_colors,
    logical_material_bytes, material_projection_error, path_is_below, projection_matrices,
    projection_matrix, projection_points, surface_ir,
};
use crate::{
    BoundsRecord, CameraRecord, CameraSelectionPolicy, CameraSelectionRecord, CubeRecord,
    DomeTextureFormat, ExtendedGprimKind, ExtendedGprimRecord, GprimAxis, LightKind, LightRecord,
    MaterialRecord, MaterialSubsetRecord, MeshRecord, NativeInstanceRecord, PackedArc,
    PackedArcKind, PlaneRecord, PointInstancerRecord, PoleAxis, PrimId, PrimSpecifier, PrimState,
    RenderPurpose, SkeletonDependencyRecord, SkeletonJointRecord, SkeletonPoseRecord,
    SkeletonTopologyRecord, SkinBindingRecord, SkinnedBoundsRecord, SkinningExecution,
    SkinningMethod, SphereRecord, StageError, SurfaceFactors, SurfaceModel, TopologyDelta,
    TopologyRecord, TransformRecord, VertexInfluences, ViewportProjection, VisibilityRecord,
};
use idtx_usd_sys::bridge::ffi;
use std::collections::{HashMap, HashSet};
use std::time::Instant;

impl StageService {
    /// Reprojects the entire stage topology and returns the refreshed retained
    /// snapshot. Equivalent to [`Self::project_topology_paths`] with no roots.
    pub fn project_topology(&mut self) -> Result<&[TopologyRecord], StageError> {
        self.project_topology_paths(&[])?;
        Ok(&self.topology)
    }

    /// Reprojects only the requested topology subtrees. Missing roots are
    /// interpreted as deletion tombstones; the retained full snapshot is
    /// updated without asking `OpenUSD` to traverse unrelated prims.
    pub fn project_topology_paths(
        &mut self,
        roots: &[String],
    ) -> Result<TopologyDelta, StageError> {
        let started = Instant::now();
        let raw = ffi::project_topology(self.handle_ref()?, roots)?;
        let logical_bytes = raw
            .iter()
            .map(|record| {
                record.path.len()
                    + record.parent_path.len()
                    + record.name.len()
                    + record.type_name.len()
                    + record.local_specifier.len()
                    + record.local_layer_identifier.len()
                    + record.packed_arc.len()
                    + record.packed_asset_path.len()
                    + 4
            })
            .map(|bytes| u64::try_from(bytes).unwrap_or(u64::MAX))
            .sum::<u64>();
        let mut paths = std::mem::take(&mut self.paths);
        let mut known_prim_by_path = std::mem::take(&mut self.known_prim_by_path);
        let mut path_by_prim = std::mem::take(&mut self.path_by_prim);
        let full_projection = roots.is_empty() || roots.iter().any(|root| root == "/");
        let mut prim_by_path = std::mem::take(&mut self.prim_by_path);
        let replaced = self
            .topology
            .iter()
            .filter(|record| {
                full_projection
                    // Shared instancing prototypes are re-emitted in full by the
                    // shim's unconditional `GetPrototypes()` pass on EVERY
                    // projection — they are stage-global and cannot be reconciled
                    // by subtree path-containment (their `/__Prototype_N` roots
                    // are not below any requested authoring root). Always evict
                    // the existing prototype records so the freshly re-emitted,
                    // correctly-flagged set replaces them exactly. Without this a
                    // subtree projection stacks a second flagged copy of every
                    // prototype prim on top of the previous one.
                    || record.is_prototype
                    || paths
                        .get(record.path)
                        .is_some_and(|path| roots.iter().any(|root| path_is_below(path, root)))
            })
            .map(|record| record.prim)
            .collect::<HashSet<_>>();
        if full_projection {
            prim_by_path.clear();
        } else {
            for record in self
                .topology
                .iter()
                .filter(|record| replaced.contains(&record.prim))
            {
                prim_by_path.remove(&record.path);
            }
        }
        for record in &raw {
            let path = paths.intern(record.path.clone())?;
            let prim = if let Some(prim) = known_prim_by_path.get(&path) {
                *prim
            } else {
                let slot = self.next_prim_slot;
                self.next_prim_slot = self.next_prim_slot.checked_add(1).ok_or_else(|| {
                    StageError::InvalidProjection {
                        path: record.path.clone(),
                        detail: "prim identity counter exhausted".to_owned(),
                    }
                })?;
                let prim = PrimId {
                    slot,
                    generation: self.generation,
                };
                known_prim_by_path.insert(path, prim);
                path_by_prim.insert(prim, path);
                prim
            };
            prim_by_path.insert(path, prim);
        }
        let mut upserts = Vec::with_capacity(raw.len());
        for record in raw {
            let path = paths
                .get_id(&record.path)
                .ok_or_else(|| StageError::UnknownPath(record.path.clone()))?;
            let prim = prim_by_path[&path];
            let parent = if record.parent_path.is_empty() {
                None
            } else {
                let parent_path = paths
                    .get_id(&record.parent_path)
                    .ok_or_else(|| StageError::UnknownPath(record.parent_path.clone()))?;
                Some(
                    *prim_by_path
                        .get(&parent_path)
                        .ok_or_else(|| StageError::UnknownPath(record.parent_path.clone()))?,
                )
            };
            let local_specifier = match record.local_specifier.as_str() {
                "" => None,
                "def" => Some(PrimSpecifier::Def),
                "over" => Some(PrimSpecifier::Over),
                "class" => Some(PrimSpecifier::Class),
                other => {
                    return Err(StageError::InvalidProjection {
                        path: record.path,
                        detail: format!("unknown prim specifier {other}"),
                    });
                }
            };
            let packed_arc = match record.packed_arc.as_str() {
                "" => None,
                "reference" => Some(PackedArc {
                    kind: PackedArcKind::Reference,
                    asset_path: record.packed_asset_path,
                }),
                "payload" => Some(PackedArc {
                    kind: PackedArcKind::Payload,
                    asset_path: record.packed_asset_path,
                }),
                other => {
                    return Err(StageError::InvalidProjection {
                        path: record.path,
                        detail: format!("unknown packed arc {other}"),
                    });
                }
            };
            upserts.push(TopologyRecord {
                prim,
                path,
                parent,
                name: record.name,
                type_name: record.type_name,
                state: PrimState {
                    active: record.active,
                    defined: record.defined,
                    loaded: record.loaded,
                },
                is_pseudo_root: record.is_pseudo_root,
                local_specifier,
                local_layer_identifier: (!record.local_layer_identifier.is_empty())
                    .then_some(record.local_layer_identifier),
                packed_arc,
                internal_ref_target: (!record.internal_ref_target.is_empty())
                    .then_some(record.internal_ref_target),
                is_xformable: record.is_xformable,
                is_instance: record.is_instance,
                is_prototype: record.is_prototype,
            });
        }
        let upsert_prims = upserts
            .iter()
            .map(|record| record.prim)
            .collect::<HashSet<_>>();
        let removed = replaced
            .difference(&upsert_prims)
            .copied()
            .collect::<HashSet<_>>();
        if full_projection {
            self.topology.clone_from(&upserts);
        } else {
            self.topology
                .retain(|record| !replaced.contains(&record.prim));
            self.topology.extend(upserts.iter().cloned());
            self.topology
                .sort_by(|left, right| paths.get(left.path).cmp(&paths.get(right.path)));
        }
        self.paths = paths;
        self.known_prim_by_path = known_prim_by_path;
        self.prim_by_path = prim_by_path;
        self.path_by_prim = path_by_prim;
        for prim in &removed {
            if let Some(path) = self.path_by_prim.get(prim).copied() {
                self.retired_identities.push_back(RetiredIdentity {
                    prim: *prim,
                    path,
                    retired_at: self.revision,
                });
            }
        }
        self.compact_identity_archive();
        self.metrics.topology.calls += 1;
        self.metrics.topology.records += u64::try_from(upserts.len()).unwrap_or(u64::MAX);
        self.metrics.topology.bytes += logical_bytes;
        self.metrics.topology.elapsed += started.elapsed();
        Ok(TopologyDelta { upserts, removed })
    }

    /// Projects all viewport-facing data for the whole stage at the stage default
    /// time code. Shorthand for a full (rootless) default-time projection.
    pub fn project_viewport(&mut self) -> Result<ViewportProjection, StageError> {
        self.project_viewport_at(None, &[], false)
    }

    /// Projects only the requested prim subtrees. Roots are coalesced by the
    /// native bridge before traversal, so a leaf edit does not require a full
    /// stage viewport projection.
    pub fn project_viewport_roots(
        &mut self,
        roots: &[PrimId],
    ) -> Result<ViewportProjection, StageError> {
        self.project_viewport_at(None, roots, false)
    }

    /// Samples all viewport-facing stage data in one `OpenUSD` bridge call.
    /// Passing host-stage time lets `OpenUSD` apply nested reference/payload
    /// layer offsets instead of recreating time mapping in Rust.
    pub fn sample_viewport(&mut self, time_code: f64) -> Result<ViewportProjection, StageError> {
        self.sample_viewport_roots(time_code, &[])
    }

    /// Samples viewport data for the given roots at `time_code`. Errors with
    /// [`StageError`] if `time_code` is not finite; an empty `roots` slice samples
    /// the whole stage.
    pub fn sample_viewport_roots(
        &mut self,
        time_code: f64,
        roots: &[PrimId],
    ) -> Result<ViewportProjection, StageError> {
        if !time_code.is_finite() {
            return Err(StageError::InvalidProjection {
                path: "/".to_owned(),
                detail: "sample time code must be finite".to_owned(),
            });
        }
        self.project_viewport_at(Some(time_code), roots, false)
    }

    /// Samples only the skeletal pose data (joint palettes, topologies, and skin
    /// bindings) for the given roots — skipping the full mesh geometry, material,
    /// light, and camera re-projection. Use this for prims whose only animated
    /// attributes drive a `UsdSkel` skeleton (joint `rotations`/`translations`/
    /// `scales`): the mesh geometry is static, so only the joint palette needs to
    /// be resampled each frame. This is the skeletal analog of
    /// [`Self::sample_transforms_only`].
    pub fn sample_skeleton_poses(
        &mut self,
        time_code: f64,
        roots: &[PrimId],
    ) -> Result<ViewportProjection, StageError> {
        if !time_code.is_finite() {
            return Err(StageError::InvalidProjection {
                path: "/".to_owned(),
                detail: "sample time code must be finite".to_owned(),
            });
        }
        self.project_viewport_at(Some(time_code), roots, true)
    }

    /// Samples only world transforms for the given prim IDs using a
    /// `UsdGeomXformCache` traversal — no geometry, material, bounds, or
    /// skeleton extraction.  Use this for prims whose only animated attributes
    /// are `xformOp.*` operations.
    pub fn sample_transforms_only(
        &mut self,
        time_code: f64,
        prim_ids: &[PrimId],
    ) -> Result<Vec<TransformRecord>, StageError> {
        if !time_code.is_finite() {
            return Err(StageError::InvalidProjection {
                path: "/".to_owned(),
                detail: "sample time code must be finite".to_owned(),
            });
        }
        self.project_transforms_only(Some(time_code), prim_ids)
    }

    /// Like [`Self::sample_transforms_only`] but samples at the stage default
    /// time (`UsdTimeCode::Default()`). Use this on the authoring transform
    /// fast path, where there is no animation clock: an ancestor / default-prim
    /// transform edit only needs fresh world matrices for the moved subtree,
    /// never a material, geometry, or light re-resolve (which would re-hydrate
    /// the subtree and emit spurious `MaterialBindingAPI` warnings). Matches the
    /// default-time behaviour of [`Self::project_viewport_roots`].
    pub fn sample_transforms_only_default(
        &mut self,
        prim_ids: &[PrimId],
    ) -> Result<Vec<TransformRecord>, StageError> {
        self.project_transforms_only(None, prim_ids)
    }

    /// Shared implementation for the transform-only samplers. `None` samples at
    /// `UsdTimeCode::Default()`; `Some(tc)` at the given (finite) time code.
    fn project_transforms_only(
        &mut self,
        time_code: Option<f64>,
        prim_ids: &[PrimId],
    ) -> Result<Vec<TransformRecord>, StageError> {
        let paths = prim_ids
            .iter()
            .map(|id| self.path_for_prim(*id).map(str::to_owned))
            .collect::<Result<Vec<_>, _>>()?;
        let raw = ffi::project_transforms_at(
            self.handle_ref()?,
            time_code.unwrap_or_default(),
            time_code.is_none(),
            &paths,
        )?;
        raw.into_iter()
            .map(|r| {
                let matrix: [f32; 16] = r.world_matrix.try_into().map_err(|v: Vec<f32>| {
                    StageError::InvalidProjection {
                        path: r.path.clone(),
                        detail: format!("world matrix has {} values", v.len()),
                    }
                })?;
                Ok(TransformRecord {
                    prim: self.prim_for_path(&r.path)?,
                    world_matrix: matrix,
                })
            })
            .collect()
    }

    fn project_viewport_at(
        &mut self,
        time_code: Option<f64>,
        roots: &[PrimId],
        poses_only: bool,
    ) -> Result<ViewportProjection, StageError> {
        let started = Instant::now();
        let root_paths = roots
            .iter()
            .map(|root| self.path_for_prim(*root).map(str::to_owned))
            .collect::<Result<Vec<_>, _>>()?;
        let raw = if poses_only {
            ffi::project_skeleton_poses(
                self.handle_ref()?,
                time_code.unwrap_or_default(),
                time_code.is_none(),
                &root_paths,
            )?
        } else {
            ffi::project_viewport(
                self.handle_ref()?,
                time_code.unwrap_or_default(),
                time_code.is_none(),
                &root_paths,
            )?
        };
        self.decode_viewport(raw, time_code, started)
    }

    pub(crate) fn decode_viewport(
        &mut self,
        raw: ffi::ViewportProjectionRaw,
        time_code: Option<f64>,
        started: Instant,
    ) -> Result<ViewportProjection, StageError> {
        if raw.schema_version != PROJECTION_SCHEMA_VERSION {
            return Err(StageError::InvalidProjection {
                path: "/".to_owned(),
                detail: format!(
                    "viewport schema version {} is unsupported",
                    raw.schema_version
                ),
            });
        }
        let transforms = raw
            .transforms
            .into_iter()
            .map(|record| {
                let matrix: [f32; 16] =
                    record.world_matrix.try_into().map_err(|values: Vec<f32>| {
                        StageError::InvalidProjection {
                            path: record.path.clone(),
                            detail: format!("world matrix has {} values", values.len()),
                        }
                    })?;
                Ok(TransformRecord {
                    prim: self.prim_for_path(&record.path)?,
                    world_matrix: matrix,
                })
            })
            .collect::<Result<Vec<_>, StageError>>()?;
        let mut diagnostics = raw.diagnostics;
        let visibility = raw
            .visibility
            .into_iter()
            .map(|record| {
                let purpose = match record.purpose.as_str() {
                    "default" | "" => RenderPurpose::Default,
                    "proxy" => RenderPurpose::Proxy,
                    "render" => RenderPurpose::Render,
                    "guide" => RenderPurpose::Guide,
                    other => {
                        diagnostics.push(format!(
                            "{} uses unknown imageable purpose {other}; treating it as default",
                            record.path
                        ));
                        RenderPurpose::Default
                    }
                };
                Ok(VisibilityRecord {
                    prim: self.prim_for_path(&record.path)?,
                    visible: record.visible,
                    purpose,
                    casts_shadow: record.casts_shadow,
                })
            })
            .collect::<Result<Vec<_>, StageError>>()?;
        let bounds = raw
            .bounds
            .into_iter()
            .map(|record| {
                let minimum: [f32; 3] = record.minimum.try_into().map_err(|values: Vec<f32>| {
                    StageError::InvalidProjection {
                        path: record.path.clone(),
                        detail: format!("bounds minimum has {} values", values.len()),
                    }
                })?;
                let maximum: [f32; 3] = record.maximum.try_into().map_err(|values: Vec<f32>| {
                    StageError::InvalidProjection {
                        path: record.path.clone(),
                        detail: format!("bounds maximum has {} values", values.len()),
                    }
                })?;
                let local_minimum: [f32; 3] =
                    record
                        .local_minimum
                        .try_into()
                        .map_err(|values: Vec<f32>| StageError::InvalidProjection {
                            path: record.path.clone(),
                            detail: format!("bounds local_minimum has {} values", values.len()),
                        })?;
                let local_maximum: [f32; 3] =
                    record
                        .local_maximum
                        .try_into()
                        .map_err(|values: Vec<f32>| StageError::InvalidProjection {
                            path: record.path.clone(),
                            detail: format!("bounds local_maximum has {} values", values.len()),
                        })?;
                Ok(BoundsRecord {
                    prim: self.prim_for_path(&record.path)?,
                    minimum,
                    maximum,
                    local_minimum,
                    local_maximum,
                })
            })
            .collect::<Result<Vec<_>, StageError>>()?;
        let cubes = raw
            .cubes
            .into_iter()
            .map(|record| {
                Ok(CubeRecord {
                    prim: self.prim_for_path(&record.path)?,
                    size: record.size,
                    display_colors: flat_to_colors(&record.display_color),
                    material_path: (!record.material_path.is_empty())
                        .then_some(record.material_path),
                    double_sided: record.double_sided,
                    prototype_only: record.prototype_only,
                })
            })
            .collect::<Result<Vec<_>, StageError>>()?;
        let spheres = raw
            .spheres
            .into_iter()
            .map(|record| {
                Ok(SphereRecord {
                    prim: self.prim_for_path(&record.path)?,
                    radius: record.radius,
                    display_colors: flat_to_colors(&record.display_color),
                    material_path: (!record.material_path.is_empty())
                        .then_some(record.material_path),
                    double_sided: record.double_sided,
                    prototype_only: record.prototype_only,
                })
            })
            .collect::<Result<Vec<_>, StageError>>()?;
        let extended_gprims = raw
            .extended_gprims
            .into_iter()
            .map(|record| {
                let kind = match record.kind.as_str() {
                    "cone" => ExtendedGprimKind::Cone,
                    "cylinder" => ExtendedGprimKind::Cylinder,
                    "capsule" => ExtendedGprimKind::Capsule,
                    other => {
                        return Err(StageError::InvalidProjection {
                            path: record.path,
                            detail: format!("unknown extended gprim kind {other}"),
                        });
                    }
                };
                let Some(axis) = GprimAxis::from_token(record.axis.as_str()) else {
                    return Err(StageError::InvalidProjection {
                        path: record.path,
                        detail: format!("unknown gprim axis {}", record.axis.as_str()),
                    });
                };
                Ok(ExtendedGprimRecord {
                    prim: self.prim_for_path(&record.path)?,
                    kind,
                    radius: record.radius,
                    height: record.height,
                    axis,
                    display_colors: flat_to_colors(&record.display_color),
                    material_path: (!record.material_path.is_empty())
                        .then_some(record.material_path),
                    double_sided: record.double_sided,
                    prototype_only: record.prototype_only,
                })
            })
            .collect::<Result<Vec<_>, StageError>>()?;
        let planes = raw
            .planes
            .into_iter()
            .map(|record| {
                let Some(axis) = GprimAxis::from_token(record.axis.as_str()) else {
                    return Err(StageError::InvalidProjection {
                        path: record.path,
                        detail: format!("unknown gprim axis {}", record.axis.as_str()),
                    });
                };
                Ok(PlaneRecord {
                    prim: self.prim_for_path(&record.path)?,
                    width: record.width,
                    length: record.length,
                    axis,
                    display_colors: flat_to_colors(&record.display_color),
                    material_path: (!record.material_path.is_empty())
                        .then_some(record.material_path),
                    double_sided: record.double_sided,
                    prototype_only: record.prototype_only,
                })
            })
            .collect::<Result<Vec<_>, StageError>>()?;
        let meshes = raw
            .meshes
            .into_iter()
            .map(|record| {
                if record.points.len() % 3 != 0 {
                    return Err(StageError::InvalidProjection {
                        path: record.path,
                        detail: "point buffer is not xyz-aligned".to_owned(),
                    });
                }
                let points = record
                    .points
                    .chunks_exact(3)
                    .map(|p| [p[0], p[1], p[2]])
                    .collect();
                if record.texcoords.len() % 2 != 0 {
                    return Err(StageError::InvalidProjection {
                        path: record.path,
                        detail: "texture-coordinate buffer is not uv-aligned".to_owned(),
                    });
                }
                let texcoords = record
                    .texcoords
                    .chunks_exact(2)
                    .map(|uv| [uv[0], uv[1]])
                    .collect();
                if record.normals.len() % 3 != 0 {
                    return Err(StageError::InvalidProjection {
                        path: record.path,
                        detail: "normals buffer is not xyz-aligned".to_owned(),
                    });
                }
                let normals = record
                    .normals
                    .chunks_exact(3)
                    .map(|n| [n[0], n[1], n[2]])
                    .collect();
                if record.tangents.len() % 4 != 0 {
                    return Err(StageError::InvalidProjection {
                        path: record.path,
                        detail: "tangents buffer is not xyzw-aligned".to_owned(),
                    });
                }
                let tangents = record
                    .tangents
                    .chunks_exact(4)
                    .map(|t| [t[0], t[1], t[2], t[3]])
                    .collect();
                let mut subset_cursor = 0_usize;
                let mut material_subsets = Vec::with_capacity(record.subset_face_counts.len());
                if record.subset_material_paths.len() != record.subset_face_counts.len() {
                    return Err(StageError::InvalidProjection {
                        path: record.path,
                        detail: "material subset path/count arrays have different lengths"
                            .to_owned(),
                    });
                }
                for (material_path, count) in record
                    .subset_material_paths
                    .into_iter()
                    .zip(record.subset_face_counts)
                {
                    let count = usize::try_from(count).unwrap_or(usize::MAX);
                    let end = subset_cursor.checked_add(count).ok_or_else(|| {
                        StageError::InvalidProjection {
                            path: record.path.clone(),
                            detail: "material subset face cursor overflowed".to_owned(),
                        }
                    })?;
                    let raw_faces = record
                        .subset_face_indices
                        .get(subset_cursor..end)
                        .ok_or_else(|| StageError::InvalidProjection {
                            path: record.path.clone(),
                            detail: "material subset counts exceed face-index buffer".to_owned(),
                        })?;
                    let face_indices = raw_faces
                        .iter()
                        .map(|face| {
                            u32::try_from(*face).map_err(|_| StageError::InvalidProjection {
                                path: record.path.clone(),
                                detail: format!("material subset has negative face index {face}"),
                            })
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    material_subsets.push(MaterialSubsetRecord {
                        material_path: (!material_path.is_empty()).then_some(material_path),
                        face_indices,
                    });
                    subset_cursor = end;
                }
                if subset_cursor != record.subset_face_indices.len() {
                    return Err(StageError::InvalidProjection {
                        path: record.path,
                        detail: "unused material subset face indices".to_owned(),
                    });
                }
                Ok(MeshRecord {
                    prim: self.prim_for_path(&record.path)?,
                    points,
                    normals,
                    tangents,
                    texcoords,
                    face_vertex_counts: record.face_vertex_counts,
                    face_vertex_indices: record.face_vertex_indices,
                    display_colors: flat_to_colors(&record.display_color),
                    material_path: (!record.material_path.is_empty())
                        .then_some(record.material_path),
                    material_subsets,
                    left_handed: record.left_handed,
                    double_sided: record.double_sided,
                    prototype_only: record.prototype_only,
                    subdivision_scheme: record.subdivision_scheme,
                    crease_indices: record.crease_indices,
                    crease_lengths: record.crease_lengths,
                    crease_sharpnesses: record.crease_sharpnesses,
                    corner_indices: record.corner_indices,
                    corner_sharpnesses: record.corner_sharpnesses,
                })
            })
            .collect::<Result<Vec<_>, StageError>>()?;
        let point_instancers = raw
            .point_instancers
            .into_iter()
            .map(|record| {
                if record.transforms.len() % 16 != 0 {
                    return Err(StageError::InvalidProjection {
                        path: record.path,
                        detail: "point-instancer transforms are not matrix aligned".to_owned(),
                    });
                }
                let prototypes = record
                    .prototype_paths
                    .iter()
                    .map(|path| self.prim_for_path(path))
                    .collect::<Result<Vec<_>, _>>()?;
                let proto_indices = record
                    .proto_indices
                    .iter()
                    .map(|index| {
                        let index =
                            u32::try_from(*index).map_err(|_| StageError::InvalidProjection {
                                path: record.path.clone(),
                                detail: format!("negative prototype index {index}"),
                            })?;
                        if usize::try_from(index).unwrap_or(usize::MAX) >= prototypes.len() {
                            return Err(StageError::InvalidProjection {
                                path: record.path.clone(),
                                detail: format!("prototype index {index} is out of bounds"),
                            });
                        }
                        Ok(index)
                    })
                    .collect::<Result<Vec<_>, StageError>>()?;
                let transforms = record
                    .transforms
                    .chunks_exact(16)
                    .map(|matrix| {
                        matrix
                            .try_into()
                            .map_err(|_| StageError::InvalidProjection {
                                path: record.path.clone(),
                                detail: "point-instancer matrix conversion failed".to_owned(),
                            })
                    })
                    .collect::<Result<Vec<[f32; 16]>, _>>()?;
                if transforms.len() != proto_indices.len() {
                    return Err(StageError::InvalidProjection {
                        path: record.path,
                        detail: "point-instancer transform/index counts differ".to_owned(),
                    });
                }
                Ok(PointInstancerRecord {
                    prim: self.prim_for_path(&record.path)?,
                    prototypes,
                    proto_indices,
                    transforms,
                })
            })
            .collect::<Result<Vec<_>, StageError>>()?;
        let native_instances = raw
            .native_instances
            .into_iter()
            .map(|record| {
                Ok(NativeInstanceRecord {
                    prim: self.prim_for_path(&record.path)?,
                    prototype: self.prim_for_path(&record.prototype_path)?,
                })
            })
            .collect::<Result<Vec<_>, StageError>>()?;
        let cameras = raw
            .cameras
            .into_iter()
            .map(|record| {
                if record.clipping_planes.len() % 4 != 0 {
                    return Err(StageError::InvalidProjection {
                        path: record.path,
                        detail: "camera clipping-plane data is not float4-aligned".to_owned(),
                    });
                }
                let clipping_planes = record
                    .clipping_planes
                    .chunks_exact(4)
                    .map(|plane| [plane[0], plane[1], plane[2], plane[3]])
                    .collect();
                Ok(CameraRecord {
                    prim: self.prim_for_path(&record.path)?,
                    projection: record.projection,
                    focal_length: record.focal_length,
                    horizontal_aperture: record.horizontal_aperture,
                    vertical_aperture: record.vertical_aperture,
                    horizontal_aperture_offset: record.horizontal_aperture_offset,
                    vertical_aperture_offset: record.vertical_aperture_offset,
                    clipping: [record.clipping_near, record.clipping_far],
                    clipping_planes,
                    f_stop: record.f_stop,
                    focus_distance: record.focus_distance,
                    stereo_role: record.stereo_role,
                    shutter: [record.shutter_open, record.shutter_close],
                    exposure: record.exposure,
                    exposure_iso: record.exposure_iso,
                    exposure_time: record.exposure_time,
                    exposure_f_stop: record.exposure_f_stop,
                    exposure_responsivity: record.exposure_responsivity,
                })
            })
            .collect::<Result<Vec<_>, StageError>>()?;
        let lights = raw
            .lights
            .into_iter()
            .map(|record| {
                let world_matrix: [f32; 16] =
                    record.world_matrix.try_into().map_err(|values: Vec<f32>| {
                        StageError::InvalidProjection {
                            path: record.path.clone(),
                            detail: format!(
                                "light world matrix has {} values instead of 16",
                                values.len()
                            ),
                        }
                    })?;
                let color: [f32; 3] = record.color.try_into().map_err(|values: Vec<f32>| {
                    StageError::InvalidProjection {
                        path: record.path.clone(),
                        detail: format!("light color has {} values instead of three", values.len()),
                    }
                })?;
                let kind = match record.kind.as_str() {
                    "distant" => LightKind::Distant {
                        angle: record.angle,
                    },
                    "sphere" => LightKind::Sphere {
                        radius: record.radius,
                    },
                    "rect" => LightKind::Rect {
                        width: record.width,
                        height: record.height,
                    },
                    "disk" => LightKind::Disk {
                        radius: record.radius,
                    },
                    "cylinder" => LightKind::Cylinder {
                        radius: record.radius,
                        length: record.length,
                    },
                    "dome" => LightKind::Dome,
                    "dome_procedural" => LightKind::DomeProcedural,
                    other => {
                        return Err(StageError::InvalidProjection {
                            path: record.path,
                            detail: format!("unknown light kind {other}"),
                        });
                    }
                };
                Ok(LightRecord {
                    prim: self.prim_for_path(&record.path)?,
                    kind,
                    world_matrix,
                    color,
                    intensity: record.intensity,
                    exposure: record.exposure,
                    diffuse: record.diffuse,
                    specular: record.specular,
                    normalize: record.normalize,
                    shadow_enabled: record.shadow_enabled,
                    has_shaping: record.has_shaping,
                    cone_angle: record.cone_angle,
                    cone_softness: record.cone_softness,
                    // Raw Kelvin, surfaced only when the light enabled temperature.
                    // The blackbody tint is already folded into `color` by the bridge.
                    temperature: record
                        .enable_color_temperature
                        .then_some(record.color_temperature),
                    dome_texture_path: (!record.dome_texture_path.is_empty())
                        .then(|| record.dome_texture_path.clone()),
                    dome_texture_format: DomeTextureFormat::from_token(&record.dome_texture_format),
                    dome_pole_axis: PoleAxis::from_token(&record.dome_pole_axis),
                })
            })
            .collect::<Result<Vec<_>, StageError>>()?;
        let selected_camera = cameras.first().map(|camera| camera.prim);
        let selection_diagnostic = (cameras.len() > 1).then(|| {
            let selected_path = selected_camera
                .and_then(|prim| self.path_for(prim))
                .unwrap_or("<unknown>");
            format!(
                "camera policy first-in-stage-traversal selected {selected_path} from {} authored cameras",
                cameras.len()
            )
        });
        if let Some(diagnostic) = &selection_diagnostic {
            diagnostics.push(diagnostic.clone());
        }
        if let Some(camera) = cameras.first() {
            let differs_from = |value: f64, default: f64| (value - default).abs() > f64::EPSILON;
            let mut unsupported = Vec::new();
            if camera.horizontal_aperture_offset != 0.0 || camera.vertical_aperture_offset != 0.0 {
                unsupported.push("aperture offsets");
            }
            if !camera.clipping_planes.is_empty() {
                unsupported.push("clipping planes");
            }
            if camera.f_stop > 0.0 || camera.focus_distance > 0.0 {
                unsupported.push("depth of field");
            }
            if camera.stereo_role != "mono" {
                unsupported.push("stereo role");
            }
            if camera.shutter != [0.0, 0.0] {
                unsupported.push("shutter interval");
            }
            if camera.exposure != 0.0
                || differs_from(camera.exposure_iso, 100.0)
                || differs_from(camera.exposure_time, 1.0)
                || differs_from(camera.exposure_f_stop, 1.0)
                || differs_from(camera.exposure_responsivity, 1.0)
            {
                unsupported.push("physical exposure");
            }
            if !unsupported.is_empty() {
                diagnostics.push(format!(
                    "selected camera preserves authored {} for inspection; the synthetic viewport does not apply them",
                    unsupported.join(", ")
                ));
            }
            if !camera.clipping[0].is_finite()
                || !camera.clipping[1].is_finite()
                || camera.clipping[0] <= 0.0
                || camera.clipping[1] <= camera.clipping[0]
            {
                diagnostics.push(
                    "selected camera has an unsuitable authored clipping range; the viewport derives safe clipping while preserving authored values"
                        .to_owned(),
                );
            }
        }
        let camera_selection = CameraSelectionRecord {
            policy: CameraSelectionPolicy::FirstInStageTraversal,
            selected: selected_camera,
            authored_camera_count: cameras.len(),
            diagnostic: selection_diagnostic,
        };
        let mut materials = raw
            .materials
            .into_iter()
            .map(|record| {
                let base_color =
                    color(&record.base_color).ok_or_else(|| StageError::InvalidProjection {
                        path: record.path.clone(),
                        detail: "material base color is not RGB".to_owned(),
                    })?;
                let emissive =
                    color(&record.emissive).ok_or_else(|| StageError::InvalidProjection {
                        path: record.path.clone(),
                        detail: "material emissive color is not RGB".to_owned(),
                    })?;
                // Absent/short transmission_color means "unauthored" → white (no tint),
                // not an error: it is an optional Beer-Lambert glass tint.
                let transmission_color =
                    color(&record.transmission_color).unwrap_or([1.0, 1.0, 1.0]);
                let factors = SurfaceFactors {
                    base_color,
                    metallic: record.metallic,
                    roughness: record.roughness,
                    subsurface: record.subsurface,
                    emissive,
                    opacity: record.opacity,
                    opacity_threshold: record.opacity_threshold,
                };
                if ![
                    factors.metallic,
                    factors.roughness,
                    factors.subsurface,
                    factors.opacity,
                    factors.opacity_threshold,
                ]
                .into_iter()
                .chain(factors.base_color)
                .chain(factors.emissive)
                .chain(transmission_color)
                .all(f32::is_finite)
                {
                    return Err(material_projection_error(
                        &record.path,
                        "material factors contain a non-finite value",
                    ));
                }
                let surface = surface_ir(&record, factors)?;
                Ok(MaterialRecord {
                    path: record.path.clone(),
                    base_color,
                    metallic: record.metallic,
                    roughness: record.roughness,
                    subsurface: record.subsurface,
                    emissive,
                    transmission_color,
                    opacity: record.opacity,
                    opacity_threshold: record.opacity_threshold,
                    opacity_mode: surface.opacity_mode,
                    diffuse_texture: (!record.diffuse_texture.is_empty())
                        .then_some(record.diffuse_texture),
                    normal_texture: (!record.normal_texture.is_empty())
                        .then_some(record.normal_texture),
                    metallic_texture: (!record.metallic_texture.is_empty())
                        .then_some(record.metallic_texture),
                    roughness_texture: (!record.roughness_texture.is_empty())
                        .then_some(record.roughness_texture),
                    emissive_texture: (!record.emissive_texture.is_empty())
                        .then_some(record.emissive_texture),
                    occlusion_texture: (!record.occlusion_texture.is_empty())
                        .then_some(record.occlusion_texture),
                    metallic_texture_channel: record.metallic_texture_channel.min(3),
                    roughness_texture_channel: record.roughness_texture_channel.min(3),
                    occlusion_texture_channel: record.occlusion_texture_channel.min(3),
                    diffuse_uv_set: (!record.diffuse_uv_set.is_empty())
                        .then_some(record.diffuse_uv_set),
                    diffuse_uv_transform: affine_uv_transform(
                        &record.diffuse_uv_transform,
                        &record.path,
                    )?,
                    uses_diffuse_texture_alpha: record.uses_diffuse_texture_alpha,
                    diagnostics: record.diagnostics,
                    surface,
                    wgsl_source: None,
                })
            })
            .collect::<Result<Vec<_>, StageError>>()?;
        // Populate MaterialX WGSL source for StandardSurface materials.
        // `materialx_wgsl_cache` persists across projection calls: once a path is resolved
        // (success or known failure) the FFI + transpile is skipped entirely on re-projection,
        // preventing redundant work and repeated log spam for the same material.
        for mat in &mut materials {
            if mat.surface.model != SurfaceModel::MaterialXStandardSurface {
                continue;
            }
            // Cache hit — apply stored result without any FFI call.
            if let Some(cached) = self.materialx_wgsl_cache.get(&mat.path) {
                mat.wgsl_source = cached.clone();
                continue;
            }
            // Cache miss — run FFI + transpile, then store result.
            let result: Option<String> = if let Ok(handle) = self.handle_ref() {
                match ffi::material_surface_wgsl(handle, &mat.path) {
                    Err(e) => {
                        eprintln!(
                            "[idtx-usd] material_surface_wgsl FFI error for '{}': {} \
                             \u{2014} falling back to PBR uber-shader",
                            mat.path, e
                        );
                        None
                    }
                    Ok(raw_wgsl) => {
                        for diag in &raw_wgsl.diagnostics {
                            if !diag.is_empty() {
                                eprintln!(
                                    "[idtx-usd] MaterialX diagnostic for '{}': {}",
                                    mat.path, diag
                                );
                            }
                        }
                        if raw_wgsl.requires_phase2 && !raw_wgsl.wgsl_source.is_empty() {
                            // GlslShaderGenerator emitted GLSL — route to naga transpile.
                            #[cfg(feature = "materialx-naga")]
                            {
                                let t0 = std::time::Instant::now();
                                match crate::transpile_glsl_to_wgsl(&raw_wgsl.wgsl_source) {
                                    Ok(wgsl) => {
                                        let elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;
                                        let wgsl_lines = wgsl.lines().count();
                                        eprintln!(
                                            "[idtx-usd] MaterialX GLSL\u{2192}WGSL \
                                             for '{}' ({} lines, {:.1} ms, glsl_bytes={})",
                                            mat.path,
                                            wgsl_lines,
                                            elapsed_ms,
                                            raw_wgsl.wgsl_source.len()
                                        );
                                        Some(wgsl)
                                    }
                                    Err(e) => {
                                        eprintln!(
                                            "[idtx-usd] MaterialX naga transpile failed \
                                             for '{}': {} \u{2014} falling back to PBR uber-shader",
                                            mat.path, e
                                        );
                                        None
                                    }
                                }
                            }
                            #[cfg(not(feature = "materialx-naga"))]
                            {
                                eprintln!(
                                    "[idtx-usd] MaterialX requires naga transpile for '{}' \
                                     (requires_phase2=true) but 'materialx-naga' feature is \
                                     disabled \u{2014} falling back to PBR uber-shader",
                                    mat.path
                                );
                                None
                            }
                        } else {
                            eprintln!(
                                "[idtx-usd] MaterialX WGSL empty source for '{}' \
                                 \u{2014} falling back to PBR uber-shader",
                                mat.path
                            );
                            None
                        }
                    }
                }
            } else {
                None
            };
            self.materialx_wgsl_cache
                .insert(mat.path.clone(), result.clone());
            mat.wgsl_source = result;
        }
        let skeleton_topologies = raw
            .skeleton_topologies
            .into_iter()
            .map(|record| {
                let skeleton = self.prim_for_path(&record.skeleton_path)?;
                let bind_transforms = projection_matrices(
                    &record.bind_transforms,
                    &record.skeleton_path,
                    "skeleton bind transforms",
                )?;
                let rest_transforms = projection_matrices(
                    &record.rest_transforms,
                    &record.skeleton_path,
                    "skeleton rest transforms",
                )?;
                let joint_count = record.joint_paths.len();
                if joint_count == 0
                    || record.parent_indices.len() != joint_count
                    || bind_transforms.len() != joint_count
                    || rest_transforms.len() != joint_count
                {
                    return Err(StageError::InvalidProjection {
                        path: record.skeleton_path,
                        detail: "skeleton topology arrays have different or zero lengths"
                            .to_owned(),
                    });
                }
                let mut joints = Vec::with_capacity(joint_count);
                for (index, (((path, parent), bind_transform), rest_transform)) in record
                    .joint_paths
                    .into_iter()
                    .zip(record.parent_indices)
                    .zip(bind_transforms)
                    .zip(rest_transforms)
                    .enumerate()
                {
                    let parent = if parent < 0 {
                        None
                    } else {
                        let parent =
                            u32::try_from(parent).map_err(|_| StageError::InvalidProjection {
                                path: record.skeleton_path.clone(),
                                detail: format!("joint {index} has an invalid parent index"),
                            })?;
                        if usize::try_from(parent).unwrap_or(usize::MAX) >= index {
                            return Err(StageError::InvalidProjection {
                                path: record.skeleton_path,
                                detail: format!(
                                    "joint {index} parent {parent} is not an earlier joint"
                                ),
                            });
                        }
                        Some(parent)
                    };
                    joints.push(SkeletonJointRecord {
                        path,
                        parent,
                        bind_transform,
                        rest_transform,
                    });
                }
                Ok(SkeletonTopologyRecord { skeleton, joints })
            })
            .collect::<Result<Vec<_>, StageError>>()?;
        let joint_counts = skeleton_topologies
            .iter()
            .map(|topology| (topology.skeleton, topology.joints.len()))
            .collect::<HashMap<_, _>>();
        let skeleton_poses = raw
            .skeleton_poses
            .into_iter()
            .map(|record| {
                let skeleton = self.prim_for_path(&record.skeleton_path)?;
                let joint_count = joint_counts.get(&skeleton).copied().ok_or_else(|| {
                    StageError::InvalidProjection {
                        path: record.skeleton_path.clone(),
                        detail: "skeleton pose has no matching topology".to_owned(),
                    }
                })?;
                let joint_local_transforms = projection_matrices(
                    &record.joint_local_transforms,
                    &record.skeleton_path,
                    "joint local transforms",
                )?;
                let joint_world_transforms = projection_matrices(
                    &record.joint_world_transforms,
                    &record.skeleton_path,
                    "joint world transforms",
                )?;
                let skinning_transforms = projection_matrices(
                    &record.skinning_transforms,
                    &record.skeleton_path,
                    "skinning transforms",
                )?;
                if joint_local_transforms.len() != joint_count
                    || joint_world_transforms.len() != joint_count
                    || skinning_transforms.len() != joint_count
                {
                    return Err(StageError::InvalidProjection {
                        path: record.skeleton_path,
                        detail: "skeleton pose arrays do not match topology joint count".to_owned(),
                    });
                }
                Ok(SkeletonPoseRecord {
                    skeleton,
                    animation_source: (!record.animation_path.is_empty())
                        .then(|| self.prim_for_path(&record.animation_path))
                        .transpose()?,
                    skeleton_world_transform: projection_matrix(
                        record.skeleton_world_transform,
                        &record.skeleton_path,
                        "skeleton world transform",
                    )?,
                    joint_local_transforms,
                    joint_world_transforms,
                    skinning_transforms,
                })
            })
            .collect::<Result<Vec<_>, StageError>>()?;
        let mesh_point_counts = meshes
            .iter()
            .map(|mesh| (mesh.prim, mesh.points.len()))
            .collect::<HashMap<_, _>>();
        let skin_bindings = raw
            .skin_bindings
            .into_iter()
            .map(|record| {
                let prim = self.prim_for_path(&record.path)?;
                let skeleton = self.prim_for_path(&record.skeleton_path)?;
                let is_rest_pose = record.execution.as_str() == "rest-pose";
                let joint_count = if is_rest_pose {
                    // Rest-pose fallback has no joint influences, so joint_count
                    // is unused in validation. The skeleton topology may not be
                    // projected (e.g., missing restTransforms), so default to 0.
                    joint_counts.get(&skeleton).copied().unwrap_or(0)
                } else {
                    joint_counts.get(&skeleton).copied().ok_or_else(|| {
                        StageError::InvalidProjection {
                            path: record.path.clone(),
                            detail: "skin binding has no matching skeleton topology".to_owned(),
                        }
                    })?
                };
                let point_count = mesh_point_counts.get(&prim).copied().ok_or_else(|| {
                    StageError::InvalidProjection {
                        path: record.path.clone(),
                        detail: "skin binding has no matching projected mesh".to_owned(),
                    }
                })?;
                let method = match record.skinning_method.as_str() {
                    "classicLinear" => SkinningMethod::ClassicLinear,
                    "dualQuaternion" => SkinningMethod::DualQuaternion,
                    _ => SkinningMethod::Unsupported,
                };
                let execution = match record.execution.as_str() {
                    "gpu-classic-linear" => SkinningExecution::GpuClassicLinear,
                    "cpu-openusd" => SkinningExecution::CpuOpenUsd,
                    "rest-pose" => SkinningExecution::RestPose,
                    other => {
                        return Err(StageError::InvalidProjection {
                            path: record.path,
                            detail: format!("unknown skinning execution mode {other}"),
                        });
                    }
                };
                if record.joint_indices.len() != record.joint_weights.len()
                    || record.joint_indices.len() % 8 != 0
                {
                    return Err(StageError::InvalidProjection {
                        path: record.path,
                        detail: "packed skin influence arrays are not 8-influence aligned"
                            .to_owned(),
                    });
                }
                let influences = record
                    .joint_indices
                    .chunks_exact(8)
                    .zip(record.joint_weights.chunks_exact(8))
                    .map(|(joints, weights)| {
                        let joints: [u16; 8] = joints.try_into().expect("chunk of 8");
                        let weights: [f32; 8] = weights.try_into().expect("chunk of 8");
                        let sum = weights.iter().sum::<f32>();
                        if joints.iter().zip(weights).any(|(joint, weight)| {
                            usize::from(*joint) >= joint_count
                                || !weight.is_finite()
                                || weight < 0.0
                        }) || (sum - 1.0).abs() > 1.0e-4
                        {
                            return Err(StageError::InvalidProjection {
                                path: record.path.clone(),
                                detail: "skin influences are not finite, normalized, and in range"
                                    .to_owned(),
                            });
                        }
                        Ok(VertexInfluences { joints, weights })
                    })
                    .collect::<Result<Vec<_>, StageError>>()?;
                let cpu_skinned_points = projection_points(
                    &record.cpu_skinned_points,
                    &record.path,
                    "CPU-skinned points",
                )?;
                let skinned_bounds = match (
                    record.skinned_bounds_minimum.as_slice(),
                    record.skinned_bounds_maximum.as_slice(),
                ) {
                    ([], []) => None,
                    ([min_x, min_y, min_z], [max_x, max_y, max_z])
                        if [min_x, min_y, min_z, max_x, max_y, max_z]
                            .into_iter()
                            .all(|value| value.is_finite())
                            && min_x <= max_x
                            && min_y <= max_y
                            && min_z <= max_z =>
                    {
                        Some(SkinnedBoundsRecord {
                            minimum: [*min_x, *min_y, *min_z],
                            maximum: [*max_x, *max_y, *max_z],
                        })
                    }
                    _ => {
                        return Err(StageError::InvalidProjection {
                            path: record.path,
                            detail: "skinned bounds are malformed or non-finite".to_owned(),
                        });
                    }
                };
                let shape_valid = match execution {
                    SkinningExecution::GpuClassicLinear => {
                        method == SkinningMethod::ClassicLinear
                            && influences.len() == point_count
                            && cpu_skinned_points.is_empty()
                            && (point_count == 0 || skinned_bounds.is_some())
                    }
                    SkinningExecution::CpuOpenUsd => {
                        influences.is_empty()
                            && cpu_skinned_points.len() == point_count
                            && (point_count == 0 || skinned_bounds.is_some())
                    }
                    SkinningExecution::RestPose => {
                        influences.is_empty()
                            && cpu_skinned_points.is_empty()
                            && skinned_bounds.is_none()
                    }
                };
                if !shape_valid {
                    return Err(StageError::InvalidProjection {
                        path: record.path,
                        detail: "skinning execution payload does not match its declared mode"
                            .to_owned(),
                    });
                }
                Ok(SkinBindingRecord {
                    prim,
                    skeleton,
                    method,
                    execution,
                    geom_bind_transform: projection_matrix(
                        record.geom_bind_transform,
                        &record.path,
                        "geometry bind transform",
                    )?,
                    influences,
                    cpu_skinned_points,
                    skinned_bounds,
                })
            })
            .collect::<Result<Vec<_>, StageError>>()?;
        let skeleton_dependencies = raw
            .skeleton_dependencies
            .into_iter()
            .map(|record| {
                Ok(SkeletonDependencyRecord {
                    skeleton: self.prim_for_path(&record.skeleton_path)?,
                    animation_source: (!record.animation_path.is_empty())
                        .then(|| self.prim_for_path(&record.animation_path))
                        .transpose()?,
                    skinned_prims: record
                        .skinned_paths
                        .iter()
                        .map(|path| self.prim_for_path(path))
                        .collect::<Result<Vec<_>, _>>()?,
                })
            })
            .collect::<Result<Vec<_>, StageError>>()?;
        let record_count = transforms.len()
            + visibility.len()
            + bounds.len()
            + cubes.len()
            + spheres.len()
            + extended_gprims.len()
            + planes.len()
            + meshes.len()
            + point_instancers.len()
            + native_instances.len()
            + cameras.len()
            + lights.len()
            + skeleton_topologies.len()
            + skeleton_poses.len()
            + skin_bindings.len()
            + skeleton_dependencies.len()
            + materials.len();
        let projected_bytes = transforms.len() * std::mem::size_of::<TransformRecord>()
            + visibility.len() * std::mem::size_of::<VisibilityRecord>()
            + bounds.len() * std::mem::size_of::<BoundsRecord>()
            + cubes.len() * std::mem::size_of::<CubeRecord>()
            + spheres.len() * std::mem::size_of::<SphereRecord>()
            + extended_gprims.len() * std::mem::size_of::<ExtendedGprimRecord>()
            + planes.len() * std::mem::size_of::<PlaneRecord>()
            + cameras
                .iter()
                .map(|camera| {
                    std::mem::size_of::<CameraRecord>()
                        + camera.clipping_planes.len() * std::mem::size_of::<[f64; 4]>()
                        + camera.projection.len()
                        + camera.stereo_role.len()
                })
                .sum::<usize>()
            + lights.len() * std::mem::size_of::<LightRecord>()
            + skeleton_topologies
                .iter()
                .map(|topology| {
                    std::mem::size_of::<SkeletonTopologyRecord>()
                        + topology
                            .joints
                            .iter()
                            .map(|joint| {
                                std::mem::size_of::<SkeletonJointRecord>() + joint.path.len()
                            })
                            .sum::<usize>()
                })
                .sum::<usize>()
            + skeleton_poses
                .iter()
                .map(|pose| {
                    std::mem::size_of::<SkeletonPoseRecord>()
                        + (pose.joint_local_transforms.len()
                            + pose.joint_world_transforms.len()
                            + pose.skinning_transforms.len())
                            * std::mem::size_of::<[f32; 16]>()
                })
                .sum::<usize>()
            + skin_bindings
                .iter()
                .map(|binding| {
                    std::mem::size_of::<SkinBindingRecord>()
                        + binding.influences.len() * std::mem::size_of::<VertexInfluences>()
                        + binding.cpu_skinned_points.len() * std::mem::size_of::<[f32; 3]>()
                })
                .sum::<usize>()
            + skeleton_dependencies
                .iter()
                .map(|dependency| {
                    std::mem::size_of::<SkeletonDependencyRecord>()
                        + dependency.skinned_prims.len() * std::mem::size_of::<PrimId>()
                })
                .sum::<usize>()
            + meshes
                .iter()
                .map(|mesh| {
                    mesh.points.len() * std::mem::size_of::<[f32; 3]>()
                        + mesh.texcoords.len() * std::mem::size_of::<[f32; 2]>()
                        + mesh.face_vertex_counts.len() * std::mem::size_of::<i32>()
                        + mesh.face_vertex_indices.len() * std::mem::size_of::<i32>()
                        + mesh
                            .material_subsets
                            .iter()
                            .map(|subset| {
                                subset.face_indices.len() * std::mem::size_of::<u32>()
                                    + subset.material_path.as_deref().map_or(0, str::len)
                            })
                            .sum::<usize>()
                })
                .sum::<usize>()
            + point_instancers
                .iter()
                .map(|instancer| {
                    instancer.transforms.len() * std::mem::size_of::<[f32; 16]>()
                        + instancer.proto_indices.len() * std::mem::size_of::<u32>()
                        + instancer.prototypes.len() * std::mem::size_of::<PrimId>()
                })
                .sum::<usize>()
            + native_instances.len() * std::mem::size_of::<NativeInstanceRecord>()
            + materials.iter().map(logical_material_bytes).sum::<usize>();
        self.metrics.viewport.calls += 1;
        self.metrics.viewport.records += u64::try_from(record_count).unwrap_or(u64::MAX);
        self.metrics.viewport.bytes += u64::try_from(projected_bytes).unwrap_or(u64::MAX);
        let curves = raw.curves.into_iter().map(|record| {
            let curve = crate::curve::BasisCurvesRecord {
                prim: self.prim_for_path(&record.path)?,
                points: projection_points(&record.points, &record.path, "curve points")?,
                counts: record.counts, curve_type: record.curve_type, basis: record.basis, wrap: record.wrap,
                world_matrix: record.world_matrix.try_into().map_err(|_| StageError::InvalidProjection { path: record.path.clone(), detail: "curve transform needs 16 values".into() })?,
                metres_per_unit: record.metres_per_unit, z_up: record.z_up,
                tolerance: record.tolerance, prototype_only: record.prototype_only,
                eligible: record.eligible, start_group: record.start_group, end_group: record.end_group,
            };
            if let Err(error) = curve.validate() { diagnostics.push(format!("{}: {error}", record.path)); }
            Ok(curve)
        }).collect::<Result<Vec<_>, StageError>>()?;
        self.metrics.viewport.records += curves.len() as u64;
        self.metrics.viewport.bytes += curves.iter().map(|c| std::mem::size_of_val(c)
            + c.points.len()*12 + c.counts.len()*4 + c.basis.len() + c.curve_type.len() + c.wrap.len()
            + c.start_group.len() + c.end_group.len()).sum::<usize>() as u64;
        self.metrics.viewport.elapsed += started.elapsed();
        Ok(ViewportProjection {
            schema_version: raw.schema_version,
            generation: self.generation,
            revision: self.revision,
            time_code,
            diagnostics,
            transforms,
            visibility,
            bounds,
            cubes,
            spheres,
            extended_gprims,
            planes,
            meshes,
            curves,
            point_instancers,
            native_instances,
            materials,
            cameras,
            lights,
            skeleton_topologies,
            skeleton_poses,
            skin_bindings,
            skeleton_dependencies,
            camera_selection,
        })
    }
}
