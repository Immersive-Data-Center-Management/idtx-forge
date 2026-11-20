#include "thirdparty/doctest/doctest.h"
#include "support/StageFixtures.h"
#include "utils/CrateUpdate.h"
#include "utils/ReferenceCleanup.h"
#include "utils/UdimNormalize.h"
#include "utils/UsdzRepackage.h"

#include <fstream>
#include <pxr/usd/sdf/assetPath.h>
#include <pxr/usd/sdf/attributeSpec.h>
#include <pxr/usd/sdf/zipFile.h>

namespace fs = std::filesystem;
using namespace pxr;
using namespace idtx::utils;

namespace
{
const char* scene = R"usd(#usda 1.0
(
    defaultPrim = "World"
)
def Xform "World" {
    custom asset texture = @tiles_%(UDIM)d.png@
    def Xform "Child" (
        prepend references = @anon:0xdeadbeef:.usda@
    ) {}
}
)usd";

void MakeLayer(const fs::path& path)
{
    SdfLayerRefPtr layer = SdfLayer::CreateAnonymous();
    REQUIRE(layer->ImportFromString(scene));
    REQUIRE(layer->Export(path.string()));
}

void AssertLayer(const std::string& path)
{
    const SdfLayerRefPtr layer = SdfLayer::FindOrOpen(path);
    REQUIRE(layer);
    CHECK(FindDanglingArcs(layer, ArResolverContext()).empty());
    const auto attribute = layer->GetAttributeAtPath(SdfPath("/World.texture"));
    REQUIRE(attribute);
    CHECK(attribute->GetDefaultValue().Get<SdfAssetPath>().GetAssetPath() == "tiles_<UDIM>.png");
    CHECK(layer->GetPrimAtPath(SdfPath("/World/Child")));
}

void AssertNoWorkspaces(const fs::path& directory)
{
    for (const auto& entry : fs::directory_iterator(directory))
        CHECK_FALSE(entry.path().filename().string().starts_with(".idtx-rewrite-"));
}
} // namespace

TEST_CASE("Maintenance publication preserves real USD and USDC content across all three rewrites")
{
    idtx::tests::TempDir scratch;
    for (const char* extension : {".usd", ".usdc", ".usda"})
    {
        const fs::path target = scratch.path() / (std::string("scene") + extension);
        MakeLayer(target);
        CHECK(CleanupLayerFileInPlace(target.string(), false, false, ArResolverContext()).ok);
        CHECK(NormalizeUdimLayerFileInPlace(target.string(), false).ok);
        if (InspectCrateVersion(target.string()).isCrate) CHECK(UpdateCrateFileInPlace(target.string()));
        AssertLayer(target.string());
        AssertNoWorkspaces(scratch.path());
    }
}

TEST_CASE("Maintenance publication preserves USDZ entry order and auxiliary bytes")
{
    idtx::tests::TempDir scratch;
    const fs::path layer = scratch.path() / "root.usdc";
    MakeLayer(layer);
    // The fixture's simple crate is backward-compatible with 0.7.0.
    {
        std::fstream file(layer, std::ios::binary | std::ios::in | std::ios::out);
        file.seekp(9);
        file.put(7);
        REQUIRE(file.good());
    }
    const fs::path auxiliary = scratch.path() / "auxiliary.bin";
    {
        std::ofstream file(auxiliary, std::ios::binary);
        file << "unchanged auxiliary bytes";
    }
    const fs::path package = scratch.path() / "scene.usdz";
    {
        SdfZipFileWriter writer = SdfZipFileWriter::CreateNew(package.string());
        REQUIRE(writer);
        REQUIRE_FALSE(writer.AddFile(layer.string(), "root.usdc").empty());
        REQUIRE_FALSE(writer.AddFile(auxiliary.string(), "assets/auxiliary.bin").empty());
        REQUIRE(writer.Save());
    }
    REQUIRE(UpdateUsdzCratesInPlace(package.string(), false).ok);
    REQUIRE(CleanupUsdzReferencesInPlace(package.string(), false, false).ok);
    REQUIRE(NormalizeUdimUsdzInPlace(package.string(), false).ok);
    AssertLayer(package.string() + "[root.usdc]");
    {
        const SdfZipFile zip = SdfZipFile::Open(package.string());
        REQUIRE(zip);
        auto entry = zip.begin();
        REQUIRE(entry != zip.end());
        CHECK(*entry == "root.usdc");
        ++entry;
        REQUIRE(entry != zip.end());
        CHECK(*entry == "assets/auxiliary.bin");
        CHECK(std::string(entry.GetFile(), entry.GetFileInfo().size) == "unchanged auxiliary bytes");
        CHECK(++entry == zip.end());
    }
    AssertNoWorkspaces(scratch.path());
}
