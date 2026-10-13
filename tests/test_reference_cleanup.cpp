/**
 * @file test_reference_cleanup.cpp
 * @brief Unit and behavioural tests for the dangling-arc cleanup feature.
 *
 * These tests author tiny `.usda` layers in a scratch directory that carry a
 * mix of arcs:
 *   - a reference and a payload to anonymous layer identifiers (always dead),
 *   - a reference and a sublayer to a non-existent on-disk path (unresolvable),
 *   - an internal (self) reference that must always be kept.
 * and then drive:
 *   - idtx::utils::FindDanglingArcs        (pure detection)
 *   - idtx::utils::RemoveDanglingArcs      (in-memory removal, incl. --anon-only)
 *   - idtx::utils::CleanupLayerFileInPlace (in-place rewrite, incl. dry-run)
 *   - RunCleanRefsCommand                  (end-to-end)
 */

#include "thirdparty/doctest/doctest.h"

#include <filesystem>
#include <fstream>
#include <string>
#include <vector>

#include <pxr/base/vt/array.h>
#include <pxr/usd/ar/resolverContext.h>
#include <pxr/usd/sdf/assetPath.h>
#include <pxr/usd/sdf/attributeSpec.h>
#include <pxr/usd/sdf/layer.h>
#include <pxr/usd/sdf/path.h>
#include <pxr/usd/sdf/primSpec.h>

#include "commands/cleanrefs.h"
#include "utils/ReferenceCleanup.h"

#include "support/StageFixtures.h"

namespace fs = std::filesystem;
using namespace pxr;
using namespace idtx::commands;
using idtx::tests::TempDir;
using idtx::utils::ArcKind;
using idtx::utils::CleanRefsResult;
using idtx::utils::DanglingArc;
using idtx::utils::DanglingReason;
using idtx::utils::FindDanglingArcs;
using idtx::utils::RemoveDanglingArcs;

namespace {

// A layer with every category of arc we care about:
//   /AnonRef     : reference  -> anonymous identifier   (dangling, anonymous)
//   /AnonPayload : payload    -> anonymous identifier   (dangling, anonymous)
//   /MissingRef  : reference  -> non-existent file       (dangling, unresolvable)
//   /SelfRef     : reference  -> internal prim </Valid>  (healthy, must be kept)
// plus a sublayer to a non-existent file                 (dangling, unresolvable)
constexpr const char* kSceneBody = R"usda(#usda 1.0
(
    subLayers = [
        @./missing_sublayer.usd@
    ]
)

def "Valid"
{
}

def "AnonRef" (
    prepend references = @anon:0xdeadbeef:.usda@
)
{
}

def "AnonPayload" (
    prepend payload = @anon:0xcafef00d:.usda@
)
{
}

def "MissingRef" (
    prepend references = @./does_not_exist.usd@
)
{
}

def "SelfRef" (
    prepend references = </Valid>
)
{
}
)usda";

// Write kSceneBody to `path`.
void WriteScene(const std::string& path)
{
    std::ofstream ofs(path, std::ios::binary | std::ios::trunc);
    REQUIRE(ofs);
    ofs << kSceneBody;
    ofs.close();
}

// Count arcs by reason.
void CountByReason(const std::vector<DanglingArc>& arcs, int& anon, int& unresolvable)
{
    anon = unresolvable = 0;
    for (const DanglingArc& a : arcs)
    {
        if (a.reason == DanglingReason::AnonymousLayer)       ++anon;
        else if (a.reason == DanglingReason::Unresolvable)    ++unresolvable;
    }
}

} // namespace

TEST_CASE("FindDanglingArcs detects anonymous + unresolvable arcs and keeps internal refs")
{
    TempDir tmp;
    const std::string p = tmp.file("scene.usda");
    WriteScene(p);

    SdfLayerRefPtr layer = SdfLayer::FindOrOpen(p);
    REQUIRE(layer);

    const std::vector<DanglingArc> arcs =
        FindDanglingArcs(layer, ArResolverContext());

    // 2 anonymous (reference + payload) and 2 unresolvable (reference + sublayer).
    CHECK(arcs.size() == 4);
    int anon = 0, unresolvable = 0;
    CountByReason(arcs, anon, unresolvable);
    CHECK(anon == 2);
    CHECK(unresolvable == 2);

    // The internal </Valid> reference must never be reported as dangling.
    for (const DanglingArc& a : arcs)
        CHECK(a.primPath.GetString() != "/SelfRef");
}

TEST_CASE("RemoveDanglingArcs honours --anon-only")
{
    TempDir tmp;
    const std::string p = tmp.file("scene.usda");
    WriteScene(p);

    SdfLayerRefPtr layer = SdfLayer::FindOrOpen(p);
    REQUIRE(layer);

    SUBCASE("anon-only removes just the anonymous arcs")
    {
        const std::vector<DanglingArc> arcs =
            FindDanglingArcs(layer, ArResolverContext());
        const int removed = RemoveDanglingArcs(layer, arcs, /*anonOnly=*/true);
        CHECK(removed == 2);

        // The two unresolvable arcs remain.
        int anon = 0, unresolvable = 0;
        CountByReason(FindDanglingArcs(layer, ArResolverContext()), anon, unresolvable);
        CHECK(anon == 0);
        CHECK(unresolvable == 2);
    }

    SUBCASE("default removes every dangling arc but keeps the internal reference")
    {
        const std::vector<DanglingArc> arcs =
            FindDanglingArcs(layer, ArResolverContext());
        const int removed = RemoveDanglingArcs(layer, arcs, /*anonOnly=*/false);
        CHECK(removed == 4);

        CHECK(FindDanglingArcs(layer, ArResolverContext()).empty());

        // The healthy internal reference on /SelfRef is untouched.
        SdfPrimSpecHandle self = layer->GetPrimAtPath(SdfPath("/SelfRef"));
        REQUIRE(self);
        CHECK(self->GetReferenceList().GetAppliedItems().size() == 1);
    }
}

TEST_CASE("CleanupLayerFileInPlace dry-run reports but does not modify the file")
{
    TempDir tmp;
    const std::string p = tmp.file("scene.usda");
    WriteScene(p);

    const CleanRefsResult res = idtx::utils::CleanupLayerFileInPlace(
        p, /*anonOnly=*/false, /*dryRun=*/true, ArResolverContext());
    CHECK(res.ok);
    CHECK(res.found == 4);
    CHECK(res.removed == 0);

    // The file on disk is untouched: a fresh open still sees all four arcs.
    SdfLayerRefPtr layer = SdfLayer::FindOrOpen(p);
    REQUIRE(layer);
    CHECK(FindDanglingArcs(layer, ArResolverContext()).size() == 4);
}

TEST_CASE("CleanupLayerFileInPlace removes dangling arcs and rewrites the file")
{
    TempDir tmp;
    const std::string p = tmp.file("scene.usda");
    WriteScene(p);

    const CleanRefsResult res = idtx::utils::CleanupLayerFileInPlace(
        p, /*anonOnly=*/false, /*dryRun=*/false, ArResolverContext());
    CHECK(res.ok);
    CHECK(res.found == 4);
    CHECK(res.removed == 4);

    // A fresh open re-reads the rewritten file: no dangling arcs, internal kept.
    SdfLayerRefPtr layer = SdfLayer::FindOrOpen(p);
    REQUIRE(layer);
    CHECK(FindDanglingArcs(layer, ArResolverContext()).empty());
    SdfPrimSpecHandle self = layer->GetPrimAtPath(SdfPath("/SelfRef"));
    REQUIRE(self);
    CHECK(self->GetReferenceList().GetAppliedItems().size() == 1);

    // No temp file left behind next to the original.
    CHECK_FALSE(fs::exists(p + ".idtxclean.usda"));
}

TEST_CASE("RunCleanRefsCommand: dry-run leaves the file, live run cleans it")
{
    TempDir tmp;
    const std::string p = tmp.file("scene.usda");
    WriteScene(p);

    CleanRefsOptions opts;
    opts.recursive = false; // operate on exactly this file

    SUBCASE("dry-run is a reporting no-op")
    {
        const int rc = RunCleanRefsCommand({p}, "unused", opts, /*dryRun=*/true);
        CHECK(rc == 0);
        SdfLayerRefPtr layer = SdfLayer::FindOrOpen(p);
        REQUIRE(layer);
        CHECK(FindDanglingArcs(layer, ArResolverContext()).size() == 4);
    }

    SUBCASE("live run removes every dangling arc")
    {
        const int rc = RunCleanRefsCommand({p}, "unused", opts, /*dryRun=*/false);
        CHECK(rc == 0);
        SdfLayerRefPtr layer = SdfLayer::FindOrOpen(p);
        REQUIRE(layer);
        CHECK(FindDanglingArcs(layer, ArResolverContext()).empty());
    }
}

// ===========================================================================
// Unresolvable asset-valued attribute (texture) cleanup (--textures)
// ===========================================================================

namespace {

// A layer whose shader-like prims carry asset-valued attributes:
//   /Missing.file : @./missing.png@          (unresolvable)   -> removed
//   /Good.file    : @./exists.png@           (resolvable)     -> kept
//   /Udim.file    : @./tex.<UDIM>.png@       (template token) -> kept (skipped)
//   /Arr.files    : [@./miss1.png@, @./exists.png@]           -> drop miss1, keep exists
constexpr const char* kTextureBody = R"usda(#usda 1.0

def "Missing"
{
    custom asset file = @./missing.png@
}

def "Good"
{
    custom asset file = @./exists.png@
}

def "Udim"
{
    custom asset file = @./tex.<UDIM>.png@
}

def "Arr"
{
    custom asset[] files = [@./miss1.png@, @./exists.png@]
}
)usda";

// Write the texture scene plus the one on-disk texture that must resolve.
void WriteTextureScene(const TempDir& tmp, const std::string& scenePath)
{
    std::ofstream ofs(scenePath, std::ios::binary | std::ios::trunc);
    REQUIRE(ofs);
    ofs << kTextureBody;
    ofs.close();

    // Create the texture that is expected to resolve; the others never exist.
    std::ofstream tex(tmp.file("exists.png"), std::ios::binary | std::ios::trunc);
    REQUIRE(tex);
    tex << "png";
    tex.close();
}

} // namespace

TEST_CASE("FindDanglingArcs with includeAttributes catches unresolvable assets, skips <UDIM> and resolvable")
{
    TempDir tmp;
    const std::string p = tmp.file("scene.usda");
    WriteTextureScene(tmp, p);

    SdfLayerRefPtr layer = SdfLayer::FindOrOpen(p);
    REQUIRE(layer);

    // Without includeAttributes nothing is found (no composition arcs here).
    CHECK(FindDanglingArcs(layer, ArResolverContext()).empty());

    // With includeAttributes: /Missing.file and the dead /Arr.files entry.
    const std::vector<DanglingArc> arcs =
        FindDanglingArcs(layer, ArResolverContext(), /*includeAttributes=*/true);
    CHECK(arcs.size() == 2);
    for (const DanglingArc& a : arcs)
    {
        CHECK((a.kind == ArcKind::Attribute));
        CHECK((a.reason == DanglingReason::Unresolvable));
        // The <UDIM> template and the resolvable texture are never reported.
        CHECK(a.assetPath.find("<UDIM>") == std::string::npos);
        CHECK(a.assetPath != "./exists.png");
    }
}

TEST_CASE("CleanupLayerFileInPlace with includeTextures removes dead attrs and drops bad array entries")
{
    TempDir tmp;
    const std::string p = tmp.file("scene.usda");
    WriteTextureScene(tmp, p);

    const CleanRefsResult res = idtx::utils::CleanupLayerFileInPlace(
        p, /*anonOnly=*/false, /*dryRun=*/false, ArResolverContext(),
        /*includeTextures=*/true);
    CHECK(res.ok);
    CHECK(res.found == 2);
    CHECK(res.removed == 2); // the single dead attr + the one dropped array entry

    SdfLayerRefPtr layer = SdfLayer::FindOrOpen(p);
    REQUIRE(layer);

    // The single-valued unresolvable attribute is gone from its prim.
    CHECK_FALSE(layer->GetAttributeAtPath(SdfPath("/Missing.file")));

    // The resolvable texture and the <UDIM> template are untouched.
    CHECK(layer->GetAttributeAtPath(SdfPath("/Good.file")));
    SdfAttributeSpecHandle udim = layer->GetAttributeAtPath(SdfPath("/Udim.file"));
    REQUIRE(udim);
    CHECK(udim->GetDefaultValue().Get<SdfAssetPath>().GetAssetPath() ==
          "./tex.<UDIM>.png");

    // The array kept only its resolvable entry.
    SdfAttributeSpecHandle arr = layer->GetAttributeAtPath(SdfPath("/Arr.files"));
    REQUIRE(arr);
    const VtArray<SdfAssetPath> files =
        arr->GetDefaultValue().Get<VtArray<SdfAssetPath>>();
    REQUIRE(files.size() == 1);
    CHECK(files[0].GetAssetPath() == "./exists.png");

    // Nothing dangling remains under the attribute scan.
    CHECK(FindDanglingArcs(layer, ArResolverContext(), /*includeAttributes=*/true)
              .empty());
}

TEST_CASE("CleanupLayerFileInPlace without includeTextures leaves asset attributes untouched")
{
    TempDir tmp;
    const std::string p = tmp.file("scene.usda");
    WriteTextureScene(tmp, p);

    const CleanRefsResult res = idtx::utils::CleanupLayerFileInPlace(
        p, /*anonOnly=*/false, /*dryRun=*/false, ArResolverContext());
    CHECK(res.ok);
    CHECK(res.found == 0);

    SdfLayerRefPtr layer = SdfLayer::FindOrOpen(p);
    REQUIRE(layer);
    CHECK(layer->GetAttributeAtPath(SdfPath("/Missing.file")));
}

TEST_CASE("RunCleanRefsCommand --textures removes unresolvable asset attributes end-to-end")
{
    TempDir tmp;
    const std::string p = tmp.file("scene.usda");
    WriteTextureScene(tmp, p);

    CleanRefsOptions opts;
    opts.recursive = false; // operate on exactly this file
    opts.textures  = true;

    const int rc = RunCleanRefsCommand({p}, "unused", opts, /*dryRun=*/false);
    CHECK(rc == 0);

    SdfLayerRefPtr layer = SdfLayer::FindOrOpen(p);
    REQUIRE(layer);
    CHECK_FALSE(layer->GetAttributeAtPath(SdfPath("/Missing.file")));
    CHECK(layer->GetAttributeAtPath(SdfPath("/Good.file")));
}
