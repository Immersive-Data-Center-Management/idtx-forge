/**
 * @file instancing.cpp
 * @brief Implementation of the "instancing" CLI subcommand.
 *
 * Two selectable modes (see instancing.h for the full description):
 *
 *  - `pseudo` (default): convert pseudo-instances into native USD scenegraph
 *    instances by flipping the referenced `over` prototype to `class` and
 *    marking each pseudo-instance `instanceable = true`.
 *
 *  - `identical-mesh`: scan the stage for identical mesh prims (an Xform
 *    wrapper with exactly one GeomMesh child whose geometry arrays match),
 *    author the first occurrence as a shared prototype `class` under
 *    `/__Prototypes__`, and turn every identical prim into a native instance
 *    that inherits that prototype (per-instance transforms preserved).
 *
 * In both modes only the root layer is mutated, so exporting the root layer
 * directly keeps every untouched prim spec verbatim.
 **/

#include "commands/instancing.h"

#include <algorithm>
#include <filesystem>
#include <set>
#include <sstream>
#include <string>
#include <unordered_map>
#include <vector>

#include <pxr/pxr.h>
#include <pxr/base/tf/hash.h>
#include <pxr/base/vt/array.h>
#include <pxr/base/vt/value.h>
#include <pxr/base/gf/vec3f.h>
#include <pxr/usd/sdf/attributeSpec.h>
#include <pxr/usd/sdf/copyUtils.h>
#include <pxr/usd/sdf/layer.h>
#include <pxr/usd/sdf/path.h>
#include <pxr/usd/sdf/primSpec.h>
#include <pxr/usd/sdf/propertySpec.h>
#include <pxr/usd/sdf/relationshipSpec.h>
#include <pxr/usd/sdf/types.h>
#include <pxr/usd/pcp/types.h>
#include <pxr/usd/usd/inherits.h>
#include <pxr/usd/usd/stage.h>
#include <pxr/usd/usd/prim.h>
#include <pxr/usd/usd/primRange.h>
#include <pxr/usd/usd/primCompositionQuery.h>
#include <pxr/usd/usdGeom/mesh.h>
#include <pxr/usd/usdGeom/tokens.h>

#include <idtx/utils/Logger.h>

#include "utils/stageutils.h"
#include "utils/UsdzRepackage.h"

namespace fs = std::filesystem;
PXR_NAMESPACE_USING_DIRECTIVE

IDTX_LOG_CATEGORY("Instancing")

namespace idtx::commands {

namespace {

// ---------------------------------------------------------------------------
// ConvertPrototypeToClass
// ---------------------------------------------------------------------------
bool ConvertPrototypeToClass(const SdfLayerHandle& layer,
                             const SdfPath&        prototypePath)
{
    SdfPrimSpecHandle proto = layer->GetPrimAtPath(prototypePath);
    if (!proto)
    {
        IDTX_LOG(IDTX_WARN,
            "  Prototype spec at '{}' not found - skipping",
            prototypePath.GetString());
        return false;
    }

    if (proto->GetSpecifier() == SdfSpecifierOver)
    {
        proto->SetSpecifier(SdfSpecifierClass);
        IDTX_LOG(IDTX_INFO,
            "  Prototype '{}' : over -> class", prototypePath.GetString());
    }
    else if (proto->GetSpecifier() == SdfSpecifierClass)
    {
        IDTX_LOG(IDTX_DEBUG,
            "  Prototype '{}' already a class", prototypePath.GetString());
    }
    else
    {
        SdfPrimSpecHandle parent = proto->GetNameParent();
        if (parent && parent->GetSpecifier() == SdfSpecifierOver)
        {
            parent->SetSpecifier(SdfSpecifierClass);
            IDTX_LOG(IDTX_INFO,
                "  Prototype parent '{}' : over -> class",
                parent->GetPath().GetString());
        }
        else
        {
            IDTX_LOG(IDTX_WARN,
                "  Prototype '{}' has unexpected specifier - leaving unchanged",
                prototypePath.GetString());
        }
    }

    return true;
}

// ---------------------------------------------------------------------------
// VtValueToString - stringify a VtValue for diagnostic output
// ---------------------------------------------------------------------------
std::string VtValueToString(const VtValue& v)
{
    std::ostringstream oss;
    oss << v;
    std::string s = oss.str();
    if (s.size() > 160)
        s = s.substr(0, 157) + "...";
    return s;
}

// ---------------------------------------------------------------------------
// MergePropertyIntoPrototype
// ---------------------------------------------------------------------------
void MergePropertyIntoPrototype(const SdfLayerHandle&        layer,
                                const SdfPropertySpecHandle& srcProp,
                                const SdfPath&               protoOwnerPath)
{
    if (!srcProp) return;

    const TfToken& propName    = srcProp->GetNameToken();
    const SdfPath  protoPropPath = protoOwnerPath.AppendProperty(propName);

    SdfPropertySpecHandle protoProp = layer->GetPropertyAtPath(protoPropPath);
    if (!protoProp)
    {
        if (!SdfCopySpec(layer, srcProp->GetPath(), layer, protoPropPath))
        {
            IDTX_LOG(IDTX_WARN,
                "    Failed to copy property '{}' -> '{}'",
                srcProp->GetPath().GetString(), protoPropPath.GetString());
            return;
        }
        IDTX_LOG(IDTX_DEBUG,
            "    Promoted property '{}' -> '{}'",
            srcProp->GetPath().GetString(), protoPropPath.GetString());
        return;
    }

    const SdfAttributeSpecHandle    srcAttr   = TfDynamic_cast<SdfAttributeSpecHandle>(srcProp);
    const SdfAttributeSpecHandle    protoAttr = TfDynamic_cast<SdfAttributeSpecHandle>(protoProp);
    const SdfRelationshipSpecHandle srcRel    = TfDynamic_cast<SdfRelationshipSpecHandle>(srcProp);
    const SdfRelationshipSpecHandle protoRel  = TfDynamic_cast<SdfRelationshipSpecHandle>(protoProp);

    if (srcAttr && protoAttr)
    {
        const VtValue srcVal   = srcAttr->GetDefaultValue();
        const VtValue protoVal = protoAttr->GetDefaultValue();

        if (srcVal != protoVal)
        {
            IDTX_LOG(IDTX_WARN,
                "    Instance override on '{}' differs from prototype: "
                "instance='{}' prototype='{}' - keeping prototype value",
                srcProp->GetPath().GetString(),
                VtValueToString(srcVal), VtValueToString(protoVal));
        }
        else
        {
            IDTX_LOG(IDTX_DEBUG,
                "    Override on '{}' matches prototype - stripping",
                srcProp->GetPath().GetString());
        }
    }
    else if (srcRel && protoRel)
    {
        const SdfPathListOp srcTargets =
            srcRel->GetField(SdfFieldKeys->TargetPaths).Get<SdfPathListOp>();
        const SdfPathListOp protoTargets =
            protoRel->GetField(SdfFieldKeys->TargetPaths).Get<SdfPathListOp>();

        if (srcTargets != protoTargets)
        {
            std::ostringstream srcOss, protoOss;
            srcOss   << srcTargets;
            protoOss << protoTargets;
            IDTX_LOG(IDTX_WARN,
                "    Instance override on relationship '{}' differs from "
                "prototype: instance={} prototype={} - keeping prototype targets",
                srcProp->GetPath().GetString(), srcOss.str(), protoOss.str());
        }
        else
        {
            IDTX_LOG(IDTX_DEBUG,
                "    Relationship override on '{}' matches prototype - stripping",
                srcProp->GetPath().GetString());
        }
    }
    else
    {
        IDTX_LOG(IDTX_WARN,
            "    Instance override on '{}' has mismatched type vs prototype "
            "- keeping prototype opinion",
            srcProp->GetPath().GetString());
    }
}

// ---------------------------------------------------------------------------
// MergeOverSubtreeIntoPrototype
// ---------------------------------------------------------------------------
void MergeOverSubtreeIntoPrototype(const SdfLayerHandle&    layer,
                                   const SdfPrimSpecHandle& srcSpec,
                                   const SdfPath&           protoTargetPath)
{
    if (!srcSpec) return;

    SdfPrimSpecHandle protoSpec = layer->GetPrimAtPath(protoTargetPath);
    if (!protoSpec)
    {
        protoSpec = SdfCreatePrimInLayer(layer, protoTargetPath);
        if (!protoSpec)
        {
            IDTX_LOG(IDTX_WARN,
                "    Could not create prototype spec at '{}' - "
                "skipping override merge",
                protoTargetPath.GetString());
            return;
        }
        IDTX_LOG(IDTX_DEBUG,
            "    Created prototype child spec '{}'",
            protoTargetPath.GetString());
    }

    for (const SdfPropertySpecHandle& propSpec : srcSpec->GetProperties())
        MergePropertyIntoPrototype(layer, propSpec, protoTargetPath);

    for (const SdfPrimSpecHandle& child : srcSpec->GetNameChildren())
        MergeOverSubtreeIntoPrototype(
            layer, child, protoTargetPath.AppendChild(child->GetNameToken()));
}

// ---------------------------------------------------------------------------
// PromoteAndStripInstanceOverrides
// ---------------------------------------------------------------------------
int PromoteAndStripInstanceOverrides(const SdfLayerHandle&    layer,
                                     const SdfPrimSpecHandle& instanceSpec,
                                     const SdfPath&           prototypePath)
{
    if (!instanceSpec) return 0;

    std::vector<SdfPrimSpecHandle> toStrip;
    for (const SdfPrimSpecHandle& child : instanceSpec->GetNameChildren())
    {
        if (child && child->GetSpecifier() == SdfSpecifierOver)
            toStrip.push_back(child);
    }

    for (const SdfPrimSpecHandle& overChild : toStrip)
    {
        const SdfPath protoChildPath =
            prototypePath.AppendChild(overChild->GetNameToken());

        IDTX_LOG(IDTX_DEBUG,
            "  Promoting instance override '{}' -> prototype '{}'",
            overChild->GetPath().GetString(), protoChildPath.GetString());

        MergeOverSubtreeIntoPrototype(layer, overChild, protoChildPath);
        instanceSpec->RemoveNameChild(overChild);
    }

    return static_cast<int>(toStrip.size());
}

// ---------------------------------------------------------------------------
// ConvertPseudoInstances - existing `pseudo` mode
// ---------------------------------------------------------------------------
void ConvertPseudoInstances(const UsdStageRefPtr& stage,
                            const std::string&    inputPath)
{
    std::set<SdfPath> convertedPrototypes;
    int               instanceCount = 0;

    // Gather candidates before mutating any prototype specifier.
    std::vector<std::pair<UsdPrim, SdfPath>> candidates;
    for (const UsdPrim& prim : stage->TraverseAll())
    {
        SdfPath prototypePath;
        if (!utils::IsPseudoInstance(prim, &prototypePath))
            continue;

        IDTX_LOG(IDTX_DEBUG,
            "  Pseudo-instance '{}' -> prototype '{}'",
            prim.GetPath().GetString(), prototypePath.GetString());

        candidates.emplace_back(prim, prototypePath);
    }

    SdfLayerHandle rootLayer      = stage->GetRootLayer();
    int            overridesMoved = 0;

    for (const auto& [prim, prototypePath] : candidates)
    {
        if (convertedPrototypes.insert(prototypePath).second)
            ConvertPrototypeToClass(rootLayer, prototypePath);

        const auto primStack = prim.GetPrimStack();
        if (!primStack.empty())
        {
            SdfPrimSpecHandle instanceSpec = primStack.front();
            overridesMoved += PromoteAndStripInstanceOverrides(
                rootLayer, instanceSpec, prototypePath);
        }

        if (!prim.SetInstanceable(true))
        {
            IDTX_LOG(IDTX_WARN,
                "  Failed to set instanceable=true on '{}'",
                prim.GetPath().GetString());
            continue;
        }
        ++instanceCount;
    }

    if (overridesMoved > 0)
        IDTX_LOG(IDTX_INFO,
            "  Promoted/stripped {} sparse instance override(s)", overridesMoved);

    if (instanceCount == 0 && convertedPrototypes.empty())
        IDTX_LOG(IDTX_WARN, "  No pseudo-instances detected in '{}'", inputPath);
    else
        IDTX_LOG(IDTX_INFO,
            "  Converted {} pseudo-instance(s) referencing {} prototype(s)",
            instanceCount, convertedPrototypes.size());
}

// ===========================================================================
// identical-mesh mode
// ===========================================================================

// The four geometry arrays that define mesh equality in iteration 1.
struct MeshArrays
{
    VtVec3fArray points;
    VtVec3fArray normals;
    VtIntArray   faceVertexIndices;
    VtIntArray   faceVertexCounts;

    bool hasPoints            = false;
    bool hasNormals           = false;
    bool hasFaceVertexIndices = false;
    bool hasFaceVertexCounts  = false;
};

// A candidate: an Xform wrapper prim with exactly one GeomMesh child.
//
// IMPORTANT: only *stable* identifiers (SdfPath / name) are cached here - never
// live UsdPrim / UsdGeomMesh handles. Handles obtained during traversal must
// not be reused across stage-composition mutations (SdfCreatePrimInLayer,
// RemoveNameChild, AddInherit, SetInstanceable, ...): any of those can trigger
// recomposition and silently invalidate previously obtained prim handles,
// leading to dangling references and undefined behaviour. All geometry reads
// happen up front during collection; every later step operates purely on
// SdfPath and freshly re-fetched specs / prims.
struct MeshCandidate
{
    SdfPath    xformPath;    ///< path of the wrapper prim (becomes the instance)
    SdfPath    meshPrimPath; ///< path of the single GeomMesh child
    TfToken    meshName;     ///< leaf name of the GeomMesh child
    MeshArrays arrays;       ///< geometry arrays read at default time
    size_t     hash = 0;     ///< pre-filter hash of the arrays
};

// ---------------------------------------------------------------------------
// ReadMeshArrays - read the four comparison arrays at Default() time
// ---------------------------------------------------------------------------
void ReadMeshArrays(const UsdGeomMesh& mesh, MeshArrays& out)
{
    const UsdTimeCode t = UsdTimeCode::Default();

    if (UsdAttribute a = mesh.GetPointsAttr(); a && a.HasAuthoredValue())
        out.hasPoints = a.Get(&out.points, t);
    if (UsdAttribute a = mesh.GetNormalsAttr(); a && a.HasAuthoredValue())
        out.hasNormals = a.Get(&out.normals, t);
    if (UsdAttribute a = mesh.GetFaceVertexIndicesAttr(); a && a.HasAuthoredValue())
        out.hasFaceVertexIndices = a.Get(&out.faceVertexIndices, t);
    if (UsdAttribute a = mesh.GetFaceVertexCountsAttr(); a && a.HasAuthoredValue())
        out.hasFaceVertexCounts = a.Get(&out.faceVertexCounts, t);
}

// ---------------------------------------------------------------------------
// ComputeMeshHash - cheap pre-filter combining the four arrays + presence
// ---------------------------------------------------------------------------
size_t ComputeMeshHash(const MeshArrays& m)
{
    size_t h = TfHash::Combine(
        m.hasPoints, m.hasNormals,
        m.hasFaceVertexIndices, m.hasFaceVertexCounts,
        m.points.size(), m.normals.size(),
        m.faceVertexIndices.size(), m.faceVertexCounts.size());

    if (m.hasPoints)            h = TfHash::Combine(h, m.points);
    if (m.hasNormals)           h = TfHash::Combine(h, m.normals);
    if (m.hasFaceVertexIndices) h = TfHash::Combine(h, m.faceVertexIndices);
    if (m.hasFaceVertexCounts)  h = TfHash::Combine(h, m.faceVertexCounts);
    return h;
}

// ---------------------------------------------------------------------------
// MeshArraysEqual - exact element-wise comparison (defends against collisions)
// ---------------------------------------------------------------------------
bool MeshArraysEqual(const MeshArrays& a, const MeshArrays& b)
{
    return a.hasPoints            == b.hasPoints
        && a.hasNormals           == b.hasNormals
        && a.hasFaceVertexIndices == b.hasFaceVertexIndices
        && a.hasFaceVertexCounts  == b.hasFaceVertexCounts
        && a.points               == b.points
        && a.normals              == b.normals
        && a.faceVertexIndices    == b.faceVertexIndices
        && a.faceVertexCounts     == b.faceVertexCounts;
}

// ---------------------------------------------------------------------------
// CollectIdenticalMeshCandidates - Xform (any prim) with exactly one Mesh child
// ---------------------------------------------------------------------------
std::vector<MeshCandidate> CollectIdenticalMeshCandidates(
    const UsdStageRefPtr&    stage,
    const InstancingOptions& opts)
{
    std::vector<MeshCandidate> candidates;

    for (const UsdPrim& prim : stage->Traverse())
    {
        if (prim.IsPseudoRoot() || prim.IsAbstract())
            continue;
        if (prim.IsInstance() || prim.IsInstanceable())
            continue;

        // Root-layer-only guard (unless the caller opted in).
        if (!opts.includeReferenced && !utils::IsAuthoredOnRootLayer(prim))
            continue;

        // Candidate wrapper: exactly one child, and that child IsA<Mesh>.
        UsdPrim onlyMeshChild;
        int     childCount = 0;
        for (const UsdPrim& child : prim.GetChildren())
        {
            ++childCount;
            if (childCount > 1)
                break;
            if (child.IsA<UsdGeomMesh>())
                onlyMeshChild = child;
        }
        if (childCount != 1 || !onlyMeshChild)
            continue;

        MeshCandidate cand;
        cand.xformPath    = prim.GetPath();
        cand.meshPrimPath = onlyMeshChild.GetPath();
        cand.meshName     = onlyMeshChild.GetName();
        ReadMeshArrays(UsdGeomMesh(onlyMeshChild), cand.arrays);
        cand.hash = ComputeMeshHash(cand.arrays);
        candidates.push_back(std::move(cand));
    }

    return candidates;
}

// ---------------------------------------------------------------------------
// EnsurePrototypeScope - create (once) the /__Prototypes__ scope
// ---------------------------------------------------------------------------
SdfPath EnsurePrototypeScope(const SdfLayerHandle& rootLayer)
{
    static const SdfPath kScope("/__Prototypes__");
    SdfPrimSpecHandle spec = rootLayer->GetPrimAtPath(kScope);
    if (!spec)
    {
        spec = SdfCreatePrimInLayer(rootLayer, kScope);
        if (spec)
        {
            // A Scope is non-renderable on its own.
            spec->SetSpecifier(SdfSpecifierDef);
            spec->SetTypeName(TfToken("Scope"));
        }
    }
    return kScope;
}

// ---------------------------------------------------------------------------
// AuthorPrototypeFromSeed
// ---------------------------------------------------------------------------
/**
 * @brief Copies the seed candidate's single Mesh child into
 *        `/__Prototypes__/Proto_<index>/<MeshName>` and marks the enclosing
 *        `Proto_<index>` prim as a `class`. The prototype holds the shared
 *        mesh; per-instance transforms remain on the Xform wrappers.
 *
 * @return the SdfPath of the authored `Proto_<index>` prototype, or an empty
 *         path on failure.
 */
SdfPath AuthorPrototypeFromSeed(const SdfLayerHandle& rootLayer,
                                const MeshCandidate&  seed,
                                const SdfPath&        scope,
                                int                   index)
{
    // Resolve a collision-free Proto_<n> name.
    SdfPath protoPath;
    for (int suffix = index; ; ++suffix)
    {
        const std::string name = "Proto_" + std::to_string(suffix);
        const SdfPath candidate = scope.AppendChild(TfToken(name));
        if (!rootLayer->GetPrimAtPath(candidate))
        {
            protoPath = candidate;
            break;
        }
    }

    SdfPrimSpecHandle protoSpec = SdfCreatePrimInLayer(rootLayer, protoPath);
    if (!protoSpec)
    {
        IDTX_LOG(IDTX_WARN,
            "  Failed to create prototype spec at '{}'", protoPath.GetString());
        return SdfPath();
    }
    protoSpec->SetSpecifier(SdfSpecifierClass);

    // Copy the seed Mesh child spec into the prototype under its own name.
    // Operate purely on the cached SdfPath / name - never on a (possibly stale)
    // live prim handle.
    const SdfPath seedMeshPath  = seed.meshPrimPath;
    const SdfPath protoMeshPath = protoPath.AppendChild(seed.meshName);
    if (!SdfCopySpec(rootLayer, seedMeshPath, rootLayer, protoMeshPath))
    {
        IDTX_LOG(IDTX_WARN,
            "  Failed to copy seed mesh '{}' -> prototype '{}'",
            seedMeshPath.GetString(), protoMeshPath.GetString());
        return SdfPath();
    }

    IDTX_LOG(IDTX_INFO,
        "  Authored prototype '{}' from seed '{}'",
        protoPath.GetString(), seedMeshPath.GetString());
    return protoPath;
}

// ---------------------------------------------------------------------------
// MakeInstanceOfPrototype
// ---------------------------------------------------------------------------
/**
 * @brief Strips the local Mesh child from @p member (keeping the Xform's own
 *        transform attributes), adds an `inherits` arc to @p prototypePath and
 *        sets `instanceable = true`.
 *
 * @note Everything is addressed by the candidate's *cached SdfPath* rather than
 *       by any live prim handle gathered during collection. The UsdPrim used
 *       for the composition-authoring calls (AddInherit / SetInstanceable) is
 *       re-fetched fresh from @p stage immediately before use, because prior
 *       edits in this pass may have recomposed the stage and invalidated any
 *       earlier handle to the same path.
 */
bool MakeInstanceOfPrototype(const UsdStageRefPtr& stage,
                             const SdfLayerHandle& rootLayer,
                             const MeshCandidate&  member,
                             const SdfPath&        prototypePath)
{
    // Remove the local Mesh child spec so the instance's content comes solely
    // from the inherited prototype (native instances cannot carry local
    // defining opinions below the instance root). All spec lookups go through
    // the stable SdfPath.
    SdfPrimSpecHandle xformSpec = rootLayer->GetPrimAtPath(member.xformPath);
    if (!xformSpec)
    {
        IDTX_LOG(IDTX_WARN,
            "  Instance spec '{}' not found on root layer - skipping",
            member.xformPath.GetString());
        return false;
    }

    SdfPrimSpecHandle meshChildSpec =
        rootLayer->GetPrimAtPath(member.meshPrimPath);
    if (meshChildSpec)
        xformSpec->RemoveNameChild(meshChildSpec);

    // Re-fetch a fresh prim by path: the RemoveNameChild above (and prototype
    // authoring earlier in this pass) can trigger recomposition, so any handle
    // captured during collection may now be stale.
    UsdPrim prim = stage->GetPrimAtPath(member.xformPath);
    if (!prim)
    {
        IDTX_LOG(IDTX_WARN,
            "  Instance prim '{}' no longer resolves after edits - skipping",
            member.xformPath.GetString());
        return false;
    }

    // Add the inherit arc to the prototype class.
    if (!prim.GetInherits().AddInherit(prototypePath))
    {
        IDTX_LOG(IDTX_WARN,
            "  Failed to add inherit '{}' on '{}'",
            prototypePath.GetString(), member.xformPath.GetString());
        return false;
    }

    if (!prim.SetInstanceable(true))
    {
        IDTX_LOG(IDTX_WARN,
            "  Failed to set instanceable=true on '{}'",
            member.xformPath.GetString());
        return false;
    }

    return true;
}

// ---------------------------------------------------------------------------
// ConvertIdenticalMeshes - new `identical-mesh` mode
// ---------------------------------------------------------------------------
void ConvertIdenticalMeshes(const UsdStageRefPtr&    stage,
                            const InstancingOptions& opts,
                            const std::string&       inputPath)
{
    SdfLayerHandle rootLayer = stage->GetRootLayer();

    // 1. Collect candidates in deterministic stage-traversal order.
    const std::vector<MeshCandidate> candidates =
        CollectIdenticalMeshCandidates(stage, opts);

    if (candidates.empty())
    {
        IDTX_LOG(IDTX_WARN,
            "  No identical-mesh candidates found in '{}'", inputPath);
        return;
    }

    // 2. Group by hash (pre-filter), then confirm exact equality within each
    //    bucket to form identity groups. Iteration order of `candidates` is
    //    preserved so the first occurrence seeds each group deterministically.
    std::unordered_map<size_t, std::vector<std::vector<int>>> buckets;
    for (int i = 0; i < static_cast<int>(candidates.size()); ++i)
    {
        auto& groups = buckets[candidates[i].hash];
        bool placed = false;
        for (auto& group : groups)
        {
            if (MeshArraysEqual(candidates[group.front()].arrays,
                                candidates[i].arrays))
            {
                group.push_back(i);
                placed = true;
                break;
            }
        }
        if (!placed)
            groups.push_back({i});
    }

    // 3. Flatten into a deterministic list of identity groups (size >= 2),
    //    ordered by the index of their seed (first member).
    std::vector<std::vector<int>> identityGroups;
    for (auto& [hash, groups] : buckets)
        for (auto& group : groups)
            if (group.size() >= 2)
                identityGroups.push_back(group);

    std::sort(identityGroups.begin(), identityGroups.end(),
              [](const std::vector<int>& a, const std::vector<int>& b) {
                  return a.front() < b.front();
              });

    if (identityGroups.empty())
    {
        IDTX_LOG(IDTX_INFO,
            "  No identical meshes found in '{}' (nothing to instance)",
            inputPath);
        return;
    }

    // 4. Author prototypes + instances.
    const SdfPath scope = EnsurePrototypeScope(rootLayer);

    int prototypeCount = 0;
    int instanceCount  = 0;

    for (const std::vector<int>& group : identityGroups)
    {
        const MeshCandidate& seed = candidates[group.front()];

        const SdfPath protoPath =
            AuthorPrototypeFromSeed(rootLayer, seed, scope, prototypeCount);
        if (protoPath.IsEmpty())
            continue;
        ++prototypeCount;

        for (int memberIdx : group)
        {
            if (MakeInstanceOfPrototype(stage, rootLayer,
                                        candidates[memberIdx], protoPath))
                ++instanceCount;
        }
    }

    IDTX_LOG(IDTX_INFO,
        "  identical-mesh: {} group(s), {} prototype(s), {} instance(s)",
        identityGroups.size(), prototypeCount, instanceCount);
}

// ---------------------------------------------------------------------------
// ExportInstancedStage - shared export/repackage tail for both modes
// ---------------------------------------------------------------------------
bool ExportInstancedStage(const UsdStageRefPtr& stage,
                          const std::string&    inputPath,
                          const std::string&    outputDir)
{
    const fs::path    inPath(inputPath);
    const std::string stem     = inPath.stem().string() + "_instanced";
    const std::string ext      = inPath.extension().string();
    std::string       lowerExt = ext;
    std::transform(lowerExt.begin(), lowerExt.end(), lowerExt.begin(),
                   ::tolower);

    // We only mutate the *root layer*, so exporting the root layer directly
    // preserves the original authoring structure (over/class/references
    // verbatim). Using stage->Export() would flatten the composed scene.
    if (lowerExt == ".usdz")
    {
        const fs::path outPath = fs::path(outputDir) / (stem + ".usdz");
        if (idtx::utils::RepackageUsdz(inputPath, stage, outPath.string()))
        {
            IDTX_LOG(IDTX_INFO, "  Exported to '{}'", outPath.string());
            return true;
        }

        IDTX_LOG(IDTX_WARN,
            "  usdz re-packaging failed - falling back to .usdc (referenced "
            "assets such as textures will not be included)");

        const fs::path fallbackPath = fs::path(outputDir) / (stem + ".usdc");
        if (!stage->GetRootLayer()->Export(fallbackPath.string()))
        {
            IDTX_LOG(IDTX_ERROR,
                "  Failed to export instanced stage to '{}'",
                fallbackPath.string());
            return false;
        }
        IDTX_LOG(IDTX_INFO, "  Exported to '{}'", fallbackPath.string());
        return true;
    }

    const fs::path outPath = fs::path(outputDir) / (stem + ext);
    if (!stage->GetRootLayer()->Export(outPath.string()))
    {
        IDTX_LOG(IDTX_ERROR,
            "  Failed to export instanced stage to '{}'", outPath.string());
        return false;
    }

    IDTX_LOG(IDTX_INFO, "  Exported to '{}'", outPath.string());
    return true;
}

// ---------------------------------------------------------------------------
// ProcessStage - open, dispatch on mode, export
// ---------------------------------------------------------------------------
bool ProcessStage(const std::string&       inputPath,
                  const std::string&       outputDir,
                  const InstancingOptions& opts,
                  bool                     dryRun)
{
    IDTX_LOG(IDTX_INFO, "Processing '{}'", inputPath);

    // Never load payload arcs - we do not want to follow them.
    UsdStageRefPtr stage = UsdStage::Open(inputPath, UsdStage::LoadNone);
    if (!stage)
    {
        IDTX_LOG(IDTX_ERROR, "  Failed to open USD stage '{}'", inputPath);
        return false;
    }

    switch (opts.mode)
    {
        case InstancingMode::Pseudo:
            ConvertPseudoInstances(stage, inputPath);
            break;
        case InstancingMode::IdenticalMesh:
            ConvertIdenticalMeshes(stage, opts, inputPath);
            break;
    }

    if (dryRun)
    {
        IDTX_LOG(IDTX_INFO, "  [dry-run] Skipping export");
        return true;
    }

    return ExportInstancedStage(stage, inputPath, outputDir);
}

} // anonymous namespace

// ===========================================================================
// Public API
// ===========================================================================

CLI::App* RegisterInstancingCommand(CLI::App& app, InstancingOptions& opts)
{
    auto* sub = app.add_subcommand("instancing",
        "Convert prims into native USD scenegraph instances\n"
        "  pseudo         - flip an 'over' prototype referenced by a\n"
        "                   pseudo-instance to 'class' and mark the\n"
        "                   pseudo-instance instanceable = true (default)\n"
        "  identical-mesh - de-duplicate identical Xform+Mesh subtrees into a\n"
        "                   shared prototype class under /__Prototypes__ and\n"
        "                   turn each match into a native instance");

    // Bind the --mode option through a callback rather than to a local string
    // variable. A previous implementation bound the option to a `std::string`
    // local that went out of scope when this function returned, leaving CLI11
    // with a dangling pointer that it wrote into during the (later) parse -
    // corrupting the stack. add_option_function<>() lets CLI11 own the parsed
    // value and only hands it to the callback, which updates the long-lived
    // `opts` reference.
    sub->add_option_function<std::string>(
           "--mode",
           [&opts](const std::string& value) {
               opts.mode = (value == "identical-mesh")
                               ? InstancingMode::IdenticalMesh
                               : InstancingMode::Pseudo;
           },
           "Instancing mode to use")
        ->type_name("pseudo|identical-mesh")
        ->check(CLI::IsMember({"pseudo", "identical-mesh"}))
        ->default_str("pseudo");

    sub->add_flag("--include-referenced", opts.includeReferenced,
        "Also consider prims that are brought into the stage via "
        "reference/payload arcs (default: only prims authored on the root "
        "layer are modified)");

    return sub;
}

int RunInstancingCommand(
    const std::vector<std::string>& inputFiles,
    const std::string&              outputDir,
    const InstancingOptions&        opts,
    bool                            dryRun)
{
    IDTX_LOG(IDTX_INFO, "instancing  (mode={})",
        opts.mode == InstancingMode::IdenticalMesh ? "identical-mesh"
                                                   : "pseudo");

    int failures = 0;
    for (const auto& inputPath : inputFiles)
    {
        if (!ProcessStage(inputPath, outputDir, opts, dryRun))
            ++failures;
    }

    if (failures > 0)
        IDTX_LOG(IDTX_ERROR,
            "{} file(s) failed instancing conversion", failures);

    return failures == 0 ? 0 : 1;
}

} // namespace idtx::commands
