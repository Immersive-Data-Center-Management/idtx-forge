/**
 * @file test_stage_traversal.cpp
 * @brief Tests for the shared prim-traversal helper (idtx::utils::TraverseMeshLike)
 *        and the supporting classification helpers in utils/stageutils.h.
 *
 * All stages are loaded from the on-disk `.usda` fixtures under tests/data/
 * (copied next to the test executable by the build). See
 * tests/support/StageFixtures.h for the lookup logic.
 */

#include "thirdparty/doctest/doctest.h"

#include <ostream>
#include <string>
#include <vector>

#include <pxr/usd/sdf/path.h>
#include <pxr/usd/usd/prim.h>
#include <pxr/usd/usd/stage.h>
#include <pxr/usd/usdGeom/imageable.h>
#include <pxr/usd/usdGeom/mesh.h>

#include "utils/stageutils.h"
#include "support/StageFixtures.h"

using namespace pxr;
using namespace idtx::utils;
using idtx::tests::OpenDataStage;

// doctest needs to stringify values used in CHECK(a == b). Provide ADL-visible
// ostream operators for the scoped enums so failing assertions print something
// readable instead of failing to compile.
namespace idtx::utils {

inline std::ostream& operator<<(std::ostream& os, PrimAuthoringProvenance p)
{
    return os << "PrimAuthoringProvenance(" << static_cast<int>(p) << ")";
}

inline std::ostream& operator<<(std::ostream& os, InstanceKind k)
{
    return os << "InstanceKind(" << static_cast<int>(k) << ")";
}

} // namespace idtx::utils

namespace {

/// Records every prim path handed to the visitor for easy assertions.
struct VisitRecorder
{
    std::vector<std::string>      visited;
    std::vector<PrimVisitContext> contexts;

    PrimVisitor visitor()
    {
        return [this](const UsdPrim& prim, const PrimVisitContext& ctx) {
            visited.push_back(prim.GetPath().GetString());
            contexts.push_back(ctx);
            return true;
        };
    }

    bool sawPath(const std::string& p) const
    {
        for (const auto& v : visited)
            if (v == p) return true;
        return false;
    }

    size_t count() const { return visited.size(); }
};

} // namespace

TEST_CASE("TraverseMeshLike: null stage yields empty result")
{
    UsdStageRefPtr stage; // null
    VisitRecorder rec;
    TraversalOptions opts;
    const TraversalResult res =
        TraverseMeshLike<UsdGeomMesh>(stage, opts, rec.visitor());
    CHECK(res.processed == 0);
    CHECK(res.failures == 0);
    CHECK(rec.count() == 0);
}

TEST_CASE("TraverseMeshLike: visits every mesh, never the pseudo-root or xforms")
{
    UsdStageRefPtr stage = OpenDataStage("two_meshes.usda");
    REQUIRE(stage);

    VisitRecorder rec;
    TraversalOptions opts;
    const TraversalResult res =
        TraverseMeshLike<UsdGeomMesh>(stage, opts, rec.visitor());

    CHECK(res.processed == 2);
    CHECK(res.failures == 0);
    CHECK(rec.sawPath("/World/MeshA"));
    CHECK(rec.sawPath("/World/MeshB"));
    CHECK_FALSE(rec.sawPath("/World")); // Xform, not a Mesh
    CHECK_FALSE(rec.sawPath("/"));      // pseudo-root never visited
}

TEST_CASE("TraverseMeshLike: schema filter selects only matching prims")
{
    UsdStageRefPtr stage = OpenDataStage("mesh_and_scope.usda");
    REQUIRE(stage);

    SUBCASE("UsdGeomMesh only matches the mesh")
    {
        VisitRecorder rec;
        TraversalOptions opts;
        const TraversalResult res =
            TraverseMeshLike<UsdGeomMesh>(stage, opts, rec.visitor());
        CHECK(res.processed == 1);
        CHECK(rec.sawPath("/World/Mesh"));
        CHECK_FALSE(rec.sawPath("/World/Scope"));
    }

    SUBCASE("UsdGeomImageable matches mesh, scope and xform")
    {
        VisitRecorder rec;
        TraversalOptions opts;
        const TraversalResult res =
            TraverseMeshLike<UsdGeomImageable>(stage, opts, rec.visitor());
        CHECK(res.processed == 3);
        CHECK(rec.sawPath("/World"));
        CHECK(rec.sawPath("/World/Mesh"));
        CHECK(rec.sawPath("/World/Scope"));
    }
}

TEST_CASE("TraverseMeshLike: reports visitor failures")
{
    UsdStageRefPtr stage = OpenDataStage("two_meshes.usda");
    REQUIRE(stage);

    int calls = 0;
    auto visitor = [&](const UsdPrim& prim, const PrimVisitContext&) {
        ++calls;
        return prim.GetName() != TfToken("MeshA"); // fail on MeshA
    };

    TraversalOptions opts;
    const TraversalResult res =
        TraverseMeshLike<UsdGeomMesh>(stage, opts, visitor);

    CHECK(calls == 2);
    CHECK(res.processed == 1);
    CHECK(res.failures == 1);
}

TEST_CASE("TraverseMeshLike: requireDefined skips undefined (over-only) prims")
{
    UsdStageRefPtr stage = OpenDataStage("over_only.usda");
    REQUIRE(stage);

    VisitRecorder rec;
    TraversalOptions opts;
    opts.requireDefined = true;
    // Include referenced/other so requireDefined is the deciding filter.
    opts.referencedPolicy = ReferencedPrimPolicy::IncludeReferencedAndPayloaded;
    TraverseMeshLike<UsdGeomMesh>(stage, opts, rec.visitor());

    CHECK(rec.sawPath("/World/Defined"));
    CHECK_FALSE(rec.sawPath("/World/OverOnly"));
}

TEST_CASE("TraverseMeshLike: referenced-prim policy guards external opinions")
{
    UsdStageRefPtr stage = OpenDataStage("referenced_stage.usda");
    REQUIRE(stage);

    SUBCASE("SkipReferencedAndPayloaded (default) skips the referenced prim")
    {
        VisitRecorder rec;
        TraversalOptions opts;
        opts.referencedPolicy =
            ReferencedPrimPolicy::SkipReferencedAndPayloaded;
        TraverseMeshLike<UsdGeomMesh>(stage, opts, rec.visitor());
        CHECK(rec.sawPath("/World/Local"));
        CHECK_FALSE(rec.sawPath("/World/Ref"));
    }

    SUBCASE("IncludeReferencedAndPayloaded also visits the referenced prim")
    {
        VisitRecorder rec;
        TraversalOptions opts;
        opts.referencedPolicy =
            ReferencedPrimPolicy::IncludeReferencedAndPayloaded;
        TraverseMeshLike<UsdGeomMesh>(stage, opts, rec.visitor());
        CHECK(rec.sawPath("/World/Local"));
        CHECK(rec.sawPath("/World/Ref"));
    }
}

// ---------------------------------------------------------------------------
// Classification helpers
// ---------------------------------------------------------------------------

TEST_CASE("ClassifyPrimProvenance: local def on root layer")
{
    UsdStageRefPtr stage = OpenDataStage("single_quad.usda");
    REQUIRE(stage);
    UsdPrim prim = stage->GetPrimAtPath(SdfPath("/World/Mesh"));
    REQUIRE(prim);

    CHECK(ClassifyPrimProvenance(prim) ==
          PrimAuthoringProvenance::LocalOnRootLayer);
    CHECK(IsAuthoredOnRootLayer(prim));
    CHECK_FALSE(IsBroughtInByReferenceOrPayload(prim));
}

TEST_CASE("ClassifyPrimProvenance: over hosting a reference is not local-editable")
{
    // A prim whose only root-layer spec is a sparse `over` that hosts a
    // reference arc is classified RootLayerOverOnly: its defining opinions live
    // behind the reference, so it is NOT considered locally authored and the
    // traversal guard leaves it untouched by default. (A def-hosted reference,
    // by contrast, is treated as LocalOnRootLayer - see the note in the
    // reference-arc guard proposal.)
    UsdStageRefPtr stage = OpenDataStage("referenced_stage.usda");
    REQUIRE(stage);
    UsdPrim prim = stage->GetPrimAtPath(SdfPath("/World/Ref"));
    REQUIRE(prim);

    CHECK(ClassifyPrimProvenance(prim) ==
          PrimAuthoringProvenance::RootLayerOverOnly);
    // The key guard contract: it is not editable-as-local, so it is skipped by
    // the default SkipReferencedAndPayloaded policy.
    CHECK_FALSE(IsAuthoredOnRootLayer(prim));
}

TEST_CASE("ClassifyPrimProvenance: over hosting a payload is not local-editable")
{
    UsdStageRefPtr stage =
        UsdStage::Open(idtx::tests::DataFile("payload_stage.usda"),
                       UsdStage::LoadAll);
    REQUIRE(stage);
    UsdPrim prim = stage->GetPrimAtPath(SdfPath("/World/Pay"));
    REQUIRE(prim);

    CHECK(ClassifyPrimProvenance(prim) ==
          PrimAuthoringProvenance::RootLayerOverOnly);
    CHECK_FALSE(IsAuthoredOnRootLayer(prim));
}

TEST_CASE("ClassifyPrimProvenance: sparse over on root layer -> RootLayerOverOnly")
{
    UsdStageRefPtr stage = OpenDataStage("over_only.usda");
    REQUIRE(stage);
    UsdPrim over = stage->GetPrimAtPath(SdfPath("/OverPrim"));
    REQUIRE(over);

    CHECK(ClassifyPrimProvenance(over) ==
          PrimAuthoringProvenance::RootLayerOverOnly);
    CHECK_FALSE(IsAuthoredOnRootLayer(over));
    CHECK_FALSE(IsBroughtInByReferenceOrPayload(over));
}

TEST_CASE("AnalyzeInstance: ordinary prim is not an instance")
{
    UsdStageRefPtr stage = OpenDataStage("single_quad.usda");
    REQUIRE(stage);
    UsdPrim prim = stage->GetPrimAtPath(SdfPath("/World/Mesh"));
    REQUIRE(prim);

    InstanceInfo info;
    CHECK_FALSE(AnalyzeInstance(prim, &info));
    CHECK_FALSE(info.isInstance);
    CHECK(info.kind == InstanceKind::None);
}

TEST_CASE("IsPseudoInstance: prim with local mesh data is not a pseudo-instance")
{
    UsdStageRefPtr stage = OpenDataStage("single_quad.usda");
    REQUIRE(stage);
    UsdPrim prim = stage->GetPrimAtPath(SdfPath("/World/Mesh"));
    REQUIRE(prim);

    SdfPath protoPath;
    CHECK_FALSE(IsPseudoInstance(prim, &protoPath));
}

TEST_CASE("IsPseudoInstance: references an 'over' prototype in the same layer")
{
    UsdStageRefPtr stage = OpenDataStage("pseudo_instance.usda");
    REQUIRE(stage);
    UsdPrim inst = stage->GetPrimAtPath(SdfPath("/World/InstA"));
    REQUIRE(inst);

    SdfPath protoPath;
    const bool isPseudo = IsPseudoInstance(inst, &protoPath);
    CHECK(isPseudo);
    if (isPseudo)
        CHECK(protoPath == SdfPath("/Proto"));

    // Through the unified detector it reports as a Pseudo instance.
    InstanceInfo info;
    CHECK(AnalyzeInstance(inst, &info));
    CHECK(info.isInstance);
    CHECK(info.kind == InstanceKind::Pseudo);
    CHECK(info.prototypeInRootLayer);
}

TEST_CASE("TraverseMeshLike: pseudo-instance is skipped, prototype processed once")
{
    UsdStageRefPtr stage = OpenDataStage("pseudo_instance.usda");
    REQUIRE(stage);

    VisitRecorder rec;
    TraversalOptions opts;
    opts.instancePolicy = InstancePolicy::SkipAndCollectPrototypes;
    // The prototype lives on the root layer, so it is eligible to be collected
    // and processed.
    opts.referencedPolicy = ReferencedPrimPolicy::IncludeReferencedAndPayloaded;
    const TraversalResult res =
        TraverseMeshLike<UsdGeomMesh>(stage, opts, rec.visitor());

    // The Mesh-typed prototype '/Proto' must be processed exactly once even
    // though two pseudo-instances reference it. The instance prims themselves
    // ('/World/InstA', '/World/InstB') are skipped in favour of the prototype.
    int protoHits = 0;
    for (const auto& p : rec.visited)
        if (p == "/Proto") ++protoHits;

    CHECK(protoHits == 1);
    CHECK(res.processed >= 1);
    CHECK_FALSE(rec.sawPath("/World/InstA"));
    CHECK_FALSE(rec.sawPath("/World/InstB"));
    // The prototype hand-off flags the prim as a prototype in its context.
    bool sawPrototypeCtx = false;
    for (const auto& ctx : rec.contexts)
        if (ctx.isPrototype) sawPrototypeCtx = true;
    CHECK(sawPrototypeCtx);
}

TEST_CASE("TraverseMeshLike: VisitInstances hands instance prims to the visitor")
{
    UsdStageRefPtr stage = OpenDataStage("pseudo_instance_mesh.usda");
    REQUIRE(stage);

    VisitRecorder rec;
    TraversalOptions opts;
    opts.instancePolicy   = InstancePolicy::VisitInstances;
    opts.referencedPolicy = ReferencedPrimPolicy::IncludeReferencedAndPayloaded;
    TraverseMeshLike<UsdGeomMesh>(stage, opts, rec.visitor());

    CHECK(rec.sawPath("/Inst"));
    bool sawInstanceCtx = false;
    for (const auto& ctx : rec.contexts)
        if (ctx.isInstance) sawInstanceCtx = true;
    CHECK(sawInstanceCtx);
}
