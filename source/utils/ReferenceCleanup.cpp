/**
 * @file ReferenceCleanup.cpp
 * @brief Implementation of the dangling-arc detection/removal helpers.
 *
 * See ReferenceCleanup.h for the behavioural contract.
 **/

#include "utils/ReferenceCleanup.h"

#include <filesystem>
#include <map>
#include <set>
#include <string>
#include <system_error>
#include <tuple>
#include <vector>

#include <pxr/pxr.h>
#include <pxr/base/vt/array.h>
#include <pxr/base/vt/value.h>
#include <pxr/usd/ar/resolvedPath.h>
#include <pxr/usd/ar/resolver.h>
#include <pxr/usd/ar/resolverContextBinder.h>
#include <pxr/usd/sdf/assetPath.h>
#include <pxr/usd/sdf/attributeSpec.h>
#include <pxr/usd/sdf/layer.h>
#include <pxr/usd/sdf/layerUtils.h>
#include <pxr/usd/sdf/path.h>
#include <pxr/usd/sdf/payload.h>
#include <pxr/usd/sdf/primSpec.h>
#include <pxr/usd/sdf/proxyTypes.h>
#include <pxr/usd/sdf/reference.h>
#include <pxr/usd/sdf/schema.h>

#include <idtx/utils/Logger.h>

namespace fs = std::filesystem;
PXR_NAMESPACE_USING_DIRECTIVE

IDTX_LOG_CATEGORY("ReferenceCleanup")

namespace idtx::utils {

namespace {

// Decide whether an asset path referenced from `layer` is dangling. Returns
// true and sets `reason` when it is; false for a healthy or internal arc.
bool ClassifyAssetPath(const SdfLayerHandle&    layer,
                       const std::string&       assetPath,
                       const ArResolverContext& context,
                       DanglingReason&          reason)
{
    // An empty asset path is an internal/self reference - never dangling.
    if (assetPath.empty())
        return false;

    // Anonymous layer identifiers are session-local and can never resolve in
    // any other process. This is the Houdini/Solaris export artifact.
    if (SdfLayer::IsAnonymousLayerIdentifier(assetPath))
    {
        reason = DanglingReason::AnonymousLayer;
        return true;
    }

    // Anchor the (possibly relative) path to the owning layer, then resolve it
    // under the stage's resolver context so relative paths are judged exactly
    // as the stage would resolve them.
    const std::string anchored =
        SdfComputeAssetPathRelativeToLayer(layer, assetPath);
    if (anchored.empty())
    {
        reason = DanglingReason::Unresolvable;
        return true;
    }

    ArResolverContextBinder binder(context);
    const ArResolvedPath resolved = ArGetResolver().Resolve(anchored);
    if (resolved.empty())
    {
        reason = DanglingReason::Unresolvable;
        return true;
    }

    return false;
}

// Inspect the reference and payload arcs authored on a single prim spec.
void InspectPrimArcs(const SdfLayerHandle&     layer,
                     const SdfPrimSpecHandle&  prim,
                     const ArResolverContext&  context,
                     std::vector<DanglingArc>& out)
{
    DanglingReason reason{};

    for (const SdfReference& ref : prim->GetReferenceList().GetAppliedItems())
    {
        if (ClassifyAssetPath(layer, ref.GetAssetPath(), context, reason))
            out.push_back(DanglingArc{ArcKind::Reference, prim->GetPath(),
                                      ref.GetAssetPath(),
                                      ref.GetPrimPath().GetString(), reason});
    }

    for (const SdfPayload& pl : prim->GetPayloadList().GetAppliedItems())
    {
        if (ClassifyAssetPath(layer, pl.GetAssetPath(), context, reason))
            out.push_back(DanglingArc{ArcKind::Payload, prim->GetPath(),
                                      pl.GetAssetPath(),
                                      pl.GetPrimPath().GetString(), reason});
    }
}

// True when an asset path contains an unexpanded template token that the
// resolver only expands at runtime - a UDIM/UV-tile token (`<UDIM>`, `<UVTILE>`,
// any `<...>`) or a printf/Python-dict token (`%(...)`). Such paths never
// resolve to a single on-disk file and must not be judged "unresolvable".
bool HasUnexpandedToken(const std::string& assetPath)
{
    const std::size_t lt = assetPath.find('<');
    if (lt != std::string::npos && assetPath.find('>', lt) != std::string::npos)
        return true;
    return assetPath.find("%(") != std::string::npos;
}

// Normalize one asset path for comparison when inspecting an attribute value.
// Returns true (and sets `reason`) when the non-empty, non-template path cannot
// be resolved under `context`; false for empty/template/resolvable paths.
bool IsAttributePathDangling(const SdfLayerHandle&    layer,
                             const std::string&       assetPath,
                             const ArResolverContext& context,
                             DanglingReason&          reason)
{
    if (assetPath.empty() || HasUnexpandedToken(assetPath))
        return false;
    return ClassifyAssetPath(layer, assetPath, context, reason);
}

// Inspect the asset-valued attribute specs on a single prim spec, reporting any
// value (default or time sample) whose path cannot be resolved.
void InspectPrimAssetAttrs(const SdfLayerHandle&     layer,
                           const SdfPrimSpecHandle&  prim,
                           const ArResolverContext&  context,
                           std::vector<DanglingArc>& out)
{
    for (const SdfAttributeSpecHandle& attr : prim->GetAttributes())
    {
        const SdfValueTypeName tn = attr->GetTypeName();
        const bool isAsset = (tn == SdfValueTypeNames->Asset);
        const bool isAssetArray = (tn == SdfValueTypeNames->AssetArray);
        if (!isAsset && !isAssetArray)
            continue;

        // Gather every authored value: the default plus each time sample.
        std::vector<VtValue> values;
        if (attr->HasDefaultValue())
            values.push_back(attr->GetDefaultValue());
        for (double t : layer->ListTimeSamplesForPath(attr->GetPath()))
        {
            VtValue v;
            if (layer->QueryTimeSample(attr->GetPath(), t, &v))
                values.push_back(v);
        }

        // De-duplicate the dead asset paths we report per attribute, so a path
        // repeated across time samples yields a single arc.
        std::set<std::string> seen;
        DanglingReason reason{};
        for (const VtValue& v : values)
        {
            auto consider = [&](const std::string& path)
            {
                if (IsAttributePathDangling(layer, path, context, reason) &&
                    seen.insert(path).second)
                    out.push_back(DanglingArc{ArcKind::Attribute, attr->GetPath(),
                                              path, std::string{}, reason});
            };

            if (isAsset && v.IsHolding<SdfAssetPath>())
                consider(v.UncheckedGet<SdfAssetPath>().GetAssetPath());
            else if (isAssetArray && v.IsHolding<VtArray<SdfAssetPath>>())
                for (const SdfAssetPath& a :
                         v.UncheckedGet<VtArray<SdfAssetPath>>())
                    consider(a.GetAssetPath());
        }
    }
}

} // anonymous namespace

// ---------------------------------------------------------------------------
// ToString helpers
// ---------------------------------------------------------------------------
const char* ToString(DanglingReason reason)
{
    switch (reason)
    {
        case DanglingReason::AnonymousLayer: return "anonymous layer";
        case DanglingReason::Unresolvable:   return "unresolvable";
    }
    return "unknown";
}

const char* ToString(ArcKind kind)
{
    switch (kind)
    {
        case ArcKind::Reference: return "reference";
        case ArcKind::Payload:   return "payload";
        case ArcKind::SubLayer:  return "sublayer";
        case ArcKind::Attribute: return "attribute";
    }
    return "arc";
}

// ---------------------------------------------------------------------------
// FindDanglingArcs
// ---------------------------------------------------------------------------
std::vector<DanglingArc> FindDanglingArcs(const SdfLayerHandle&    layer,
                                          const ArResolverContext& context,
                                          bool                     includeAttributes)
{
    std::vector<DanglingArc> result;
    if (!layer)
        return result;

    // Walk every spec path; prim specs (including those nested in variant sets)
    // are the ones that can carry reference/payload arcs and asset attributes.
    layer->Traverse(SdfPath::AbsoluteRootPath(),
        [&](const SdfPath& specPath)
        {
            if (const SdfPrimSpecHandle prim = layer->GetPrimAtPath(specPath))
            {
                InspectPrimArcs(layer, prim, context, result);
                if (includeAttributes)
                    InspectPrimAssetAttrs(layer, prim, context, result);
            }
        });

    // Sublayers are authored at the layer level, not on a prim.
    DanglingReason reason{};
    for (const std::string& sub : layer->GetSubLayerPaths())
    {
        if (ClassifyAssetPath(layer, sub, context, reason))
            result.push_back(DanglingArc{ArcKind::SubLayer,
                                         SdfPath::AbsoluteRootPath(), sub,
                                         std::string{}, reason});
    }

    return result;
}

// ---------------------------------------------------------------------------
// RemoveDanglingArcs
// ---------------------------------------------------------------------------
int RemoveDanglingArcs(const SdfLayerHandle&           layer,
                       const std::vector<DanglingArc>& arcs,
                       bool                            anonOnly)
{
    if (!layer)
        return 0;

    // Build quick lookups of what to remove, keyed by (prim path, asset path).
    std::set<std::pair<std::string, std::string>> refKeys;
    std::set<std::pair<std::string, std::string>> payloadKeys;
    std::set<std::string>                         sublayerPaths;
    // Attribute arcs: property path -> set of dead asset-path strings.
    std::map<std::string, std::set<std::string>>  attrDead;

    for (const DanglingArc& arc : arcs)
    {
        if (anonOnly && arc.reason != DanglingReason::AnonymousLayer)
            continue;
        switch (arc.kind)
        {
            case ArcKind::Reference:
                refKeys.emplace(arc.primPath.GetString(), arc.assetPath);
                break;
            case ArcKind::Payload:
                payloadKeys.emplace(arc.primPath.GetString(), arc.assetPath);
                break;
            case ArcKind::SubLayer:
                sublayerPaths.insert(arc.assetPath);
                break;
            case ArcKind::Attribute:
                attrDead[arc.primPath.GetString()].insert(arc.assetPath);
                break;
        }
    }

    int removed = 0;

    // References and payloads: re-fetch the live, stored arc objects so that
    // equality (asset path + prim path + layer offset + custom data) matches
    // exactly when we ask the list editor to drop them.
    if (!refKeys.empty() || !payloadKeys.empty())
    {
        layer->Traverse(SdfPath::AbsoluteRootPath(),
            [&](const SdfPath& specPath)
            {
                const SdfPrimSpecHandle prim = layer->GetPrimAtPath(specPath);
                if (!prim)
                    return;
                const std::string primStr = prim->GetPath().GetString();

                if (!refKeys.empty())
                {
                    SdfReferencesProxy refs = prim->GetReferenceList();
                    for (const SdfReference& ref : refs.GetAppliedItems())
                    {
                        if (refKeys.count({primStr, ref.GetAssetPath()}))
                        {
                            refs.RemoveItemEdits(ref);
                            ++removed;
                        }
                    }
                }

                if (!payloadKeys.empty())
                {
                    SdfPayloadsProxy pls = prim->GetPayloadList();
                    for (const SdfPayload& pl : pls.GetAppliedItems())
                    {
                        if (payloadKeys.count({primStr, pl.GetAssetPath()}))
                        {
                            pls.RemoveItemEdits(pl);
                            ++removed;
                        }
                    }
                }
            });
    }

    // Sublayers: remove by index. Each removal shifts later indices, so we
    // re-scan for every path we want gone.
    for (const std::string& dead : sublayerPaths)
    {
        bool again = true;
        while (again)
        {
            again = false;
            const SdfSubLayerProxy subs = layer->GetSubLayerPaths();
            for (size_t i = 0; i < subs.size(); ++i)
            {
                const std::string cur = subs[i];
                if (cur == dead)
                {
                    layer->RemoveSubLayerPath(static_cast<int>(i));
                    ++removed;
                    again = true;
                    break;
                }
            }
        }
    }

    // Asset-valued attributes: drop the unresolvable path(s). A single-valued
    // asset attribute is removed from its prim entirely; an asset[] keeps its
    // resolvable entries and only loses the dead ones (and is itself removed if
    // that empties every authored value).
    for (const auto& [propPathStr, deadSet] : attrDead)
    {
        const SdfPath propPath(propPathStr);
        const SdfAttributeSpecHandle attr = layer->GetAttributeAtPath(propPath);
        if (!attr)
            continue;
        const SdfPrimSpecHandle prim =
            layer->GetPrimAtPath(propPath.GetPrimPath());
        if (!prim)
            continue;

        const bool isArray = (attr->GetTypeName() == SdfValueTypeNames->AssetArray);

        // Returns the pruned value for `in`, or an empty VtValue when every
        // element was dead; increments `removed` for each dropped path.
        auto prune = [&](const VtValue& in) -> VtValue
        {
            if (!isArray)
            {
                if (in.IsHolding<SdfAssetPath>() &&
                    deadSet.count(in.UncheckedGet<SdfAssetPath>().GetAssetPath()))
                {
                    ++removed;
                    return VtValue(); // whole value is dead
                }
                return in;
            }

            if (!in.IsHolding<VtArray<SdfAssetPath>>())
                return in;
            VtArray<SdfAssetPath> kept;
            for (const SdfAssetPath& a : in.UncheckedGet<VtArray<SdfAssetPath>>())
            {
                if (deadSet.count(a.GetAssetPath()))
                    ++removed;
                else
                    kept.push_back(a);
            }
            if (kept.empty())
                return VtValue();
            return VtValue(kept);
        };

        bool anyLiveRemaining = false;

        if (attr->HasDefaultValue())
        {
            const VtValue pruned = prune(attr->GetDefaultValue());
            if (pruned.IsEmpty())
                attr->ClearDefaultValue();
            else
            {
                attr->SetDefaultValue(pruned);
                anyLiveRemaining = true;
            }
        }

        for (double t : layer->ListTimeSamplesForPath(propPath))
        {
            VtValue v;
            if (!layer->QueryTimeSample(propPath, t, &v))
                continue;
            const VtValue pruned = prune(v);
            if (pruned.IsEmpty())
                layer->EraseTimeSample(propPath, t);
            else
            {
                layer->SetTimeSample(propPath, t, pruned);
                anyLiveRemaining = true;
            }
        }

        // Nothing resolvable left on the attribute: remove the property so the
        // prim no longer carries a dead texture reference.
        if (!anyLiveRemaining)
            prim->RemoveProperty(attr);
    }

    return removed;
}

// ---------------------------------------------------------------------------
// CleanupLayerFileInPlace
// ---------------------------------------------------------------------------
CleanRefsResult CleanupLayerFileInPlace(const std::string&       path,
                                        bool                     anonOnly,
                                        bool                     dryRun,
                                        const ArResolverContext& context,
                                        bool                     includeTextures)
{
    CleanRefsResult result;

    const fs::path target(path);
    const fs::path tmp =
        target.string() + ".idtxclean" + target.extension().string();
    std::error_code ec;

    {
        SdfLayerRefPtr layer = SdfLayer::FindOrOpen(path);
        if (!layer)
        {
            IDTX_LOG(IDTX_ERROR,
                "  Could not open layer '{}' for inspection", path);
            return result; // ok == false
        }

        result.arcs  = FindDanglingArcs(layer, context, includeTextures);
        result.found = static_cast<int>(result.arcs.size());

        // How many of the found arcs are removable under the current filter?
        int removable = 0;
        for (const DanglingArc& arc : result.arcs)
            if (!anonOnly || arc.reason == DanglingReason::AnonymousLayer)
                ++removable;

        if (dryRun || removable == 0)
        {
            // Nothing to write (either reporting only, or every found arc is
            // kept by the --anon-only filter).
            result.ok = true;
            return result;
        }

        result.removed = RemoveDanglingArcs(layer, result.arcs, anonOnly);
        if (result.removed == 0)
        {
            result.ok = true;
            return result;
        }

        fs::remove(tmp, ec); // best-effort cleanup of any stale temp

        // Export (not Save) the edited layer to a sibling temp, keeping the
        // extension so the original on-disk encoding is preserved. The layer
        // handle (and its mapping of the original file) is released at the end
        // of this scope, before we replace the original below.
        if (!layer->Export(tmp.string()))
        {
            IDTX_LOG(IDTX_ERROR, "  Failed to export cleaned layer '{}'", path);
            fs::remove(tmp, ec);
            return result; // ok == false
        }
    }

    // Atomically replace the original: remove it, then move the temp into
    // place, falling back to copy+remove if the move fails cross-device.
    fs::remove(target, ec);
    std::error_code moveEc;
    fs::rename(tmp, target, moveEc);
    if (moveEc)
    {
        std::error_code copyEc;
        fs::copy_file(tmp, target, fs::copy_options::overwrite_existing, copyEc);
        fs::remove(tmp, ec);
        if (copyEc)
        {
            IDTX_LOG(IDTX_ERROR, "  Could not replace original '{}': {}",
                path, copyEc.message());
            return result; // ok == false
        }
    }

    result.ok = true;
    return result;
}

} // namespace idtx::utils
