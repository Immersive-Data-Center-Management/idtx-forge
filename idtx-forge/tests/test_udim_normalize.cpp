/**
 * @file test_udim_normalize.cpp
 * @brief Unit and behavioural tests for the UDIM-token normalization feature.
 *
 * These tests author a tiny `.usda` layer in a scratch directory carrying a mix
 * of asset-valued attributes:
 *   - a printf/Python-dict UDIM token `%(UDIM)d`,
 *   - a zero-padded width variant `%(UDIM)04d`,
 *   - an asset[] with one tokenized and one plain entry,
 *   - a lower-case `<udim>` token,
 *   - an already-canonical `<UDIM>` token (must be left untouched),
 *   - a non-asset attribute (must be left untouched),
 * and then drive:
 *   - idtx::utils::NormalizeUdimString          (pure string transform)
 *   - idtx::utils::FindUdimRewrites             (pure detection)
 *   - idtx::utils::ApplyUdimRewrites            (in-memory rewrite)
 *   - idtx::utils::NormalizeUdimLayerFileInPlace (in-place rewrite, incl. dry-run)
 *   - RunCleanRefsCommand with --udim           (end-to-end)
 */

#include "thirdparty/doctest/doctest.h"

#include <filesystem>
#include <fstream>
#include <string>
#include <vector>

#include <pxr/base/vt/array.h>
#include <pxr/usd/sdf/assetPath.h>
#include <pxr/usd/sdf/attributeSpec.h>
#include <pxr/usd/sdf/layer.h>
#include <pxr/usd/sdf/path.h>

#include "commands/cleanrefs.h"
#include "utils/UdimNormalize.h"

#include "support/StageFixtures.h"

namespace fs = std::filesystem;
using namespace pxr;
using namespace idtx::commands;
using idtx::tests::TempDir;
using idtx::utils::ApplyUdimRewrites;
using idtx::utils::FindUdimRewrites;
using idtx::utils::NormalizeUdimLayerFileInPlace;
using idtx::utils::NormalizeUdimString;
using idtx::utils::UdimResult;
using idtx::utils::UdimRewrite;

namespace {

// A layer with every category of asset path we care about. Four of the six
// asset-path strings carry a non-standard UDIM token:
//   /PrintfTex.file  : @tex_%(UDIM)d_diff.png@   (printf form)        -> rewrite
//   /WidthTex.file   : @tex_%(UDIM)04d.png@      (width variant)      -> rewrite
//   /ArrayTex.files  : [@a_%(UDIM)d.png@, @plain.png@] (one tokenized) -> rewrite
//   /LowerTex.file   : @color.<udim>.exr@        (lower-case token)   -> rewrite
//   /OkTex.file      : @ok.<UDIM>.exr@           (already canonical)  -> kept
//   /NonAsset.x      : 1.0 (float, not an asset)                      -> kept
constexpr const char* kSceneBody = R"usda(#usda 1.0

def "PrintfTex"
{
    custom asset file = @tex_%(UDIM)d_diff.png@
}

def "WidthTex"
{
    custom asset file = @tex_%(UDIM)04d.png@
}

def "ArrayTex"
{
    custom asset[] files = [@a_%(UDIM)d.png@, @plain.png@]
}

def "LowerTex"
{
    custom asset file = @color.<udim>.exr@
}

def "OkTex"
{
    custom asset file = @ok.<UDIM>.exr@
}

def "NonAsset"
{
    custom float x = 1
}
)usda";

void WriteScene(const std::string& path)
{
    std::ofstream ofs(path, std::ios::binary | std::ios::trunc);
    REQUIRE(ofs);
    ofs << kSceneBody;
    ofs.close();
}

} // namespace

TEST_CASE("NormalizeUdimString rewrites the recognised forms and keeps the rest")
{
    std::string out;

    CHECK(NormalizeUdimString("tex_%(UDIM)d_diff.png", out));
    CHECK(out == "tex_<UDIM>_diff.png");

    CHECK(NormalizeUdimString("tex_%(UDIM)04d.png", out));
    CHECK(out == "tex_<UDIM>.png");

    CHECK(NormalizeUdimString("tex_%(udim)4d.png", out)); // lower-case key
    CHECK(out == "tex_<UDIM>.png");

    CHECK(NormalizeUdimString("color.<udim>.exr", out));
    CHECK(out == "color.<UDIM>.exr");

    // Already canonical / unrelated strings are reported as unchanged.
    CHECK_FALSE(NormalizeUdimString("ok.<UDIM>.exr", out));
    CHECK(out == "ok.<UDIM>.exr");
    CHECK_FALSE(NormalizeUdimString("plain.png", out));
    CHECK(out == "plain.png");
}

TEST_CASE("FindUdimRewrites detects non-standard tokens and skips canonical/non-asset")
{
    TempDir tmp;
    const std::string p = tmp.file("scene.usda");
    WriteScene(p);

    SdfLayerRefPtr layer = SdfLayer::FindOrOpen(p);
    REQUIRE(layer);

    const std::vector<UdimRewrite> rewrites = FindUdimRewrites(layer);

    // PrintfTex, WidthTex, ArrayTex (one element), LowerTex -> 4; OkTex and the
    // non-asset attribute contribute nothing.
    CHECK(rewrites.size() == 4);

    bool sawOk = false;
    for (const UdimRewrite& rw : rewrites)
    {
        CHECK(rw.newPath.find("<UDIM>") != std::string::npos);
        CHECK(rw.newPath.find("%(") == std::string::npos);
        CHECK(rw.newPath.find("<udim>") == std::string::npos);
        if (rw.oldPath == "ok.<UDIM>.exr")
            sawOk = true;
    }
    CHECK_FALSE(sawOk);
}

TEST_CASE("ApplyUdimRewrites rewrites every non-standard token in memory")
{
    TempDir tmp;
    const std::string p = tmp.file("scene.usda");
    WriteScene(p);

    SdfLayerRefPtr layer = SdfLayer::FindOrOpen(p);
    REQUIRE(layer);

    const int rewritten = ApplyUdimRewrites(layer);
    CHECK(rewritten == 4);

    // Nothing left to find.
    CHECK(FindUdimRewrites(layer).empty());

    // The already-canonical token and the plain array entry are preserved.
    SdfAttributeSpecHandle ok =
        layer->GetAttributeAtPath(SdfPath("/OkTex.file"));
    REQUIRE(ok);
    CHECK(ok->GetDefaultValue().Get<SdfAssetPath>().GetAssetPath() ==
          "ok.<UDIM>.exr");

    SdfAttributeSpecHandle arr =
        layer->GetAttributeAtPath(SdfPath("/ArrayTex.files"));
    REQUIRE(arr);
    const VtArray<SdfAssetPath> files =
        arr->GetDefaultValue().Get<VtArray<SdfAssetPath>>();
    REQUIRE(files.size() == 2);
    CHECK(files[0].GetAssetPath() == "a_<UDIM>.png");
    CHECK(files[1].GetAssetPath() == "plain.png");
}

TEST_CASE("NormalizeUdimLayerFileInPlace dry-run reports but does not modify the file")
{
    TempDir tmp;
    const std::string p = tmp.file("scene.usda");
    WriteScene(p);

    const UdimResult res = NormalizeUdimLayerFileInPlace(p, /*dryRun=*/true);
    CHECK(res.ok);
    CHECK(res.found == 4);
    CHECK(res.rewritten == 0);

    // A fresh open still sees all four non-standard tokens.
    SdfLayerRefPtr layer = SdfLayer::FindOrOpen(p);
    REQUIRE(layer);
    CHECK(FindUdimRewrites(layer).size() == 4);
}

TEST_CASE("NormalizeUdimLayerFileInPlace rewrites tokens and re-serializes the file")
{
    TempDir tmp;
    const std::string p = tmp.file("scene.usda");
    WriteScene(p);

    const UdimResult res = NormalizeUdimLayerFileInPlace(p, /*dryRun=*/false);
    CHECK(res.ok);
    CHECK(res.found == 4);
    CHECK(res.rewritten == 4);

    // A fresh open re-reads the rewritten file: no non-standard tokens remain.
    SdfLayerRefPtr layer = SdfLayer::FindOrOpen(p);
    REQUIRE(layer);
    CHECK(FindUdimRewrites(layer).empty());

    // No temp file left behind next to the original.
    CHECK_FALSE(fs::exists(p + ".idtxudim.usda"));
}

TEST_CASE("RunCleanRefsCommand --udim: dry-run leaves the file, live run normalizes it")
{
    TempDir tmp;
    const std::string p = tmp.file("scene.usda");
    WriteScene(p);

    CleanRefsOptions opts;
    opts.recursive = false; // operate on exactly this file
    opts.udim      = true;

    SUBCASE("dry-run is a reporting no-op")
    {
        const int rc = RunCleanRefsCommand({p}, "unused", opts, /*dryRun=*/true);
        CHECK(rc == 0);
        SdfLayerRefPtr layer = SdfLayer::FindOrOpen(p);
        REQUIRE(layer);
        CHECK(FindUdimRewrites(layer).size() == 4);
    }

    SUBCASE("live run normalizes every non-standard token")
    {
        const int rc = RunCleanRefsCommand({p}, "unused", opts, /*dryRun=*/false);
        CHECK(rc == 0);
        SdfLayerRefPtr layer = SdfLayer::FindOrOpen(p);
        REQUIRE(layer);
        CHECK(FindUdimRewrites(layer).empty());
    }
}

TEST_CASE("RunCleanRefsCommand without --udim leaves UDIM tokens untouched")
{
    TempDir tmp;
    const std::string p = tmp.file("scene.usda");
    WriteScene(p);

    CleanRefsOptions opts;
    opts.recursive = false;
    opts.udim      = false;

    const int rc = RunCleanRefsCommand({p}, "unused", opts, /*dryRun=*/false);
    CHECK(rc == 0);

    SdfLayerRefPtr layer = SdfLayer::FindOrOpen(p);
    REQUIRE(layer);
    CHECK(FindUdimRewrites(layer).size() == 4);
}
