/**
 * @file test_layer_discovery.cpp
 * @brief Behavioural tests for idtx::utils::CollectLayerTargets.
 *
 * The maintenance commands (updateversion, cleanrefs) discover the layers they
 * operate on through CollectLayerTargets. The key contract these tests pin down
 * is that discovery is *dependency-based*, not *composition-based*: a layer that
 * is reachable only through an unselected variant (or a deactivated prim) never
 * survives composition - stage->GetUsedLayers() would not list it - yet USD
 * still opens it when packaging, so CollectLayerTargets must find it too.
 *
 * The fixtures author a root `.usda` whose selected variant references nothing
 * while the *unselected* variant references an on-disk layer, then assert that
 * layer shows up in the discovered target set. A matching end-to-end check that
 * a deprecated crate hidden behind an unselected variant is actually migrated
 * lives in test_crate_update.cpp.
 */

#include "thirdparty/doctest/doctest.h"

#include <filesystem>
#include <fstream>
#include <set>
#include <string>

#include <pxr/usd/ar/resolverContext.h>

#include "utils/LayerDiscovery.h"

#include "support/StageFixtures.h"

using namespace pxr;
using idtx::tests::TempDir;
using idtx::utils::CollectLayerTargets;
using idtx::utils::DiscoveredLayers;

namespace {

// Does `set` contain an entry whose filename matches `name`?
bool ContainsFilename(const std::set<std::string>& set, const std::string& name)
{
    for (const std::string& p : set)
        if (std::filesystem::path(p).filename() == name)
            return true;
    return false;
}

// Write `body` to `path`.
void WriteFile(const std::string& path, const std::string& body)
{
    std::ofstream ofs(path, std::ios::binary | std::ios::trunc);
    REQUIRE(ofs);
    ofs << body;
    ofs.close();
}

// A trivial, self-contained leaf layer.
constexpr const char* kLeafBody = R"usda(#usda 1.0
def "Leaf" {}
)usda";

// A root whose "shading" variant set selects "clean" (which references nothing)
// while the *unselected* "orig" variant references ./hidden.usda. The hidden
// layer therefore never participates in the composed stage.
constexpr const char* kRootBody = R"usda(#usda 1.0
(
    defaultPrim = "Root"
)

def "Root" (
    prepend variantSets = "shading"
    variants = {
        string shading = "clean"
    }
)
{
    variantSet "shading" = {
        "clean" {
        }
        "orig" (
            prepend references = @./hidden.usda@
        ) {
        }
    }
}
)usda";

} // namespace

TEST_CASE("CollectLayerTargets discovers a layer referenced only from an unselected variant")
{
    TempDir tmp;
    const std::string hidden = tmp.file("hidden.usda");
    const std::string root   = tmp.file("root.usda");
    WriteFile(hidden, kLeafBody);
    WriteFile(root, kRootBody);

    DiscoveredLayers targets;
    ArResolverContext ctx;
    CollectLayerTargets(root, /*recursive=*/true, targets, ctx);

    // Both the root and the variant-hidden leaf must be discovered, even though
    // the leaf is not part of the composed stage (the "clean" variant is the
    // selected one). This is the behaviour stage->GetUsedLayers() lacked.
    CHECK(ContainsFilename(targets.plainFiles, "root.usda"));
    CHECK(ContainsFilename(targets.plainFiles, "hidden.usda"));
}

TEST_CASE("CollectLayerTargets non-recursive classifies only the named input")
{
    TempDir tmp;
    const std::string hidden = tmp.file("hidden.usda");
    const std::string root   = tmp.file("root.usda");
    WriteFile(hidden, kLeafBody);
    WriteFile(root, kRootBody);

    DiscoveredLayers targets;
    ArResolverContext ctx;
    CollectLayerTargets(root, /*recursive=*/false, targets, ctx);

    // Non-recursive: only the explicitly named file, no dependency walk.
    CHECK(ContainsFilename(targets.plainFiles, "root.usda"));
    CHECK_FALSE(ContainsFilename(targets.plainFiles, "hidden.usda"));
    CHECK(targets.plainFiles.size() == 1);
    CHECK(targets.packages.empty());
}
