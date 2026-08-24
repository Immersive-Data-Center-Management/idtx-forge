#pragma once

#include <functional>
#include <string>
#include <unordered_set>

#include <pxr/usd/sdf/layer.h>
#include <pxr/usd/sdf/path.h>
#include <pxr/usd/usd/prim.h>
#include <pxr/usd/usd/stage.h>
#include <pxr/usd/usd/primRange.h>
#include <pxr/usd/usdGeom/mesh.h>

#include <idtx/utils/Logger.h>


namespace idtx::utils
{
/**
 * Check if the GeomMesh orientation propert is authored as "leftHanded".
 * @param mesh The UsdGeomMesh prim to check.
 * @return 
 */
bool IsLeftHanded(const pxr::UsdGeomMesh& mesh);
    
/**
 * Check if the actual prim could be considered a pseude prim as it refers to some sort
 * of template prim for it's mesh instead of authoring its own data.
 * @param usdPrim The prim to check
 * @param prototypePath The path to the prim the one in <usdPrim> uses as "template"
 * @return 
 */
bool IsPseudoInstance(const pxr::UsdPrim& usdPrim, pxr::SdfPath* prototypePath);

// ---------------------------------------------------------------------------
// Reference/Payload arc authoring guard
// ---------------------------------------------------------------------------

/**
 * @brief Describes how a prim's opinions reach the composed stage, relative to
 *        the stage's root (currently edited) layer.
 */
enum class PrimAuthoringProvenance
{
    /// The prim is defined/authored (def or class) on the root layer.
    LocalOnRootLayer,
    /// The prim only carries a sparse `over` on the root layer; its defining
    /// opinions live elsewhere (e.g. behind a reference/payload).
    RootLayerOverOnly,
    /// The prim is pulled in by a non-ancestral reference arc.
    ViaReference,
    /// The prim is pulled in by a non-ancestral payload arc.
    ViaPayload,
    /// The prim is pulled in by some other arc (inherit/specialize/variant/
    /// ancestral) and has no local root-layer opinion.
    ViaOtherComposition,
};

/**
 * @brief Classify how @p prim reaches the stage relative to its root layer.
 *
 * @param prim              The composed prim to classify.
 * @param introducingLayer  Optional out-param: the layer that introduced the
 *                          strongest reference/payload arc (if any).
 */
PrimAuthoringProvenance ClassifyPrimProvenance(
    const pxr::UsdPrim&  prim,
    pxr::SdfLayerHandle* introducingLayer = nullptr);

/**
 * @brief Convenience predicate: true when @p prim has defining opinions
 *        authored directly on the stage's root layer (def/class), i.e. it is
 *        safe to edit without implicitly shadowing referenced content.
 */
bool IsAuthoredOnRootLayer(const pxr::UsdPrim& prim);

/**
 * @brief Convenience predicate: true when @p prim is brought into the stage by
 *        a non-ancestral reference or payload arc.
 */
bool IsBroughtInByReferenceOrPayload(const pxr::UsdPrim& prim);

// ---------------------------------------------------------------------------
// Shared prim-traversal helper (Proposal A: Options + Callback)
// ---------------------------------------------------------------------------

/**
 * @brief Controls how the shared traversal helper treats *instances*.
 *
 * From the traversal's point of view a "native" USD instance (a prim authored
 * as `instanceable = true` that pulls its geometry in through an
 * inherits/reference/payload arc) and a "pseudo" instance (a prim that refers
 * to a template/`over` prototype for its mesh instead of authoring its own
 * data) are treated identically: both are simply *an instance* pointing at
 * *a prototype*. This single policy therefore governs both kinds.
 */
enum class InstancePolicy
{
    /// reduce/triangulate/normals/... behaviour: skip the instance prim itself
    /// (native or pseudo) and instead process the (de-duplicated)
    /// prototype/template prim exactly once.
    SkipAndCollectPrototypes,
    /// instancing behaviour: hand the instance prim itself to the callback
    /// (no prototype collection).
    VisitInstances,
    /// treat instances like any other prim (no special handling).
    Ignore
};

/**
 * @brief Controls whether the traversal is allowed to hand prims that are not
 *        locally authored on the root layer to the visitor.
 */
enum class ReferencedPrimPolicy
{
    /// SAFE DEFAULT: only visit prims whose defining opinions are authored on
    /// the stage's root layer. Prims brought in via reference/payload (and
    /// prims that only carry a sparse `over` on the root layer) are skipped
    /// with an informative log line.
    SkipReferencedAndPayloaded,
    /// Explicit opt-in: also visit prims that reach the stage via a
    /// reference/payload arc. Use this only when the caller intends to author
    /// overrides on referenced content on purpose.
    IncludeReferencedAndPayloaded,
};

/**
 * @brief The kind of instancing a prim uses to pull its geometry from a
 *        prototype/template.
 */
enum class InstanceKind
{
    /// Not an instance at all.
    None,
    /// Native USD instance: authored as `instanceable = true` and pulling its
    /// prototype in through an inherits/reference/payload arc.
    Native,
    /// Pseudo instance: refers to a template/`over` prototype for its mesh
    /// instead of authoring its own geometry data.
    Pseudo,
};

/**
 * @brief Result of inspecting a prim for instancing.
 *
 * From the traversal's point of view a native and a pseudo instance are the
 * same thing: "an instance" that points at "a prototype". This struct captures
 * that unified view while still reporting which concrete @ref InstanceKind was
 * detected (useful for logging).
 */
struct InstanceInfo
{
    /// true when the prim is an instance of either kind.
    bool          isInstance = false;
    /// Which concrete kind of instance was detected.
    InstanceKind  kind = InstanceKind::None;
    /// The composed path of the prototype/template the instance targets.
    pxr::SdfPath  prototypePath;
    /// The layer that actually provides the prototype's defining opinions
    /// (only meaningful for native instances).
    pxr::SdfLayerHandle prototypeLayer;
    /// true when the prototype's defining opinions live inside the stage's
    /// root layer (i.e. the "current processed layer"); false when they come
    /// from a sublayer / referenced file. For pseudo instances the shared
    /// detector already restricts matches to the root layer, so this is true.
    bool          prototypeInRootLayer = false;
};

/**
 * @brief Result of inspecting a native (USD) instanceable prim.
 */
struct NativeInstanceInfo
{
    /// true when the prim is authored as `instanceable = true` and carries an
    /// inherits/reference/payload arc that points at an instance template.
    bool         isNativeInstance = false;
    /// The composed path of the instance template/prototype the arc targets.
    pxr::SdfPath prototypePath;
    /// The layer that actually provides the prototype's defining opinions.
    pxr::SdfLayerHandle prototypeLayer;
    /// true when the prototype's defining opinions live inside the stage's
    /// root layer (i.e. the "current processed layer"); false when they come
    /// from a sublayer / referenced file.
    bool         prototypeInRootLayer = false;
};

/**
 * @brief Detect whether @p prim is a native USD instance (authored as
 *        `instanceable = true`) that pulls its template in through an
 *        inherits/reference/payload arc, and report where the template lives.
 *
 * @param prim The prim to inspect.
 * @param info Out-param filled with the analysis result.
 * @return true when @p prim is such a native instance.
 */
bool AnalyzeNativeInstance(const pxr::UsdPrim& prim, NativeInstanceInfo* info);

/**
 * @brief Unified instance detector: reports whether @p prim is an instance of
 *        either kind (native or pseudo) and, if so, where its prototype lives.
 *
 * This is the detector the shared traversal uses so that native and pseudo
 * instances are handled identically ("skip the instance, collect the
 * prototype"). It first checks for a native instance and, failing that, for a
 * pseudo instance.
 *
 * @param prim The prim to inspect.
 * @param info Out-param filled with the analysis result.
 * @return true when @p prim is an instance of either kind.
 */
bool AnalyzeInstance(const pxr::UsdPrim& prim, InstanceInfo* info);

/**
 * @brief Behaviour knobs for TraverseMeshLike().
 */
struct TraversalOptions
{
    bool                 requireDefined      = true;
    bool                 skipAbstract        = true;
    bool                 skipNativeInstances = true;
    bool                 traverseAll         = false;   // TraverseAll() vs Traverse()
    /// Unified policy governing *both* native and pseudo instances. From the
    /// traversal's point of view both are simply "an instance" pointing at "a
    /// prototype" and are handled identically.
    InstancePolicy       instancePolicy      = InstancePolicy::SkipAndCollectPrototypes;
    // Safe default: leave referenced/payloaded prims untouched unless the
    // caller explicitly opts into editing them.
    ReferencedPrimPolicy referencedPolicy    = ReferencedPrimPolicy::SkipReferencedAndPayloaded;
};

/**
 * @brief Context describing *why* a prim is being handed to the visitor.
 *
 * Carries the prim's instancing state so the callback can react to it (e.g.
 * the `instancing` command needs to know it received an instance prim, and
 * which concrete kind, rather than an ordinary prim).
 */
struct PrimVisitContext
{
    /// true when the prim is a de-duplicated prototype/template prim processed
    /// after the main traversal (policy SkipAndCollectPrototypes).
    bool         isPrototype = false;
    /// true when the prim is an instance prim handed straight to the visitor
    /// because of InstancePolicy::VisitInstances.
    bool         isInstance = false;
    /// The concrete instance kind when @ref isInstance is true; InstanceKind::
    /// None otherwise.
    InstanceKind instanceKind = InstanceKind::None;
};

/**
 * @brief Per-prim visitor.
 *
 * The visitor returns true on success, false on failure (for failure
 * counting). @p ctx describes why the prim is being visited (prototype vs.
 * instance vs. ordinary prim) - see @ref PrimVisitContext.
 */
using PrimVisitor = std::function<bool(const pxr::UsdPrim& prim, const PrimVisitContext& ctx)>;

/**
 * @brief Result of a traversal: how many prims were processed successfully and
 *        how many the visitor reported as failed.
 */
struct TraversalResult
{
    int processed = 0;
    int failures  = 0;
};

/**
 * @brief Walk @p stage once, applying a consistent prim filter, unified
 *        instance handling (native and pseudo alike) and prototype
 *        de-duplication, invoking @p visit for every prim that should be
 *        processed.
 *
 * The pseudo-root is never handed to @p visit. Prototype/template prims for
 * skipped instances (policy SkipAndCollectPrototypes) are processed exactly
 * once after the main traversal.
 *
 * @tparam SchemaT The USD schema type each candidate prim must be an `IsA<>` of
 *                 (e.g. UsdGeomMesh, UsdGeomBoundable).
 */
template <typename SchemaT>
TraversalResult TraverseMeshLike(const pxr::UsdStageRefPtr& stage,
                                 const TraversalOptions&    opts,
                                 const PrimVisitor&         visit)
{
    TraversalResult result;
    if (!stage)
        return result;

    // Prototype/template prims (native prototypes are frequently `class`/`over`
    // prims outside the instance's local subtree; pseudo templates are usually
    // authored as `over`) are not visited by the outer Traverse(). Collect
    // their unique paths while skipping the instance prims so each prototype is
    // processed exactly once afterwards. Native and pseudo instances are
    // treated identically here: both are simply "an instance" pointing at "a
    // prototype".
    std::unordered_set<std::string> prototypePathsToCover;

    // When true, the standard prim filter skips native USD instances
    // (prim.IsInstance()). InstancePolicy::VisitInstances explicitly wants those
    // instance prims delivered to the visitor, so the filter's native-instance
    // check is bypassed for a prim the instance detector matched.
    auto passesFilter = [&](const pxr::UsdPrim& prim, bool allowNativeInstance) -> bool
    {
        if (!prim.IsA<SchemaT>())
            return false;
        if (opts.skipAbstract && prim.IsAbstract())
            return false;
        if (opts.requireDefined && !prim.IsDefined())
            return false;
        if (opts.skipNativeInstances && !allowNativeInstance && prim.IsInstance())
            return false;
        return true;
    };

    const auto range = opts.traverseAll ? stage->TraverseAll() : stage->Traverse();
    for (const pxr::UsdPrim& prim : range)
    {
        // Never invoke the visitor on the pseudo-root.
        if (prim.IsPseudoRoot())
            continue;

        // Records the instance state of `prim` when InstancePolicy::VisitInstances
        // decides to hand the instance prim straight to the visitor, so the
        // callback can be told it received an instance (and which kind).
        PrimVisitContext ctx;

        // Unified instance handling. Both native (USD `instanceable = true`)
        // and pseudo (template/`over`-referencing) instances are treated the
        // same: with SkipAndCollectPrototypes we skip the instance prim itself
        // and collect its (de-duplicated) prototype so the underlying geometry
        // is processed exactly once. This must run *before* passesFilter(),
        // because a native instanceable prim does not report its instanced
        // children (and is skipped by skipNativeInstances) - we still want to
        // reach its prototype.
        if (opts.instancePolicy != InstancePolicy::Ignore)
        {
            InstanceInfo instInfo;
            if (AnalyzeInstance(prim, &instInfo) && instInfo.isInstance)
            {
                const char* kindStr =
                    instInfo.kind == InstanceKind::Native ? "native" : "pseudo";

                if (opts.instancePolicy == InstancePolicy::SkipAndCollectPrototypes)
                {
                    if (instInfo.prototypePath.IsEmpty())
                    {
                        IDTX_LOGF(IDTX_INFO,
                            "Skipping {} instance '{}' - no resolvable prototype",
                            kindStr, prim.GetPath().GetString());
                        continue;
                    }

                    // A prototype whose defining opinions live outside the
                    // current processed (root) layer - e.g. in a sublayer or
                    // referenced file - is only collected when the caller
                    // explicitly opts into editing referenced/payloaded
                    // content.
                    if (!instInfo.prototypeInRootLayer
                        && opts.referencedPolicy
                               == ReferencedPrimPolicy::SkipReferencedAndPayloaded)
                    {
                        IDTX_LOGF(IDTX_INFO,
                            "Skipping {} instance '{}' - prototype '{}' lives "
                            "outside the current layer (use the explicit "
                            "include option to edit it)",
                            kindStr,
                            prim.GetPath().GetString(),
                            instInfo.prototypePath.GetString());
                        continue;
                    }

                    IDTX_LOGF(IDTX_INFO,
                        "Skipping {} instance '{}', collecting prototype '{}'",
                        kindStr,
                        prim.GetPath().GetString(),
                        instInfo.prototypePath.GetString());
                    prototypePathsToCover.insert(
                        instInfo.prototypePath.GetString());
                    continue;
                }

                // VisitInstances: fall through and hand the instance prim
                // itself to the visitor below. Flag it so the callback knows it
                // received an instance (and which kind) rather than an ordinary
                // prim.
                ctx.isInstance   = true;
                ctx.instanceKind = instInfo.kind;
                IDTX_LOGF(IDTX_INFO,
                    "{} instance '{}' handed to visitor (VisitInstances policy)",
                    kindStr, prim.GetPath().GetString());
            }
        }

        // VisitInstances explicitly wants instance prims (native ones included),
        // so bypass the filter's skipNativeInstances check for them.
        if (!passesFilter(prim, /*allowNativeInstance=*/ctx.isInstance))
            continue;

        // Reference/payload authoring guard: by default only prims whose
        // defining opinions are authored on the stage's root layer are handed
        // to the visitor. Prims that reach the stage via a reference/payload
        // arc (or that only carry a sparse `over` on the root layer) are left
        // untouched so we never implicitly shadow referenced content.
        if (opts.referencedPolicy == ReferencedPrimPolicy::SkipReferencedAndPayloaded)
        {
            pxr::SdfLayerHandle introducing;
            const PrimAuthoringProvenance prov =
                ClassifyPrimProvenance(prim, &introducing);

            if (prov != PrimAuthoringProvenance::LocalOnRootLayer)
            {
                const char* reason = "not authored on root layer";
                switch (prov)
                {
                    case PrimAuthoringProvenance::ViaReference:
                        reason = "brought in via 'references' arc"; break;
                    case PrimAuthoringProvenance::ViaPayload:
                        reason = "brought in via 'payload' arc"; break;
                    case PrimAuthoringProvenance::RootLayerOverOnly:
                        reason = "root layer only carries a sparse 'over'"; break;
                    case PrimAuthoringProvenance::ViaOtherComposition:
                        reason = "provided by inherit/specialize/variant/ancestral arc"; break;
                    default: break;
                }
                IDTX_LOGF(IDTX_INFO,
                    "Skipping '{}' - {} (use the explicit include option to edit it)",
                    prim.GetPath().GetString(), reason);
                continue;
            }
        }

        // ctx already carries any instance state set by the VisitInstances
        // branch above; ordinary prims see the default-constructed context.
        if (visit(prim, ctx))
            ++result.processed;
        else
            ++result.failures;
    }

    // Process the collected instance prototypes exactly once each. A prototype
    // prim is frequently a `class`/`over` living outside the instance's local
    // subtree; it may hold the geometry directly or contain matching
    // descendants (the prototype subtree is not reached by the outer
    // Traverse() because a native instanceable prim prunes its children).
    for (const std::string& prototypePathStr : prototypePathsToCover)
    {
        const pxr::SdfPath prototypePath(prototypePathStr);
        pxr::UsdPrim prototypePrim = stage->GetPrimAtPath(prototypePath);
        if (!prototypePrim)
        {
            IDTX_LOGF(IDTX_WARN,
                "  Instance prototype '{}' is not a valid target - skipping",
                prototypePathStr);
            continue;
        }

        // A prototype/template is frequently an `over` that is itself provided
        // by a reference; apply the same authoring guard here so it is only
        // processed when locally authored on the root layer (unless the caller
        // explicitly opted in).
        if (opts.referencedPolicy == ReferencedPrimPolicy::SkipReferencedAndPayloaded
            && !IsAuthoredOnRootLayer(prototypePrim))
        {
            IDTX_LOGF(IDTX_INFO,
                "Skipping prototype prim '{}' - not authored on root layer "
                "(use the explicit include option to edit it)",
                prototypePathStr);
            continue;
        }

        // Hand the prototype itself to the visitor when it matches the schema,
        // otherwise descend into it and process matching descendants.
        auto handlePrototypePrim = [&](const pxr::UsdPrim& p)
        {
            if (!p.IsA<SchemaT>())
                return;
            if (opts.skipAbstract && p.IsAbstract())
                return;
            IDTX_LOGF(IDTX_DEBUG,
                "  Processing instance prototype prim '{}'",
                p.GetPath().GetString());
            PrimVisitContext protoCtx;
            protoCtx.isPrototype = true;
            if (visit(p, protoCtx))
                ++result.processed;
            else
                ++result.failures;
        };

        handlePrototypePrim(prototypePrim);
        for (const pxr::UsdPrim& child : prototypePrim.GetDescendants())
            handlePrototypePrim(child);
    }

    return result;
}
}
