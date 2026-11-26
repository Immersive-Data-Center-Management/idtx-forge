// Included by usd_shim.cpp: schema-free declarations and isolated native binding.
namespace {
template <typename T>
T animation_value(const UsdPrim& prim, const std::string& name,
                  const SdfValueTypeName& type, T fallback, bool required = false) {
    const auto attr = prim.GetAttribute(TfToken(name));
    if (!attr) {
        if (required) throw std::runtime_error("missing " + name);
        return fallback;
    }
    if (attr.GetTypeName() != type || attr.GetVariability() != SdfVariabilityUniform ||
        attr.GetNumTimeSamples() != 0) {
        throw std::runtime_error(name + " must be a uniform " + type.GetAsToken().GetString());
    }
    T value;
    if (!attr.Get(&value)) throw std::runtime_error("unresolved " + name);
    return value;
}

SdfPathVector animation_targets(const UsdPrim& prim, const std::string& name) {
    const auto rel = prim.GetRelationship(TfToken(name));
    SdfPathVector paths;
    if (rel && !rel.GetForwardedTargets(&paths))
        throw std::runtime_error("unresolved relationship " + name);
    for (const auto& path : paths) {
        const auto target = prim.GetStage()->GetPrimAtPath(path);
        if (!path.IsPrimPath() || !target || !target.IsActive() || !target.IsLoaded())
            throw std::runtime_error(name + " has missing/unloaded target " + path.GetString());
        if (target.IsInstanceProxy() || target.IsInPrototype())
            throw std::runtime_error(name + " requires an editable rig occurrence, not an instance proxy");
    }
    return paths;
}

bool animation_contains(const std::vector<std::string>& roots, const SdfPath& path) {
    return std::any_of(roots.begin(), roots.end(), [&](const auto& root) {
        return path.HasPrefix(SdfPath(root));
    });
}

void sync_animation_stage(const UsdStageRefPtr& original, const UsdStageRefPtr& stage) {
    stage->SetLoadRules(original->GetLoadRules());
    stage->SetInterpolationType(original->GetInterpolationType());
    if (stage->GetPopulationMask() != original->GetPopulationMask())
        stage->SetPopulationMask(original->GetPopulationMask());
    const auto desired = original->GetMutedLayers();
    std::vector<std::string> unmute;
    for (const auto& layer : stage->GetMutedLayers())
        if (std::find(desired.begin(), desired.end(), layer) == desired.end()) unmute.push_back(layer);
    if (stage->GetMutedLayers() != desired) stage->MuteAndUnmuteLayers(desired, unmute);
}

void refresh_animation_binding(AnimationBindingContext& ctx) {
    if (ctx.initialized) return;
    if (!ctx.source.empty()) {
        const auto source = ctx.stage->GetPrimAtPath(SdfPath(ctx.source));
        if (!source || !source.IsActive() || !source.IsLoaded() || !source.IsA<UsdSkelAnimation>())
            throw std::runtime_error("animation source was removed or unloaded: " + ctx.source);
    }
    refresh_skeleton_cache(ctx.stage, &ctx.cache, &ctx.dependencies,
                           &ctx.initialized, SdfPath(ctx.owner));
    ctx.initialized = false;
    // Bare skeletons are valid native animation consumers too (e.g. rig
    // inspection before meshes are attached). ComputeSkelBindings lists only
    // skinning consumers, so retain skeleton-only dependencies explicitly.
    for (const auto& root : ctx.roots) {
        const auto target = ctx.stage->GetPrimAtPath(SdfPath(root));
        if (!target || !target.IsActive() || !target.IsLoaded()) throw std::runtime_error("animation target was removed or unloaded");
        for (const auto& prim : UsdPrimRange(target)) {
            if (!prim.IsA<UsdSkelSkeleton>() || std::any_of(ctx.dependencies.begin(), ctx.dependencies.end(),
                [&](const auto& d) { return d.skeleton_path == prim.GetPath(); })) continue;
            const auto query = ctx.cache.GetSkelQuery(UsdSkelSkeleton(prim));
            if (!query) throw std::runtime_error("animation skeleton requires a SkelRoot within its actor");
            auto ancestor = prim.GetParent();
            while (ancestor && !ancestor.IsA<UsdSkelRoot>()) ancestor = ancestor.GetParent();
            CachedSkeletonDependency dependency;
            dependency.root_path = ancestor ? ancestor.GetPath() : prim.GetPath();
            dependency.skeleton_path = prim.GetPath();
            if (query.GetAnimQuery()) dependency.animation_path = query.GetAnimQuery().GetPrim().GetPath();
            ctx.dependencies.push_back(std::move(dependency));
        }
    }
    for (const auto& dep : ctx.dependencies) {
        if (!animation_contains(ctx.roots, dep.skeleton_path)) continue;
        for (const auto& mesh : dep.skinned_paths) {
            if (!animation_contains(ctx.roots, mesh))
                throw std::runtime_error("animation binding escapes targets at " + mesh.GetString());
        }
        const auto query = ctx.cache.GetSkelQuery(UsdSkelSkeleton(ctx.stage->GetPrimAtPath(dep.skeleton_path)));
        std::string reason;
        if (!query || !query.GetTopology().Validate(&reason))
            throw std::runtime_error("invalid skeleton topology: " + reason);
        if (!ctx.source.empty()) {
            const auto anim = query.GetAnimQuery();
            if (!anim || anim.GetPrim().GetPath() != SdfPath(ctx.source))
                throw std::runtime_error("selected native animation source did not bind");
            const auto joints = query.GetJointOrder();
            const auto animated = anim.GetJointOrder();
            if (!animated.empty() && std::none_of(animated.begin(), animated.end(), [&](const auto& joint) {
                    return std::find(joints.begin(), joints.end(), joint) != joints.end();
                })) throw std::runtime_error("animation source has no compatible skeleton joints");
        }
        VtMatrix4dArray rest;
        if (!query.ComputeJointLocalTransforms(&rest, UsdTimeCode::Default(), true) ||
            rest.size() != query.GetJointOrder().size() || !matrices_are_finite(rest))
            throw std::runtime_error("invalid skeleton rest transforms");
        VtMatrix4dArray palette;
        if (!query.ComputeSkinningTransforms(&palette, UsdTimeCode(0)) ||
            palette.size() != query.GetJointOrder().size() || !matrices_are_finite(palette))
            throw std::runtime_error("invalid native source pose or bind transforms");
    }
    ctx.initialized = true;
}
} // namespace

rust::Vec<AnimationClipRaw> read_animation_clips(const StageHandle& handle, rust::Str owner) {
    return handle.impl_->access([&] {
        rust::Vec<AnimationClipRaw> result;
        const auto prim = handle.impl_->stage->GetPrimAtPath(SdfPath(to_std(owner)));
        if (!prim || !prim.HasAttribute(TfToken("idtx:animation:contractVersion"))) return result;
        try {
            const auto version = animation_value<int>(prim, "idtx:animation:contractVersion", SdfValueTypeNames->Int, 0, true);
            if (version != 1) throw std::runtime_error("unsupported animation contractVersion (expected 1)");
            const double rate = handle.impl_->stage->GetTimeCodesPerSecond();
            if (!std::isfinite(rate) || rate <= 0) throw std::runtime_error("animation requires finite positive timeCodesPerSecond");
            if (!(prim.IsA<UsdGeomXform>() || prim.IsA<UsdSkelRoot>()) || prim.IsInstanceProxy() || prim.IsInPrototype())
                throw std::runtime_error("animation owner must be an editable Xform or SkelRoot");
            const auto names = animation_value<VtTokenArray>(prim, "idtx:animation:clipNames", SdfValueTypeNames->TokenArray, {}, true);
            if (names.size() > 128) throw std::runtime_error("animation owner exceeds 128 clips");
            std::set<std::string> seen;
            for (const auto& token : names) {
                AnimationClipRaw record;
                const auto name = token.GetString();
                record.name = rust::String(name);
                record.speed = 1;
                record.channel = rust::String("body");
                try {
                    if (name.size() > 256 || !TfIsValidIdentifier(name) || !seen.insert(name).second)
                        throw std::runtime_error("clip names must be unique identifiers");
                    const std::string prefix = "idtx:animation:" + name + ":";
                    const auto targets = animation_targets(prim, prefix + "targets");
                    if (targets.empty() || targets.size() > 64) throw std::runtime_error("clip requires 1..64 target roots");
                    std::vector<SdfPath> normalized;
                    for (const auto& path : targets) {
                        if (!path.HasPrefix(prim.GetPath())) throw std::runtime_error("clip target is outside its owner");
                        add_projection_root(&normalized, path);
                    }
                    for (const auto& path : normalized) record.targets.push_back(rust::String(path.GetString()));
                    const auto sources = animation_targets(prim, prefix + "source");
                    if (sources.size() > 1) throw std::runtime_error("clip requires at most one source");
                    if (!sources.empty()) {
                        record.source = rust::String(sources[0].GetString());
                        if (!handle.impl_->stage->GetPrimAtPath(sources[0]).IsA<UsdSkelAnimation>())
                            throw std::runtime_error("clip source must be SkelAnimation: " + sources[0].GetString());
                    }
                    record.start = animation_value<GfTimeCode>(prim, prefix + "start", SdfValueTypeNames->TimeCode, GfTimeCode(), true).GetValue();
                    record.end = animation_value<GfTimeCode>(prim, prefix + "end", SdfValueTypeNames->TimeCode, GfTimeCode(), true).GetValue();
                    record.speed = animation_value<double>(prim, prefix + "speed", SdfValueTypeNames->Double, 1);
                    record.looping = animation_value<bool>(prim, prefix + "loop", SdfValueTypeNames->Bool, false);
                    record.autoplay = animation_value<bool>(prim, prefix + "autoPlay", SdfValueTypeNames->Bool, false);
                    record.channel = rust::String(animation_value<TfToken>(prim, prefix + "channel", SdfValueTypeNames->Token, TfToken("body")).GetString());
                    if (!std::isfinite(record.start) || !std::isfinite(record.end) || record.end <= record.start || !std::isfinite(record.speed))
                        throw std::runtime_error("clip range/speed must be finite and end must exceed start");
                    if (record.channel.empty()) throw std::runtime_error("clip channel must not be empty");
                } catch (const std::exception& e) { record.error = rust::String(e.what()); }
                result.push_back(std::move(record));
            }
        } catch (const std::exception& e) {
            AnimationClipRaw record;
            record.error = rust::String(e.what());
            result.push_back(std::move(record));
        }
        return result;
    });
}

void prepare_animation_binding(const StageHandle& handle, rust::Str key_text, rust::Str owner_text,
                               rust::Str source_text, rust::Slice<const rust::String> roots_text) {
    with_stage_lifecycle_lock<std::unique_lock<std::shared_mutex>>([&] {
        const auto key = to_std(key_text), owner = to_std(owner_text), source = to_std(source_text);
        std::vector<std::string> roots;
        for (const auto& root : roots_text) roots.push_back(to_std(root));
        // Validate incoming consumers against the main stage too: updating a
        // palette can deform a mesh outside the requested subtree even when
        // that mesh itself was not included in this action's projection.
        const auto validate_consumers = [&] {
            if (handle.impl_->observer->take_skeleton_cache_dirty()) handle.impl_->skeleton_cache_initialized = false;
            refresh_skeleton_cache(handle.impl_->stage, &handle.impl_->skeleton_cache,
                &handle.impl_->skeleton_dependencies, &handle.impl_->skeleton_cache_initialized);
            for (const auto& dependency : handle.impl_->skeleton_dependencies) {
                if (!animation_contains(roots, dependency.skeleton_path)) continue;
                for (const auto& mesh : dependency.skinned_paths)
                    if (!animation_contains(roots, mesh)) throw std::runtime_error("animation has an out-of-target skin consumer: " + mesh.GetString());
            }
        };
        auto found = handle.impl_->animation_bindings.find(key);
        if (found != handle.impl_->animation_bindings.end() && found->second->roots == roots &&
            found->second->owner == owner && found->second->source == source) {
            auto& ctx = *found->second;
            sync_animation_stage(handle.impl_->stage, ctx.stage);
            bool check_consumers = false;
            for (const auto& change : ctx.observer->drain()) {
                const SdfPath path(change.prim_path);
                check_consumers |= change.resync || (change.domains & (kSkeleton | kRelationship)) != 0;
                if ((change.resync || (change.domains & (kSkeleton | kAnimation | kRelationship))) &&
                    (paths_intersect(path, SdfPath(owner)) || (!source.empty() && paths_intersect(path, SdfPath(source)))))
                    ctx.initialized = false;
            }
            if (check_consumers) validate_consumers();
            refresh_animation_binding(ctx);
            return;
        }
        const auto original = handle.impl_->stage;
        validate_consumers();
        const auto actor = original->GetPrimAtPath(SdfPath(owner));
        if (!actor || actor.IsInstanceProxy() || actor.IsInPrototype() || roots.empty())
            throw std::runtime_error("animation requires a live editable owner and explicit targets");
        auto ctx = std::make_unique<AnimationBindingContext>();
        ctx->owner = owner; ctx->source = source; ctx->roots = roots;
        auto session = SdfLayer::CreateAnonymous("idtx-animation-session.usda");
        session->SetSubLayerPaths({original->GetSessionLayer()->GetIdentifier()});
        ctx->stage = UsdStage::Open(original->GetRootLayer(), session,
                                   original->GetPathResolverContext(), UsdStage::LoadNone);
        sync_animation_stage(original, ctx->stage);
        ctx->stage->SetEditTarget(session);
        std::size_t bound = 0;
        for (const auto& root : roots) {
            const auto target = ctx->stage->GetPrimAtPath(SdfPath(root));
            if (!target || !target.IsActive() || !target.IsLoaded() || !target.GetPath().HasPrefix(SdfPath(owner)))
                throw std::runtime_error("invalid animation target " + root);
            for (const auto& prim : UsdPrimRange(target, UsdTraverseInstanceProxies())) {
                if (prim.IsInstanceProxy() || prim.IsInPrototype())
                    throw std::runtime_error("use editable referenced rigs for independent animation");
                if (!prim.IsA<UsdSkelSkeleton>()) continue;
                ++bound;
                if (!source.empty()) {
                    if (!ctx->stage->GetPrimAtPath(SdfPath(source)).IsA<UsdSkelAnimation>())
                        throw std::runtime_error("missing or invalid animation source " + source);
                    if (!UsdSkelBindingAPI::Apply(prim).CreateAnimationSourceRel().SetTargets({SdfPath(source)}))
                        throw std::runtime_error("could not select native animation source");
                }
            }
        }
        if (!source.empty() && bound == 0) throw std::runtime_error("skeletal clip targets contain no skeleton");
        refresh_animation_binding(*ctx);
        if (!source.empty() && ctx->dependencies.empty()) throw std::runtime_error("skeletal clip requires a native SkelRoot and skeleton");
        ctx->observer = std::make_unique<Observer>();
        ctx->observer_key = TfNotice::Register(TfCreateWeakPtr(ctx->observer.get()), &Observer::changed, ctx->stage);
        handle.impl_->animation_bindings[key] = std::move(ctx);
    });
}

void release_animation_binding(const StageHandle& handle, rust::Str key) {
    handle.impl_->access([&] { handle.impl_->animation_bindings.erase(to_std(key)); });
}

ViewportProjectionRaw project_animation_binding(const StageHandle& handle, rust::Str key, double tc, double placement_tc, std::uint32_t mode) {
    return handle.impl_->access([&] {
        const auto found = handle.impl_->animation_bindings.find(to_std(key));
        if (found == handle.impl_->animation_bindings.end() || !std::isfinite(tc) || !std::isfinite(placement_tc))
            throw std::runtime_error("animation binding not prepared or invalid sample time");
        auto& ctx = *found->second;
        rust::Vec<rust::String> roots;
        for (const auto& root : ctx.roots) roots.push_back(rust::String(root));
        ViewportProjectionRaw out;
        if (mode == 2) {
            out.schema_version = static_cast<std::uint32_t>(ViewportProjectionSchema::Current);
            UsdGeomXformCache transforms{UsdTimeCode(tc)};
            for (const auto& root : ctx.roots) {
                for (const auto& prim : UsdPrimRange(ctx.stage->GetPrimAtPath(SdfPath(root)))) {
                    TransformRecordRaw record;
                    record.path = rust::String(prim.GetPath().GetString());
                    append_matrix(transforms.GetLocalToWorldTransform(prim), &record.world_matrix);
                    out.transforms.push_back(std::move(record));
                }
            }
        } else {
            out = project_viewport_locked(ctx.stage, &ctx.point_prototypes, &ctx.prototype_roots,
                &ctx.point_initialized, &ctx.cache, &ctx.dependencies, &ctx.initialized,
                &ctx.prototypes_resident, tc, false,
                rust::Slice<const rust::String>(roots.data(), roots.size()), mode == 1);
        }
        if (mode != 0) {
            // Visibility inherits from placement ancestors even when only
            // matrices/palettes change. Carry it without re-extracting meshes.
            for (const auto& root : ctx.roots) {
                for (const auto& prim : UsdPrimRange(ctx.stage->GetPrimAtPath(SdfPath(root)))) {
                    const UsdGeomImageable imageable(prim);
                    if (!imageable) continue;
                    VisibilityRecordRaw record;
                    record.path = rust::String(prim.GetPath().GetString());
                    record.purpose = rust::String(imageable.ComputePurpose().GetString());
                    record.casts_shadow = true;
                    const auto casts_shadow = prim.GetAttribute(TfToken("idtx:castsShadows"));
                    if (casts_shadow) casts_shadow.Get(&record.casts_shadow);
                    out.visibility.push_back(std::move(record));
                }
            }
        }
        // USD expands dependencies before projection. Only bounded actor writes
        // cross the bridge, even if a source is shared outside this actor.
        const auto keep = [&](const auto& path) { return animation_contains(ctx.roots, SdfPath(to_std(path))); };
#define IDTX_FILTER_ANIMATION(field, pathfield) \
        { decltype(out.field) retained; for (auto& record : out.field) \
          if (keep(record.pathfield)) retained.push_back(std::move(record)); out.field = std::move(retained); }
        IDTX_FILTER_ANIMATION(transforms, path)
        IDTX_FILTER_ANIMATION(visibility, path)
        IDTX_FILTER_ANIMATION(bounds, path)
        IDTX_FILTER_ANIMATION(meshes, path)
        IDTX_FILTER_ANIMATION(curves, path)
        IDTX_FILTER_ANIMATION(cubes, path)
        IDTX_FILTER_ANIMATION(spheres, path)
        IDTX_FILTER_ANIMATION(extended_gprims, path)
        IDTX_FILTER_ANIMATION(planes, path)
        IDTX_FILTER_ANIMATION(point_instancers, path)
        IDTX_FILTER_ANIMATION(native_instances, path)
        IDTX_FILTER_ANIMATION(cameras, path)
        IDTX_FILTER_ANIMATION(lights, path)
        IDTX_FILTER_ANIMATION(skeleton_topologies, skeleton_path)
        IDTX_FILTER_ANIMATION(skeleton_poses, skeleton_path)
        IDTX_FILTER_ANIMATION(skin_bindings, path)
        IDTX_FILTER_ANIMATION(skeleton_dependencies, skeleton_path)
#undef IDTX_FILTER_ANIMATION
        for (auto& record : out.visibility) {
            record.visible = true;
            for (auto prim = ctx.stage->GetPrimAtPath(SdfPath(to_std(record.path)));
                 prim && !prim.IsPseudoRoot(); prim = prim.GetParent()) {
                const UsdGeomImageable imageable(prim);
                if (!imageable) continue;
                TfToken visibility;
                const double time = animation_contains(ctx.roots, prim.GetPath()) ? tc : placement_tc;
                if (imageable.GetVisibilityAttr().Get(&visibility, UsdTimeCode(time)) && visibility == UsdGeomTokens->invisible) {
                    record.visible = false;
                    break;
                }
            }
        }
        // Clip-local animation composes with the containing timeline's placement.
        // Correct world records, leaving skeleton-space palettes/data untouched.
        UsdGeomXformCache clip_xforms{UsdTimeCode(tc)}, placement_xforms{UsdTimeCode(placement_tc)};
        std::map<SdfPath, GfMatrix4d> corrections;
        for (const auto& root : ctx.roots) {
            const auto parent = ctx.stage->GetPrimAtPath(SdfPath(root).GetParentPath());
            if (!parent || parent.IsPseudoRoot()) continue;
            const auto old_parent = clip_xforms.GetLocalToWorldTransform(parent);
            const auto new_parent = placement_xforms.GetLocalToWorldTransform(parent);
            if (old_parent != new_parent) {
                if (std::abs(old_parent.GetDeterminant()) < 1e-12)
                    throw std::runtime_error("singular animation placement transform");
                corrections.emplace(SdfPath(root), old_parent.GetInverse() * new_parent);
            }
        }
        const auto correction = [&](const rust::String& path) -> const GfMatrix4d* {
            for (const auto& entry : corrections) if (SdfPath(to_std(path)).HasPrefix(entry.first)) {
                for (auto prim = ctx.stage->GetPrimAtPath(SdfPath(to_std(path)));
                     prim && prim.GetPath().HasPrefix(entry.first); prim = prim.GetParent()) {
                    const UsdGeomXformable xform(prim);
                    if (xform && xform.GetResetXformStack()) return nullptr;
                }
                return &entry.second;
            }
            return nullptr;
        };
        const auto transform_matrices = [](rust::Vec<float>& values, const GfMatrix4d& delta) {
            for (std::size_t i = 0; i + 16 <= values.size(); i += 16) {
                GfMatrix4d matrix;
                for (int r = 0; r < 4; ++r) for (int c = 0; c < 4; ++c) matrix[r][c] = values[i + r * 4 + c];
                matrix *= delta;
                for (int r = 0; r < 4; ++r) for (int c = 0; c < 4; ++c) values[i + r * 4 + c] = static_cast<float>(matrix[r][c]);
            }
        };
        const auto transform_bounds = [](rust::Vec<float>& minimum, rust::Vec<float>& maximum, const GfMatrix4d& delta) {
            if (minimum.size() != 3 || maximum.size() != 3) return;
            GfVec3d low(std::numeric_limits<double>::infinity()), high(-std::numeric_limits<double>::infinity());
            for (int i = 0; i < 8; ++i) {
                const auto p = delta.Transform(GfVec3d((i & 1) ? maximum[0] : minimum[0],
                    (i & 2) ? maximum[1] : minimum[1], (i & 4) ? maximum[2] : minimum[2]));
                for (int j = 0; j < 3; ++j) { low[j] = std::min(low[j], p[j]); high[j] = std::max(high[j], p[j]); }
            }
            for (int j = 0; j < 3; ++j) { minimum[j] = low[j]; maximum[j] = high[j]; }
        };
        for (auto& record : out.transforms) if (const auto* delta = correction(record.path)) transform_matrices(record.world_matrix, *delta);
        for (auto& record : out.skeleton_poses) if (const auto* delta = correction(record.skeleton_path)) {
            transform_matrices(record.skeleton_world_transform, *delta);
            transform_matrices(record.joint_world_transforms, *delta);
        }
        for (auto& record : out.bounds) if (const auto* delta = correction(record.path)) transform_bounds(record.minimum, record.maximum, *delta);
        for (auto& record : out.skin_bindings) if (const auto* delta = correction(record.path))
            transform_bounds(record.skinned_bounds_minimum, record.skinned_bounds_maximum, *delta);
        return out;
    });
}
