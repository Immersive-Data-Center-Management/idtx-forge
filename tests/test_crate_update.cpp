/**
 * @file test_crate_update.cpp
 * @brief Unit and behavioural tests for the crate-version update feature.
 *
 * The on-disk fixtures in tests/data/ are all `.usda` (text) layers, which
 * carry no crate version. To exercise the binary-crate paths these tests first
 * materialise a `.usdc` by exporting a fixture stage into a scratch directory,
 * then drive:
 *   - idtx::utils::CompareCrateVersions  (pure version arithmetic)
 *   - idtx::utils::InspectCrateVersion   (crate vs. non-crate detection)
 *   - idtx::utils::UpdateCrateFileInPlace (in-place re-serialization)
 *   - RunUpdateVersionCommand            (end-to-end, incl. dry-run)
 *
 * A freshly written crate file is, by construction, at the current software
 * version, so these tests assert the "already current" contract and the
 * idempotence of an in-place update rather than an actual version bump (which
 * would require shipping a stale crate fixture).
 */

#include "thirdparty/doctest/doctest.h"

#include <cstdio>
#include <filesystem>
#include <fstream>
#include <string>
#include <vector>

#include <pxr/usd/usd/stage.h>

#include "commands/updateversion.h"
#include "utils/CrateUpdate.h"

#include "support/StageFixtures.h"

namespace fs = std::filesystem;
using namespace pxr;
using namespace idtx::commands;
using idtx::tests::DataFile;
using idtx::tests::TempDir;

namespace {

// Export a `.usda` fixture into `dir` as a binary `.usdc` crate file and return
// its path. The crate is written at the current software version.
std::string MakeCrateFromFixture(const TempDir& dir,
                                 const std::string& fixture,
                                 const std::string& outName)
{
    UsdStageRefPtr stage = UsdStage::Open(DataFile(fixture));
    REQUIRE(stage);
    const std::string out = dir.file(outName);
    REQUIRE(stage->Export(out)); // .usdc extension => crate encoding
    return out;
}

// Overwrite the crate header's minor-version byte (offset 9) so the file claims
// an older version. OpenUSD stores the crate version as the three bytes
// following the 8-byte "PXR-USDC" magic (major, minor, patch). A simple crate
// written at 0.8.0 is structurally identical to 0.7.0 (0.8.0 only adds payload
// list-op/offset support), so relabeling it to 0.7.0 yields a file OpenUSD can
// still parse but now treats as deprecated.
void RelabelMinorVersion(const std::string& crPath, unsigned char minor)
{
    std::fstream f(crPath, std::ios::in | std::ios::out | std::ios::binary);
    REQUIRE(f);
    f.seekp(9, std::ios::beg);
    const char b = static_cast<char>(minor);
    f.write(&b, 1);
    REQUIRE(f.good());
}

} // namespace

TEST_CASE("CompareCrateVersions orders dotted version tokens")
{
    using idtx::utils::CompareCrateVersions;

    CHECK(CompareCrateVersions("0.8.0", "0.10.0") < 0);
    CHECK(CompareCrateVersions("0.10.0", "0.8.0") > 0);
    CHECK(CompareCrateVersions("0.8.0", "0.8.0") == 0);

    // Missing trailing components are treated as zero.
    CHECK(CompareCrateVersions("0.8", "0.8.0") == 0);
    CHECK(CompareCrateVersions("1", "0.9.9") > 0);

    // Numeric (not lexicographic) comparison of components.
    CHECK(CompareCrateVersions("0.2.0", "0.10.0") < 0);
}

TEST_CASE("deprecation threshold identifies versions below OLDEST_CURRENT_VERSION")
{
    using idtx::utils::CompareCrateVersions;
    const std::string threshold = idtx::utils::CrateDeprecationThreshold();

    // A file is deprecated iff strictly older than the threshold.
    CHECK(CompareCrateVersions("0.7.0", threshold) < 0);  // deprecated
    CHECK(CompareCrateVersions("0.0.1", threshold) < 0);  // deprecated
    CHECK(CompareCrateVersions(threshold, threshold) == 0); // NOT deprecated
    CHECK(CompareCrateVersions("0.10.0", threshold) > 0);   // NOT deprecated
}

TEST_CASE("InspectCrateVersion distinguishes crate from text layers")
{
    TempDir tmp;

    SUBCASE("text .usda layer is not a crate")
    {
        const idtx::utils::CrateVersionInfo info =
            idtx::utils::InspectCrateVersion(DataFile("single_quad.usda"));
        CHECK_FALSE(info.isCrate);
        CHECK_FALSE(info.isOutdated);
    }

    SUBCASE("exported .usdc is a current (non-deprecated) crate")
    {
        const std::string crate =
            MakeCrateFromFixture(tmp, "single_quad.usda", "single_quad.usdc");

        const idtx::utils::CrateVersionInfo info =
            idtx::utils::InspectCrateVersion(crate);
        CHECK(info.isCrate);
        CHECK_FALSE(info.fileVersion.empty());
        CHECK_FALSE(info.softwareVersion.empty());
        // OpenUSD writes new files at the current (>= threshold) version, so a
        // freshly exported crate is never deprecated.
        CHECK_FALSE(info.isOutdated);
        CHECK(idtx::utils::CompareCrateVersions(
                  info.fileVersion,
                  idtx::utils::CrateDeprecationThreshold()) >= 0);
    }
}

TEST_CASE("UpdateCrateFileInPlace rewrites a crate without corrupting it")
{
    TempDir tmp;
    const std::string crate =
        MakeCrateFromFixture(tmp, "two_meshes.usda", "two_meshes.usdc");

    REQUIRE(idtx::utils::UpdateCrateFileInPlace(crate));

    // Still a valid, openable, non-deprecated crate afterwards, and no temp
    // file left behind next to it.
    const idtx::utils::CrateVersionInfo info =
        idtx::utils::InspectCrateVersion(crate);
    CHECK(info.isCrate);
    CHECK_FALSE(info.isOutdated);
    CHECK(UsdStage::Open(crate)); // re-opens cleanly
    CHECK_FALSE(fs::exists(crate + ".idtxupdate.usdc"));
}

TEST_CASE("a deprecated crate is detected and migrated to a current version")
{
    TempDir tmp;
    const std::string crate =
        MakeCrateFromFixture(tmp, "single_quad.usda", "deprecated.usdc");

    // Relabel the freshly written crate as version 0.7.0 (below the 0.8.0
    // deprecation threshold) so it looks like a legacy asset.
    RelabelMinorVersion(crate, 7);

    SUBCASE("detection")
    {
        const idtx::utils::CrateVersionInfo info =
            idtx::utils::InspectCrateVersion(crate);
        CHECK(info.isCrate);
        CHECK(info.fileVersion == "0.7.0");
        CHECK(info.isOutdated);
    }

    SUBCASE("in-place migration lifts it above the threshold")
    {
        REQUIRE(idtx::utils::UpdateCrateFileInPlace(crate));

        const idtx::utils::CrateVersionInfo after =
            idtx::utils::InspectCrateVersion(crate);
        CHECK(after.isCrate);
        CHECK_FALSE(after.isOutdated);
        CHECK(idtx::utils::CompareCrateVersions(
                  after.fileVersion,
                  idtx::utils::CrateDeprecationThreshold()) >= 0);
        CHECK(UsdStage::Open(crate)); // content preserved & still openable
        CHECK_FALSE(fs::exists(crate + ".idtxupdate.usdc"));
    }

    SUBCASE("the command migrates it and reports one update")
    {
        UpdateVersionOptions opts;
        opts.recursive = false; // operate on exactly this file
        const int rc = RunUpdateVersionCommand({crate}, "unused", opts,
                                                /*dryRun=*/false);
        CHECK(rc == 0);
        CHECK_FALSE(idtx::utils::InspectCrateVersion(crate).isOutdated);
    }

    SUBCASE("dry-run reports but does not migrate it")
    {
        UpdateVersionOptions opts;
        opts.recursive = false;
        const int rc = RunUpdateVersionCommand({crate}, "unused", opts,
                                                /*dryRun=*/true);
        CHECK(rc == 0);
        // Still deprecated: dry-run must not touch the file.
        CHECK(idtx::utils::InspectCrateVersion(crate).isOutdated);
    }
}

TEST_CASE("RunUpdateVersionCommand succeeds and leaves current crates intact")
{
    TempDir tmp;
    const std::string crate =
        MakeCrateFromFixture(tmp, "single_quad.usda", "scene.usdc");

    UpdateVersionOptions opts; // recursive by default

    SUBCASE("live run on an already-current crate is a no-op success")
    {
        const int rc = RunUpdateVersionCommand({crate}, tmp.path().string(),
                                                opts, /*dryRun=*/false);
        CHECK(rc == 0);
        CHECK(UsdStage::Open(crate));
    }

    SUBCASE("dry-run writes nothing and reports success")
    {
        const int rc = RunUpdateVersionCommand({crate}, tmp.path().string(),
                                                opts, /*dryRun=*/true);
        CHECK(rc == 0);
        CHECK(UsdStage::Open(crate));
    }
}

TEST_CASE("RunUpdateVersionCommand on a text layer is a graceful no-op")
{
    UpdateVersionOptions opts;
    opts.recursive = false; // treat the named file verbatim

    const int rc = RunUpdateVersionCommand({DataFile("single_quad.usda")},
                                            "unused", opts, /*dryRun=*/false);
    // A text layer carries no crate version: nothing to update, no failure.
    CHECK(rc == 0);
}
