#include "utils/stageutils.h"

#include <pxr/usd/pcp/types.h>
#include <pxr/usd/sdf/primSpec.h>
#include <pxr/usd/usdGeom/tokens.h>
#include <pxr/usd/usd/primCompositionQuery.h>

namespace idtx::utils
{
    PXR_NAMESPACE_USING_DIRECTIVE

    PrimAuthoringProvenance ClassifyPrimProvenance(
        const UsdPrim& prim, SdfLayerHandle* introducingLayer)
    {
        const UsdStageRefPtr stage     = prim.GetStage();
        const SdfLayerHandle rootLayer = stage ? stage->GetRootLayer()
                                               : SdfLayerHandle();

        // 1) Inspect the composition arcs on *this* prim first. A non-ancestral
        //    reference/payload arc means the prim's defining geometry data is
        //    pulled in from another layer, even if the root layer carries a
        //    local `def`/`class` declaration that merely *hosts* that arc
        //    (e.g. `def "Mesh" (references = @proto.usda@</Proto>) {}`). Such a
        //    prim has not been "authored at the current layer" in any way that
        //    is safe to mutate, so it must be classified as referenced/payloaded
        //    regardless of the root-layer specifier.
        //
        //    An arc introduced by an ancestor (IsAncestral) is intentionally
        //    skipped: it means a parent prim was referenced and this child came
        //    along for the ride - handled below as ViaOtherComposition when it
        //    has no local defining opinion of its own.
        UsdPrimCompositionQuery query(prim);
        for (const UsdPrimCompositionQueryArc& arc : query.GetCompositionArcs())
        {
            if (arc.IsAncestral())
                continue;  // arc comes from an ancestor, not this prim

            const PcpArcType type = arc.GetArcType();
            if (type == PcpArcTypeReference || type == PcpArcTypePayload)
            {
                if (introducingLayer)
                    *introducingLayer = arc.GetIntroducingLayer();
                return type == PcpArcTypeReference
                           ? PrimAuthoringProvenance::ViaReference
                           : PrimAuthoringProvenance::ViaPayload;
            }
            if (type == PcpArcTypeRoot)
                break;  // reached the prim's own root opinions; stop here
        }

        // 2) No direct reference/payload arc: decide based on the root-layer
        //    spec. A def/class means the prim is locally authored here; a bare
        //    `over` only carries a sparse override on top of data defined
        //    elsewhere (typically behind an ancestral reference/payload).
        bool rootLayerDefines  = false;
        bool rootLayerOverOnly = false;
        for (const SdfPrimSpecHandle& spec : prim.GetPrimStack())
        {
            if (!spec || spec->GetLayer() != rootLayer)
                continue;
            if (spec->GetSpecifier() == SdfSpecifierOver)
                rootLayerOverOnly = true;   // sparse override only (so far)
            else
                rootLayerDefines = true;    // def or class => local definition
        }
        if (rootLayerDefines)
            return PrimAuthoringProvenance::LocalOnRootLayer;

        if (rootLayerOverOnly)
            return PrimAuthoringProvenance::RootLayerOverOnly;

        return PrimAuthoringProvenance::ViaOtherComposition;
    }

    bool IsAuthoredOnRootLayer(const UsdPrim& prim)
    {
        return ClassifyPrimProvenance(prim)
               == PrimAuthoringProvenance::LocalOnRootLayer;
    }

    bool IsBroughtInByReferenceOrPayload(const UsdPrim& prim)
    {
        const PrimAuthoringProvenance p = ClassifyPrimProvenance(prim);
        return p == PrimAuthoringProvenance::ViaReference
            || p == PrimAuthoringProvenance::ViaPayload;
    }

    bool AnalyzeNativeInstance(const UsdPrim& prim, NativeInstanceInfo* info)
    {
        if (info)
            *info = NativeInstanceInfo{};

        if (!prim || !info)
            return false;

        // A native instance is explicitly authored as `instanceable = true`.
        // We deliberately check the authored metadata rather than IsInstance()
        // so we also handle instanceable prims whose prototype could not be
        // composed yet.
        if (!prim.IsInstanceable())
            return false;

        const UsdStageRefPtr stage    = prim.GetStage();
        const SdfLayerHandle rootLayer = stage ? stage->GetRootLayer()
                                               : SdfLayerHandle();

        // Walk the composition arcs (strongest first) and pick the first
        // non-ancestral inherits/reference/payload arc introduced directly on
        // this prim: that arc points at the instance template.
        UsdPrimCompositionQuery query(prim);
        for (const UsdPrimCompositionQueryArc& arc : query.GetCompositionArcs())
        {
            if (arc.IsAncestral())
                continue;

            const PcpArcType type = arc.GetArcType();
            if (type == PcpArcTypeReference ||
                type == PcpArcTypePayload   ||
                type == PcpArcTypeInherit)
            {
                info->isNativeInstance = true;
                info->prototypePath    = arc.GetTargetPrimPath();
                info->prototypeLayer   = arc.GetTargetLayer();

                // The prototype lives inside the "current processed layer" only
                // when its defining opinions are provided by the stage's root
                // layer. A reference/payload that targets an external file, or
                // an arc whose target opinions come from a sublayer, is treated
                // as living outside the current layer.
                info->prototypeInRootLayer =
                    rootLayer && info->prototypeLayer == rootLayer;

                return true;
            }
            if (type == PcpArcTypeRoot)
                break;  // reached this prim's own root opinions; stop
        }

        return false;
    }

    bool AnalyzeInstance(const UsdPrim& prim, InstanceInfo* info)
    {
        if (info)
            *info = InstanceInfo{};

        if (!prim || !info)
            return false;

        // 1) Native USD instance (instanceable = true + inherits/reference/
        //    payload arc). Reuse the dedicated analyzer and translate its
        //    result into the unified InstanceInfo.
        NativeInstanceInfo nativeInfo;
        if (AnalyzeNativeInstance(prim, &nativeInfo) && nativeInfo.isNativeInstance)
        {
            info->isInstance           = true;
            info->kind                 = InstanceKind::Native;
            info->prototypePath        = nativeInfo.prototypePath;
            info->prototypeLayer       = nativeInfo.prototypeLayer;
            info->prototypeInRootLayer = nativeInfo.prototypeInRootLayer;
            return true;
        }

        // 2) Pseudo instance (refers to a template/`over` prototype for its
        //    mesh). The shared detector only matches templates that live in the
        //    stage's root layer, so from the traversal's point of view such a
        //    prototype is always "in the current layer".
        SdfPath templatePath;
        if (IsPseudoInstance(prim, &templatePath))
        {
            info->isInstance           = true;
            info->kind                 = InstanceKind::Pseudo;
            info->prototypePath        = templatePath;
            info->prototypeLayer       = prim.GetStage()
                                             ? prim.GetStage()->GetRootLayer()
                                             : SdfLayerHandle();
            info->prototypeInRootLayer = true;
            return true;
        }

        return false;
    }

    bool IsLeftHanded(const pxr::UsdGeomMesh& mesh)
    {
        TfToken orientation;
        if (mesh.GetOrientationAttr().Get(&orientation))
            return orientation == UsdGeomTokens->leftHanded;
        return false; // rightHanded default
    }

    bool IsPseudoInstance(const UsdPrim& usdPrim, SdfPath* prototypePath)
    {
        // if it is an actual real instance, it's not a pseudo one
        if (usdPrim.IsInstanceable() || usdPrim.IsInstance())
            return false;

        // if the prim has authored attributes that would make it impossible to be
        // treated as an instance (locally authored mesh data)
        const auto primStack = usdPrim.GetPrimStack();
        if (primStack.empty())
            return false;

        SdfPrimSpecHandle prim_spec = primStack.at(0);
        if (!prim_spec)
            return false;

        const SdfAttributeSpecView spec_attributes = prim_spec->GetAttributes();
        if (spec_attributes.has(UsdGeomTokens->points) ||
            spec_attributes.has(UsdGeomTokens->normals) ||
            spec_attributes.has(UsdGeomTokens->faceVertexIndices) ||
            spec_attributes.has(UsdGeomTokens->faceVertexCounts))
            return false;

        // to analyze further we need the composition arcs paying into this one
        // from strongest to weakest
        UsdPrimCompositionQuery composition_query(usdPrim);
        for (const UsdPrimCompositionQueryArc& composition_arc :
             composition_query.GetCompositionArcs())
        {
            // Skip arcs that are inherited via an ancestor prim - only arcs
            // introduced directly on this prim indicate that *this* prim (and
            // not one of its ancestors) is the pseudo-instance root.
            if (composition_arc.IsAncestral())
                continue;

            if (composition_arc.GetArcType() == PcpArcTypeReference ||
                composition_arc.GetArcType() == PcpArcTypeInherit)
            {
                *prototypePath = composition_arc.GetTargetPrimPath();
                SdfLayerHandle prototype_layer = composition_arc.GetTargetLayer();
                SdfLayerHandle stage_layer     = usdPrim.GetStage()->GetRootLayer();

                // If the referenced path is absolute and the prototype layer is
                // the same as the prim's layer (i.e. in the same file).
                if (prototypePath->IsAbsolutePath() &&
                    prototype_layer == stage_layer)
                {
                    // Check the prototype prim spec: if declared as "over" it is
                    // usually skipped during traversal and does not render on its
                    // own, identifying it as a pseudo-instancing prototype.
                    if (stage_layer->GetSpecType(*prototypePath) == SdfSpecTypePrim)
                    {
                        SdfPrimSpecHandle prototype_spec =
                            stage_layer->GetPrimAtPath(*prototypePath);
                        if (prototype_spec &&
                            prototype_spec->GetSpecifier() == SdfSpecifierOver)
                        {
                            return true;
                        }
                        if (prototype_spec)
                        {
                            SdfPrimSpecHandle parent_spec =
                                prototype_spec->GetNameParent();
                            if (parent_spec &&
                                parent_spec->GetSpecifier() == SdfSpecifierOver)
                            {
                                return true;
                            }
                        }
                    }
                }
                // only check the strongest composition layer reference. If a
                // reference comes from weaker layers it's very unlikely that we
                // have a pseudo-instancing scenario.
                break;
            }
        }

        return false;
    }
}