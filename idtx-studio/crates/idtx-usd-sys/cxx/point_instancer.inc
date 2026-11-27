// Native PointInstancer value projection and failure-atomic, bounded authoring.
// Included inside namespace idtx after StageHandle::Impl and the viewport helpers.
namespace {
constexpr std::size_t kMaxPointInstances = 4096;
constexpr std::size_t kMaxPointPrototypes = 256;

PointInstancerRecordRaw point_instancer_record(
    const UsdStageRefPtr& stage, const UsdGeomPointInstancer& pi, UsdTimeCode time,
    bool mask_only, rust::Vec<rust::String>* diagnostics) {
    PointInstancerRecordRaw out;
    out.path = rust::String(pi.GetPath().GetString());
    for (const auto& path : get_instancer_prototype_paths(stage, pi))
        out.prototype_paths.push_back(rust::String(path.GetString()));
    VtIntArray indices;
    VtInt64Array ids;
    pi.GetProtoIndicesAttr().Get(&indices, time);
    pi.GetIdsAttr().Get(&ids, time);
    const auto mask = pi.ComputeMaskAtTime(time);
    VtFloatArray opacities;
    const auto opacity = UsdGeomPrimvarsAPI(pi.GetPrim()).GetPrimvar(TfToken("displayOpacity"));
    if (opacity && opacity.GetAttr().HasAuthoredValueOpinion()) {
        const auto interpolation = opacity.GetInterpolation();
        const bool valid = opacity.ComputeFlattened(&opacities, time) &&
            ((interpolation == UsdGeomTokens->constant && opacities.size() == 1) ||
             (interpolation == UsdGeomTokens->vertex && opacities.size() == indices.size())) &&
            std::all_of(opacities.begin(), opacities.end(), [](float a) { return std::isfinite(a) && a >= 0 && a <= 1; });
        if (!valid) {
            diagnostics->push_back(rust::String(pi.GetPath().GetString() +
                " PointInstancer: displayOpacity requires finite [0,1] constant or vertex values"));
            opacities = VtFloatArray(1, 0.0f);
        }
    }
    VtArray<GfMatrix4d> transforms;
    if ((!ids.empty() && ids.size() != indices.size()) ||
        (!mask.empty() && mask.size() != indices.size()) ||
        (!mask_only && !indices.empty() &&
         (!pi.ComputeInstanceTransformsAtTime(&transforms, time, time,
             UsdGeomPointInstancer::IncludeProtoXform, UsdGeomPointInstancer::IgnoreMask) ||
          transforms.size() != indices.size()))) {
        diagnostics->push_back(rust::String(pi.GetPath().GetString() +
            " PointInstancer: invalid ids/mask/placement arrays"));
        return out;
    }
    if (indices.size() <= kMaxPointInstances) {
        out.ids.reserve(indices.size());
        out.proto_indices.reserve(indices.size());
        if (!mask_only) out.transforms.reserve(indices.size() * 16);
    }
    for (std::size_t i = 0; i < indices.size(); ++i) {
        // Keep the pre-existing compact projection for large static DCC pools.
        // Only the bounded runtime case retains hidden matrices/slots across FFI.
        if (indices.size() > kMaxPointInstances && !mask.empty() && !mask[i]) continue;
        out.ids.push_back(ids.empty() ? static_cast<int64_t>(i) : ids[i]);
        if (!opacities.empty()) out.opacities.push_back(opacities.size() == 1 ? opacities[0] : opacities[i]);
        out.proto_indices.push_back(indices[i]);
        if (indices.size() <= kMaxPointInstances && !mask.empty()) out.mask.push_back(mask[i] ? 1 : 0);
        if (!mask_only)
            for (int r = 0; r < 4; ++r)
                for (int c = 0; c < 4; ++c)
                    out.transforms.push_back(static_cast<float>(transforms[i][r][c]));
    }
    return out;
}

// Validates precisely the arrays owned by this transaction. Native prototype
// relationships / inactiveIds are never reauthored by a pulse.
std::string validate_point_arrays(const PointInstancerUpdateRaw& a, std::size_t prototypes) {
    const auto n = a.ids.size();
    if (n > kMaxPointInstances || prototypes > kMaxPointPrototypes)
        return "PointInstancer exceeds 4096 instances / 256 prototypes";
    if (a.positions.size() != n * 3 || a.proto_indices.size() != n ||
        (!a.orientations_xyzw.empty() && a.orientations_xyzw.size() != n * 4) ||
        (!a.scales.empty() && a.scales.size() != n * 3))
        return "PointInstancer array lengths differ";
    std::unordered_set<int64_t> ids;
    for (std::size_t i = 0; i < n; ++i) {
        if (!ids.insert(a.ids[i]).second) return "PointInstancer ids must be unique";
        if (a.proto_indices[i] < 0 || static_cast<std::size_t>(a.proto_indices[i]) >= prototypes)
            return "PointInstancer prototype index out of range at entry " + std::to_string(i);
        for (std::size_t j = 0; j < 3; ++j)
            if (!std::isfinite(a.positions[i*3+j]) ||
                (!a.scales.empty() && !std::isfinite(a.scales[i*3+j])))
                return "PointInstancer non-finite position/scale at entry " + std::to_string(i);
        if (!a.orientations_xyzw.empty()) {
            double length2 = 0;
            for (std::size_t j = 0; j < 4; ++j) {
                const double q = a.orientations_xyzw[i*4+j];
                length2 += q*q;
            }
            if (!std::isfinite(length2) || std::abs(length2 - 1.0) > 0.002)
                return "PointInstancer orientation must be a unit XYZW quaternion at entry " + std::to_string(i);
        }
    }
    if (a.invisible_ids.size() > n) return "PointInstancer invisibleIds exceeds instance count";
    std::unordered_set<int64_t> hidden;
    for (auto id : a.invisible_ids)
        if (!ids.count(id) || !hidden.insert(id).second)
            return "PointInstancer invisibleIds must be unique existing ids";
    return {};
}

PointInstancerUpdateRaw read_point_arrays(const UsdGeomPointInstancer& pi, UsdTimeCode time,
                                        bool bounded, bool* truncated, uint64_t* count) {
    PointInstancerUpdateRaw a;
    VtVec3fArray p, s;
    VtQuathArray q;
    VtQuatfArray qf;
    VtIntArray indices;
    VtInt64Array ids, invisible;
    pi.GetPositionsAttr().Get(&p, time);
    const bool float_rotations = pi.UsesOrientationsf();
    if (float_rotations) pi.GetOrientationsfAttr().Get(&qf, time);
    else pi.GetOrientationsAttr().Get(&q, time);
    pi.GetScalesAttr().Get(&s, time);
    pi.GetProtoIndicesAttr().Get(&indices, time);
    pi.GetIdsAttr().Get(&ids, time);
    pi.GetInvisibleIdsAttr().Get(&invisible, time);
    *count = indices.size();
    const auto limit = bounded ? kMaxPointInstances : std::numeric_limits<std::size_t>::max();
    *truncated = std::max({p.size(), q.size(), qf.size(), s.size(), indices.size(), ids.size(), invisible.size()}) > limit;
    for (std::size_t i = 0; i < std::min(p.size(), limit); ++i)
        for (int j = 0; j < 3; ++j) a.positions.push_back(p[i][j]);
    for (std::size_t i = 0; i < std::min(s.size(), limit); ++i)
        for (int j = 0; j < 3; ++j) a.scales.push_back(s[i][j]);
    for (std::size_t i = 0; i < std::min(q.size(), limit); ++i) {
        for (int j = 0; j < 3; ++j) a.orientations_xyzw.push_back(float(q[i].GetImaginary()[j]));
        a.orientations_xyzw.push_back(float(q[i].GetReal()));
    }
    for (std::size_t i = 0; i < std::min(qf.size(), limit); ++i) {
        for (int j = 0; j < 3; ++j) a.orientations_xyzw.push_back(qf[i].GetImaginary()[j]);
        a.orientations_xyzw.push_back(qf[i].GetReal());
    }
    for (std::size_t i = 0; i < std::min(indices.size(), limit); ++i) a.proto_indices.push_back(indices[i]);
    for (std::size_t i = 0; i < std::min(ids.empty() ? indices.size() : ids.size(), limit); ++i)
        a.ids.push_back(ids.empty() ? static_cast<int64_t>(i) : ids[i]);
    for (std::size_t i = 0; i < std::min(invisible.size(), limit); ++i) a.invisible_ids.push_back(invisible[i]);
    return a;
}
} // namespace

PointInstancerSamplesRaw sample_point_instancers(
    const StageHandle& handle, rust::Slice<const rust::String> paths,
    double time_code, bool use_default_time, bool mask_only) {
    return handle.impl_->access([&] {
        PointInstancerSamplesRaw out;
        const auto time = use_default_time ? UsdTimeCode::Default() : UsdTimeCode(time_code);
        for (const auto& path : paths) {
            const UsdGeomPointInstancer pi(handle.impl_->stage->GetPrimAtPath(SdfPath(to_std(path))));
            if (!pi) throw std::runtime_error("PointInstancer target no longer exists");
            out.records.push_back(point_instancer_record(handle.impl_->stage, pi, time, mask_only, &out.diagnostics));
        }
        return out;
    });
}

PointInstancerInspectionRaw inspect_point_instancer(
    const StageHandle& handle, rust::Str path, double time_code, bool use_default_time) {
    return handle.impl_->access([&] {
        const UsdGeomPointInstancer pi(handle.impl_->stage->GetPrimAtPath(SdfPath(to_std(path))));
        if (!pi) throw std::runtime_error("target is not a PointInstancer");
        const auto time = use_default_time ? UsdTimeCode::Default() : UsdTimeCode(time_code);
        PointInstancerInspectionRaw out;
        out.arrays = read_point_arrays(pi, time, true, &out.truncated, &out.count);
        const auto prototypes = get_instancer_prototype_paths(handle.impl_->stage, pi);
        for (const auto& target : prototypes) {
            if (out.prototype_paths.size() == kMaxPointPrototypes) { out.truncated = true; break; }
            out.prototype_paths.push_back(rust::String(target.GetString()));
            const auto prim = handle.impl_->stage->GetPrimAtPath(target);
            if (!prim || !prim.IsActive() || !prim.IsLoaded())
                out.diagnostics.push_back(rust::String("Unresolved/unloaded prototype: " + target.GetString()));
        }
        const auto error = validate_point_arrays(out.arrays, prototypes.size());
        if (!error.empty()) out.diagnostics.push_back(rust::String(error));
        if (out.truncated) out.diagnostics.push_back(rust::String("Inspection truncated: exceeds bounded runtime limits"));
        const auto mask = pi.ComputeMaskAtTime(time);
        for (std::size_t i = 0; i < std::min(mask.size(), kMaxPointInstances); ++i)
            out.mask.push_back(mask[i] ? 1 : 0);
        SdfInt64ListOp inactive;
        if (pi.GetPrim().GetMetadata(UsdGeomTokens->inactiveIds, &inactive)) {
            const auto applied = inactive.GetAppliedItems();
            for (std::size_t i = 0; i < std::min(applied.size(), kMaxPointInstances); ++i)
                out.inactive_ids.push_back(applied[i]);
        }
        return out;
    });
}

#ifdef IDTX_AUTHORING
void update_point_instancer(StageHandle& handle, rust::Str path,
                           const PointInstancerUpdateRaw& a, bool mask_only, bool session_layer) {
    handle.impl_->access([&] {
        const SdfPath prim_path(to_std(path));
        const auto& stage = handle.impl_->stage;
        const auto prim = stage->GetPrimAtPath(prim_path);
        const UsdGeomPointInstancer pi(prim);
        const auto layer = session_layer ? stage->GetSessionLayer() : stage->GetRootLayer();
        if (!pi || !prim.IsActive() || !prim.IsLoaded() || prim.IsInstanceProxy() || prim.IsInPrototype())
            throw std::runtime_error("target must be an existing loaded editable PointInstancer");
        if (!layer || !layer->PermissionToEdit()) throw std::runtime_error("PointInstancer layer is read-only");
        const auto prototypes = get_instancer_prototype_paths(stage, pi);
        if (prototypes.size() > kMaxPointPrototypes) throw std::runtime_error("PointInstancer exceeds prototype limit");
        for (const auto& target : prototypes) {
            const auto proto = stage->GetPrimAtPath(target);
            if (!proto || !proto.IsActive() || !proto.IsLoaded())
                throw std::runtime_error("PointInstancer prototype is unresolved/unloaded");
        }
        // Capture native schema attributes before the Sdf-only change block.
        const bool float_rotations = pi.UsesOrientationsf();
        const std::array<UsdAttribute, 7> attrs = {pi.GetPositionsAttr(), pi.GetOrientationsAttr(),
            pi.GetScalesAttr(), pi.GetProtoIndicesAttr(), pi.GetIdsAttr(), pi.GetInvisibleIdsAttr(), pi.GetOrientationsfAttr()};
        const std::array<SdfValueTypeName, 7> types = {SdfValueTypeNames->Point3fArray, SdfValueTypeNames->QuathArray,
            SdfValueTypeNames->Float3Array, SdfValueTypeNames->IntArray, SdfValueTypeNames->Int64Array, SdfValueTypeNames->Int64Array, SdfValueTypeNames->QuatfArray};
        for (std::size_t i = 0; i < attrs.size(); ++i)
            if (!attrs[i] || attrs[i].GetTypeName() != types[i])
                throw std::runtime_error("PointInstancer attribute has incompatible schema type");
        std::array<VtValue, 7> values;
        const auto begin = mask_only ? 5u : 0u;
        const auto end = !mask_only && float_rotations ? 7u : 6u;
        if (mask_only) {
            if (!a.positions.empty() || !a.orientations_xyzw.empty() || !a.scales.empty() ||
                !a.proto_indices.empty() || !a.ids.empty() || a.invisible_ids.size() > kMaxPointInstances)
                throw std::runtime_error("mask transaction accepts only bounded invisibleIds");
            VtIntArray indices;
            VtInt64Array ids;
            VtVec3fArray positions;
            VtVec3fArray scales;
            VtQuathArray orientations;
            VtQuatfArray orientationsf;
            pi.GetProtoIndicesAttr().Get(&indices);
            pi.GetIdsAttr().Get(&ids);
            pi.GetPositionsAttr().Get(&positions);
            pi.GetScalesAttr().Get(&scales);
            if (float_rotations) pi.GetOrientationsfAttr().Get(&orientationsf);
            else pi.GetOrientationsAttr().Get(&orientations);
            if (indices.size() > kMaxPointInstances || positions.size() != indices.size() ||
                (!ids.empty() && ids.size() != indices.size()) ||
                (!scales.empty() && scales.size() != indices.size()) ||
                (!orientations.empty() && orientations.size() != indices.size()) ||
                (!orientationsf.empty() && orientationsf.size() != indices.size()))
                throw std::runtime_error("PointInstancer invalid existing array lengths / size limit");
            std::unordered_set<int64_t> stable;
            for (std::size_t i = 0; i < indices.size(); ++i) {
                if (indices[i] < 0 || static_cast<std::size_t>(indices[i]) >= prototypes.size() ||
                    !stable.insert(ids.empty() ? static_cast<int64_t>(i) : ids[i]).second)
                    throw std::runtime_error("PointInstancer invalid existing ids / prototype indices");
                for (int j = 0; j < 3; ++j)
                    if (!std::isfinite(positions[i][j]) || (!scales.empty() && !std::isfinite(scales[i][j])))
                        throw std::runtime_error("PointInstancer invalid existing position / scale");
                if (!orientations.empty()) {
                    const double norm = double(orientations[i].GetReal()) * double(orientations[i].GetReal()) +
                        GfVec3d(orientations[i].GetImaginary()).GetLengthSq();
                    if (!std::isfinite(norm) || std::abs(norm - 1.0) > 0.002)
                        throw std::runtime_error("PointInstancer invalid existing orientation");
                }
                if (!orientationsf.empty()) {
                    const double norm = double(orientationsf[i].GetReal()) * double(orientationsf[i].GetReal()) +
                        GfVec3d(orientationsf[i].GetImaginary()).GetLengthSq();
                    if (!std::isfinite(norm) || std::abs(norm - 1.0) > 0.002)
                        throw std::runtime_error("PointInstancer invalid existing float orientation");
                }
            }
            std::unordered_set<int64_t> hidden;
            for (auto id : a.invisible_ids)
                if (!stable.count(id) || !hidden.insert(id).second)
                    throw std::runtime_error("invisibleIds must be unique existing stable ids");
        } else {
            const auto error = validate_point_arrays(a, prototypes.size());
            if (!error.empty()) throw std::runtime_error(error);
            const auto n = a.ids.size();
            VtVec3fArray p(n), s(n, GfVec3f(1));
            VtQuathArray q(n, GfQuath(1));
            VtQuatfArray qf(float_rotations ? n : 0, GfQuatf(1));
            VtIntArray indices(n);
            VtInt64Array ids(n);
            for (std::size_t i = 0; i < n; ++i) {
                for (int j = 0; j < 3; ++j) {
                    p[i][j] = a.positions[i*3+j];
                    if (!a.scales.empty()) s[i][j] = a.scales[i*3+j];
                }
                if (!a.orientations_xyzw.empty()) {
                    const auto rotation = GfQuatf(a.orientations_xyzw[i*4+3], GfVec3f(
                        a.orientations_xyzw[i*4], a.orientations_xyzw[i*4+1], a.orientations_xyzw[i*4+2])).GetNormalized();
                    q[i] = GfQuath(rotation);
                    if (float_rotations) qf[i] = rotation;
                }
                indices[i] = a.proto_indices[i]; ids[i] = a.ids[i];
            }
            values[0] = VtValue(p); values[1] = VtValue(q); values[2] = VtValue(s);
            values[3] = VtValue(indices); values[4] = VtValue(ids);
            // USD prefers authored orientationsf. Update it when present, and
            // keep the half array aligned too (zero instances must not fall back
            // to an old, nonempty half array). Both belong to this transaction.
            if (float_rotations) values[6] = VtValue(qf);
        }
        VtInt64Array invisible(a.invisible_ids.size());
        std::copy(a.invisible_ids.begin(), a.invisible_ids.end(), invisible.begin());
        values[5] = VtValue(invisible);

        // Snapshot only six/seven (or one) small, COW fields, never the session layer
        // or prototype meshes. Repeated identical masks author nothing.
        std::array<SdfAttributeSpecHandle, 7> specs;
        std::array<VtValue, 7> defaults, samples;
        bool identical = true;
        bool animation_changed = false;
        for (std::size_t i = begin; i < end; ++i) {
            // Reconcile tracks once when rolling defaults shadow time samples;
            // stale tracks must not force full extraction on later play frames.
            animation_changed |= attrs[i].ValueMightBeTimeVarying();
            specs[i] = layer->GetAttributeAtPath(attrs[i].GetPath());
            defaults[i] = layer->GetField(attrs[i].GetPath(), SdfFieldKeys->Default);
            samples[i] = layer->GetField(attrs[i].GetPath(), SdfFieldKeys->TimeSamples);
            identical &= specs[i] && defaults[i] == values[i] && samples[i].IsEmpty();
        }
        if (identical) return;
        std::vector<SdfPath> new_ancestors;
        for (auto p = prim_path; p.IsPrimPath() && !layer->GetPrimAtPath(p); p = p.GetParentPath())
            new_ancestors.push_back(p);
        const auto checkpoint = handle.impl_->observer->instancer_checkpoint();
        bool ok = true;
        {
            SdfChangeBlock block;
            const auto owner = SdfCreatePrimInLayer(layer, prim_path);
            ok = bool(owner);
            for (std::size_t i = begin; ok && i < end; ++i) {
                auto spec = specs[i] ? specs[i] : SdfAttributeSpec::New(owner, attrs[i].GetName().GetString(), types[i], SdfVariabilityVarying, false);
                ok = spec && spec->SetDefaultValue(values[i]);
                if (ok) layer->EraseField(attrs[i].GetPath(), SdfFieldKeys->TimeSamples);
#ifdef IDTX_TEST_SUPPORT
                if (handle.impl_->instancer_failure_after_writes == i - begin + 1) {
                    handle.impl_->instancer_failure_after_writes = 0;
                    ok = false;
                }
#endif
            }
            if (!ok) {
                for (std::size_t i = begin; i < end; ++i) {
                    const auto p = attrs[i].GetPath();
                    if (specs[i]) {
                        if (defaults[i].IsEmpty()) layer->EraseField(p, SdfFieldKeys->Default);
                        else layer->SetField(p, SdfFieldKeys->Default, defaults[i]);
                        if (samples[i].IsEmpty()) layer->EraseField(p, SdfFieldKeys->TimeSamples);
                        else layer->SetField(p, SdfFieldKeys->TimeSamples, samples[i]);
                    } else if (const auto added = layer->GetPropertyAtPath(p)) owner->RemoveProperty(added);
                }
                for (const auto& p : new_ancestors)
                    if (const auto spec = layer->GetPrimAtPath(p)) layer->ScheduleRemoveIfInert(spec.GetSpec());
            }
        }
        // The block exits normally before exceptions cross cxx (also on MSVC).
        if (!ok) {
            handle.impl_->observer->instancer_commit(checkpoint, prim_path, mask_only, false);
            throw std::runtime_error("PointInstancer transaction rolled back");
        }
        handle.impl_->observer->instancer_commit(checkpoint, prim_path, mask_only, true, animation_changed);
    });
}
#endif

rust::String session_layer_text(const StageHandle& handle) {
    return handle.impl_->access([&] {
        std::string text;
        if (!handle.impl_->stage->GetSessionLayer()->ExportToString(&text))
            throw std::runtime_error("could not inspect session layer");
        return rust::String(text);
    });
}

#ifdef IDTX_TEST_SUPPORT
void conformance_fail_instancer_after(StageHandle& handle, uint32_t writes) {
    handle.impl_->access([&] { handle.impl_->instancer_failure_after_writes = writes; });
}
#endif
