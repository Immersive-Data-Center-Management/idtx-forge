/**
 * @file UsdzRepackage.cpp
 * @brief Implementation of the usdz re-packaging utility.
 *
 * See UsdzRepackage.h for the high-level description of the algorithm.
 **/

#include "utils/UsdzRepackage.h"

#include <cstdint>
#include <filesystem>
#include <fstream>
#include <string>
#include <system_error>
#include <utility>
#include <vector>

#include <pxr/usd/sdf/layer.h>
#include <pxr/usd/sdf/zipFile.h>

#include <idtx/utils/Logger.h>

#include "utils/CrateUpdate.h"
#include "utils/ReferenceCleanup.h"
#include "utils/TextureResize.h"
#include "utils/UdimNormalize.h"

#include <algorithm>

#include <pxr/usd/ar/resolverContext.h>

namespace fs = std::filesystem;
PXR_NAMESPACE_USING_DIRECTIVE

IDTX_LOG_CATEGORY("UsdzRepackage")

namespace idtx::utils {

namespace {

/// RAII guard that recursively removes a directory on destruction.
struct TempDirGuard
{
    fs::path path;
    bool     keep = false;
    explicit TempDirGuard(fs::path p) : path(std::move(p)) {}
    ~TempDirGuard()
    {
        if (keep || path.empty()) return;
        std::error_code ec;
        fs::remove_all(path, ec);
    }
};

/// Extracts every entry of a usdz package into @p destDir while preserving
/// the original ordering and relative paths. The first entry is, per the
/// usdz specification, the default root layer.
bool ExtractUsdzContents(
    const std::string&        usdzPath,
    const fs::path&           destDir,
    std::vector<std::string>& outFileOrder)
{
    SdfZipFile zip = SdfZipFile::Open(usdzPath);
    if (!zip)
    {
        IDTX_LOG(IDTX_WARN, "  Cannot open usdz package '{}'", usdzPath);
        return false;
    }

    for (auto it = zip.begin(); it != zip.end(); ++it)
    {
        const std::string  name     = *it;
        const SdfZipFile::FileInfo info = it.GetFileInfo();
        const char*        dataPtr  = it.GetFile();
        const size_t       dataSize = info.size;

        // We only support uncompressed entries (compressionMethod == 0),
        // which is the only mode usdz officially uses. If we ever encounter
        // a compressed entry we cannot decompress it ourselves, so abort.
        if (info.compressionMethod != 0)
        {
            IDTX_LOG(IDTX_WARN,
                "  Compressed entry '{}' in usdz '{}' - cannot re-package",
                name, usdzPath);
            return false;
        }

        if (!dataPtr && dataSize > 0)
        {
            IDTX_LOG(IDTX_WARN, "  Could not read entry '{}' from '{}'",
                name, usdzPath);
            return false;
        }

        const fs::path outFile = destDir / name;
        std::error_code ec;
        fs::create_directories(outFile.parent_path(), ec);
        if (ec && !fs::exists(outFile.parent_path()))
        {
            IDTX_LOG(IDTX_WARN, "  Could not create directory '{}': {}",
                outFile.parent_path().string(), ec.message());
            return false;
        }

        std::ofstream ofs(outFile, std::ios::binary | std::ios::trunc);
        if (!ofs)
        {
            IDTX_LOG(IDTX_WARN, "  Could not open '{}' for writing",
                outFile.string());
            return false;
        }
        if (dataSize > 0)
            ofs.write(dataPtr, static_cast<std::streamsize>(dataSize));
        ofs.close();

        outFileOrder.push_back(name);
        IDTX_LOG(IDTX_DEBUG, "    extracted '{}' ({} bytes)", name, dataSize);
    }

    if (outFileOrder.empty())
    {
        IDTX_LOG(IDTX_WARN, "  usdz package '{}' is empty", usdzPath);
        return false;
    }
    return true;
}

/// Builds a new usdz package from the contents of @p srcDir. Files are added
/// in @p fileOrder; the first element of that vector is added first as
/// required by the usdz specification (it becomes the default root layer).
bool WriteUsdzPackage(
    const std::string&              dstUsdz,
    const fs::path&                 srcDir,
    const std::vector<std::string>& fileOrder)
{
    SdfZipFileWriter writer = SdfZipFileWriter::CreateNew(dstUsdz);
    if (!writer)
    {
        IDTX_LOG(IDTX_WARN, "  Could not create usdz '{}'", dstUsdz);
        return false;
    }

    for (const std::string& name : fileOrder)
    {
        const fs::path absPath = srcDir / name;
        if (!fs::exists(absPath))
        {
            IDTX_LOG(IDTX_WARN, "  Missing file in temp dir: '{}'",
                absPath.string());
            writer.Discard();
            return false;
        }
        const std::string added = writer.AddFile(absPath.string(), name);
        if (added.empty())
        {
            IDTX_LOG(IDTX_WARN, "  Failed to add '{}' to usdz", name);
            writer.Discard();
            return false;
        }
        IDTX_LOG(IDTX_DEBUG, "    added '{}'", name);
    }

    if (!writer.Save())
    {
        IDTX_LOG(IDTX_WARN, "  Failed to save usdz '{}'", dstUsdz);
        return false;
    }
    return true;
}

} // anonymous namespace

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------
bool RepackageUsdz(
    const std::string&          srcUsdz,
    const UsdStageRefPtr&       stage,
    const std::string&          dstUsdz,
    const TextureResizeOptions& texOpts)
{
    const fs::path dstPath(dstUsdz);

    // Create a unique hidden temporary working directory next to the
    // destination usdz. Cleaned up automatically by TempDirGuard.
    const fs::path tmpDir = dstPath.parent_path() /
        ("." + dstPath.stem().string() + "_idtx_tmp");

    std::error_code ec;
    fs::remove_all(tmpDir, ec); // best-effort, may not exist
    fs::create_directories(tmpDir, ec);
    if (ec && !fs::exists(tmpDir))
    {
        IDTX_LOG(IDTX_WARN, "  Could not create temp dir '{}': {}",
            tmpDir.string(), ec.message());
        return false;
    }
    TempDirGuard guard(tmpDir);

    // 1) Extract the source usdz contents (preserving order).
    std::vector<std::string> fileOrder;
    if (!ExtractUsdzContents(srcUsdz, tmpDir, fileOrder))
        return false;

    // 2) Overwrite the root layer (first entry) with the modified stage.
    //
    //    IMPORTANT: we must export via SdfLayer::Export() on the root layer,
    //    NOT via UsdStage::Export().
    //
    //    UsdStage::Export() writes the fully *composed/flattened* stage and
    //    resolves every SdfAssetPath to its absolute resolved path at the time
    //    of export.  When the stage was opened from a usdz the resolver bakes
    //    those paths as absolute package-internal URLs, e.g.:
    //        C:\...\original.usdz[0/texture.png]
    //    Those absolute paths would be embedded verbatim in the new root layer,
    //    making every texture and sub-layer reference point back to the
    //    *original* source file instead of the siblings re-packed alongside it.
    //
    //    SdfLayer::Export() writes only the *authored* opinions of the single
    //    root layer, preserving the original relative/package-internal asset
    //    paths (e.g. "0/texture.png") exactly as they were
    //    stored in the source usdz.  Those relative paths will correctly
    //    resolve to the co-packed siblings inside the new output usdz.
    //
    //    We keep the original root file name so its original encoding (.usda
    //    vs .usdc) is preserved.
    const std::string& rootLayerName = fileOrder.front();
    const fs::path     rootLayerPath = tmpDir / rootLayerName;

    fs::remove(rootLayerPath, ec); // ensure writable

    SdfLayerHandle rootLayer = stage->GetRootLayer();
    if (!rootLayer)
    {
        IDTX_LOG(IDTX_WARN, "  Stage has no root layer - cannot re-package");
        return false;
    }

    if (!rootLayer->Export(rootLayerPath.string()))
    {
        IDTX_LOG(IDTX_WARN, "  Failed to export modified root layer to '{}'",
            rootLayerPath.string());
        return false;
    }

    // 3) Optionally downsample texture entries. We iterate every non-root
    //    entry, decode it as an image, resize and re-encode into the same
    //    in-package path. Files that are not decodable images (USD layers,
    //    audio, ...) or that are already at-or-below the target dimensions
    //    are left untouched, matching the contract of TryResizeImageFile.
    //
    //    Doing this here - after the modified root layer has been written
    //    but before the package is zipped - keeps the asset references in
    //    the root layer (e.g. "0/baseColor.png") valid, since file names
    //    are preserved verbatim. No SdfAssetPath rewriting is necessary.
    if (texOpts.scale < 1.0f)
    {
        std::size_t totalSrc = 0, totalDst = 0;
        int reduced = 0, skipped = 0;
        // Skip index 0: that is the modified root layer we just wrote.
        for (std::size_t i = 1; i < fileOrder.size(); ++i)
        {
            const std::string& name = fileOrder[i];
            const fs::path     entryPath = tmpDir / name;

            std::error_code rdEc;
            const auto srcSize = fs::file_size(entryPath, rdEc);
            if (rdEc) { ++skipped; continue; }

            // Slurp the file into memory.
            std::ifstream ifs(entryPath, std::ios::binary);
            if (!ifs) { ++skipped; continue; }
            std::vector<std::uint8_t> srcBuf(srcSize);
            if (srcSize > 0)
                ifs.read(reinterpret_cast<char*>(srcBuf.data()),
                         static_cast<std::streamsize>(srcSize));
            ifs.close();

            std::vector<std::uint8_t> dstBuf;
            const bool didResize = TryResizeImageFile(
                name, srcBuf.data(), srcBuf.size(), texOpts, dstBuf);

            if (!didResize) { ++skipped; continue; }

            // Overwrite the temp-dir copy with the reduced bytes so that
            // the subsequent WriteUsdzPackage step picks it up.
            std::ofstream ofs(entryPath,
                std::ios::binary | std::ios::trunc);
            if (!ofs)
            {
                IDTX_LOG(IDTX_WARN,
                    "  Could not write resized texture '{}', keeping original",
                    entryPath.string());
                ++skipped;
                continue;
            }
            ofs.write(reinterpret_cast<const char*>(dstBuf.data()),
                      static_cast<std::streamsize>(dstBuf.size()));
            ofs.close();

            totalSrc += srcSize;
            totalDst += dstBuf.size();
            ++reduced;
        }

        if (reduced > 0)
        {
            const double pct = totalSrc > 0
                ? (100.0 * static_cast<double>(totalDst) /
                   static_cast<double>(totalSrc))
                : 0.0;
            IDTX_LOG(IDTX_INFO,
                "  Resized {} texture(s) (scale={:.3f}): {} -> {} bytes ({:.1f}%); {} entries untouched",
                reduced, texOpts.scale, totalSrc, totalDst, pct, skipped);
        }
        else
        {
            IDTX_LOG(IDTX_DEBUG,
                "  No textures resized (scale={:.3f}, {} entries untouched)",
                texOpts.scale, skipped);
        }
    }

    // 4) Re-zip every entry (root layer first) into the destination usdz.
    if (!WriteUsdzPackage(dstUsdz, tmpDir, fileOrder))
    {
        std::error_code rmEc;
        fs::remove(dstPath, rmEc); // best-effort cleanup of partial output
        return false;
    }

    IDTX_LOG(IDTX_INFO, "  Re-packaged {} file(s) into '{}'",
        fileOrder.size(), dstUsdz);
    return true;
}

// ---------------------------------------------------------------------------
// UpdateUsdzCratesInPlace
// ---------------------------------------------------------------------------
UsdzCrateUpdateResult UpdateUsdzCratesInPlace(
    const std::string& usdzPath,
    bool               dryRun)
{
    UsdzCrateUpdateResult result;

    const fs::path srcPath(usdzPath);

    // Unique hidden temporary working directory next to the package. Cleaned up
    // automatically by TempDirGuard.
    const fs::path tmpDir = srcPath.parent_path() /
        ("." + srcPath.stem().string() + "_idtx_crate_tmp");

    std::error_code ec;
    fs::remove_all(tmpDir, ec); // best-effort, may not exist
    fs::create_directories(tmpDir, ec);
    if (ec && !fs::exists(tmpDir))
    {
        IDTX_LOG(IDTX_WARN, "  Could not create temp dir '{}': {}",
            tmpDir.string(), ec.message());
        return result;
    }
    TempDirGuard guard(tmpDir);

    // 1) Extract every entry (preserving order). The SdfZipFile the extractor
    //    opens is released before we return from ExtractUsdzContents, so the
    //    original package is no longer memory-mapped by the time we rewrite it.
    std::vector<std::string> fileOrder;
    if (!ExtractUsdzContents(usdzPath, tmpDir, fileOrder))
        return result;

    // 2) Inspect every extracted entry; update the deprecated crate ones in
    //    place inside the temp dir. UpdateCrateFileInPlace operates on the
    //    extracted plain-file copy, preserving the entry's authored (relative)
    //    asset paths so the re-zipped package stays self-consistent.
    for (const std::string& name : fileOrder)
    {
        const fs::path entryPath = tmpDir / name;
        const CrateVersionInfo info = InspectCrateVersion(entryPath.string());

        if (!info.isCrate)
        {
            ++result.nonCrate;
            continue;
        }

        if (!info.isOutdated)
        {
            IDTX_LOG(IDTX_INFO, "    '{}' crate version {} is current",
                name, info.fileVersion);
            ++result.upToDate;
            continue;
        }

        IDTX_LOG(IDTX_INFO,
            "    '{}' has deprecated crate version {} (below current {})",
            name, info.fileVersion, CrateDeprecationThreshold());

        if (dryRun)
        {
            ++result.updated; // count what *would* be updated for the report
            continue;
        }

        if (!UpdateCrateFileInPlace(entryPath.string()))
        {
            IDTX_LOG(IDTX_WARN,
                "    Failed to update packaged entry '{}' - aborting, package "
                "left unchanged", name);
            return result;
        }
        ++result.updated;
    }

    // 3) Nothing to do (or a dry-run): leave the original package untouched.
    if (dryRun || result.updated == 0)
    {
        result.ok = true;
        return result;
    }

    // 4) Re-zip every entry (original order) into a sibling temp package, then
    //    atomically swap it over the original. We write to a sibling rather
    //    than directly over the source so a failed write never destroys the
    //    original.
    const fs::path newPkg = srcPath.parent_path() /
        ("." + srcPath.stem().string() + "_idtx_crate_new.usdz");
    fs::remove(newPkg, ec); // best-effort

    if (!WriteUsdzPackage(newPkg.string(), tmpDir, fileOrder))
    {
        fs::remove(newPkg, ec); // best-effort cleanup of partial output
        return result;
    }

    // Swap: remove the original then move the new package into its place. A
    // plain rename cannot overwrite an existing file on every platform, so we
    // remove first and fall back to copy+remove if the move fails.
    fs::remove(srcPath, ec);
    std::error_code moveEc;
    fs::rename(newPkg, srcPath, moveEc);
    if (moveEc)
    {
        std::error_code copyEc;
        fs::copy_file(newPkg, srcPath,
            fs::copy_options::overwrite_existing, copyEc);
        fs::remove(newPkg, ec);
        if (copyEc)
        {
            IDTX_LOG(IDTX_ERROR,
                "  Could not replace original package '{}': {}",
                usdzPath, copyEc.message());
            return result;
        }
    }

    IDTX_LOG(IDTX_INFO,
        "  Updated {} crate entry/entries in package '{}'",
        result.updated, usdzPath);
    result.ok = true;
    return result;
}

// ---------------------------------------------------------------------------
// CleanupUsdzReferencesInPlace
// ---------------------------------------------------------------------------
UsdzCleanupResult CleanupUsdzReferencesInPlace(
    const std::string& usdzPath,
    bool               anonOnly,
    bool               dryRun,
    bool               includeTextures)
{
    UsdzCleanupResult result;

    const fs::path srcPath(usdzPath);

    // Unique hidden temporary working directory next to the package. Cleaned up
    // automatically by TempDirGuard.
    const fs::path tmpDir = srcPath.parent_path() /
        ("." + srcPath.stem().string() + "_idtx_clean_tmp");

    std::error_code ec;
    fs::remove_all(tmpDir, ec); // best-effort, may not exist
    fs::create_directories(tmpDir, ec);
    if (ec && !fs::exists(tmpDir))
    {
        IDTX_LOG(IDTX_WARN, "  Could not create temp dir '{}': {}",
            tmpDir.string(), ec.message());
        return result;
    }
    TempDirGuard guard(tmpDir);

    // 1) Extract every entry (preserving order). The SdfZipFile is released
    //    before we return from ExtractUsdzContents.
    std::vector<std::string> fileOrder;
    if (!ExtractUsdzContents(usdzPath, tmpDir, fileOrder))
        return result;

    // 2) Clean each packaged USD layer entry in place inside the temp dir.
    //    Non-layer entries (textures, audio, ...) are left verbatim. Relative
    //    asset paths resolve against the extracted siblings under the default
    //    resolver context, which is exactly what the re-zipped package needs.
    const ArResolverContext context;
    int totalRemoved = 0;
    for (const std::string& name : fileOrder)
    {
        std::string ext = fs::path(name).extension().string();
        std::transform(ext.begin(), ext.end(), ext.begin(), ::tolower);
        if (ext != ".usd" && ext != ".usda" && ext != ".usdc")
            continue; // not a (plain) USD layer

        const fs::path entryPath = tmpDir / name;
        const CleanRefsResult res = CleanupLayerFileInPlace(
            entryPath.string(), anonOnly, dryRun, context, includeTextures);

        if (!res.ok)
        {
            IDTX_LOG(IDTX_WARN,
                "    Failed to process packaged entry '{}' - aborting, package "
                "left unchanged", name);
            return result; // ok == false
        }

        result.found += res.found;
        for (const DanglingArc& arc : res.arcs)
            IDTX_LOG(IDTX_WARN,
                "    dangling {} in package entry '{}' <{}> -> @{}@ ({})",
                ToString(arc.kind), name, arc.primPath.GetString(),
                arc.assetPath, ToString(arc.reason));

        totalRemoved += res.removed;
    }

    result.removed = totalRemoved;

    // 3) Nothing removed (or a dry-run): leave the original package untouched.
    if (dryRun || totalRemoved == 0)
    {
        result.ok = true;
        return result;
    }

    // 4) Re-zip every entry (original order) into a sibling temp package, then
    //    atomically swap it over the original.
    const fs::path newPkg = srcPath.parent_path() /
        ("." + srcPath.stem().string() + "_idtx_clean_new.usdz");
    fs::remove(newPkg, ec); // best-effort

    if (!WriteUsdzPackage(newPkg.string(), tmpDir, fileOrder))
    {
        fs::remove(newPkg, ec); // best-effort cleanup of partial output
        return result;
    }

    fs::remove(srcPath, ec);
    std::error_code moveEc;
    fs::rename(newPkg, srcPath, moveEc);
    if (moveEc)
    {
        std::error_code copyEc;
        fs::copy_file(newPkg, srcPath,
            fs::copy_options::overwrite_existing, copyEc);
        fs::remove(newPkg, ec);
        if (copyEc)
        {
            IDTX_LOG(IDTX_ERROR,
                "  Could not replace original package '{}': {}",
                usdzPath, copyEc.message());
            return result;
        }
    }

    IDTX_LOG(IDTX_INFO,
        "  Removed {} dangling arc(s) across package '{}'",
        totalRemoved, usdzPath);
    result.ok = true;
    return result;
}

// ---------------------------------------------------------------------------
// NormalizeUdimUsdzInPlace
// ---------------------------------------------------------------------------
UsdzUdimResult NormalizeUdimUsdzInPlace(
    const std::string& usdzPath,
    bool               dryRun)
{
    UsdzUdimResult result;

    const fs::path srcPath(usdzPath);

    // Unique hidden temporary working directory next to the package. Cleaned up
    // automatically by TempDirGuard.
    const fs::path tmpDir = srcPath.parent_path() /
        ("." + srcPath.stem().string() + "_idtx_udim_tmp");

    std::error_code ec;
    fs::remove_all(tmpDir, ec); // best-effort, may not exist
    fs::create_directories(tmpDir, ec);
    if (ec && !fs::exists(tmpDir))
    {
        IDTX_LOG(IDTX_WARN, "  Could not create temp dir '{}': {}",
            tmpDir.string(), ec.message());
        return result;
    }
    TempDirGuard guard(tmpDir);

    // 1) Extract every entry (preserving order). The SdfZipFile is released
    //    before we return from ExtractUsdzContents.
    std::vector<std::string> fileOrder;
    if (!ExtractUsdzContents(usdzPath, tmpDir, fileOrder))
        return result;

    // 2) Normalize each packaged USD layer entry in place inside the temp dir.
    //    Non-layer entries (textures, audio, ...) are left verbatim.
    int totalRewritten = 0;
    for (const std::string& name : fileOrder)
    {
        std::string ext = fs::path(name).extension().string();
        std::transform(ext.begin(), ext.end(), ext.begin(), ::tolower);
        if (ext != ".usd" && ext != ".usda" && ext != ".usdc")
            continue; // not a (plain) USD layer

        const fs::path entryPath = tmpDir / name;
        const UdimResult res =
            NormalizeUdimLayerFileInPlace(entryPath.string(), dryRun);

        if (!res.ok)
        {
            IDTX_LOG(IDTX_WARN,
                "    Failed to process packaged entry '{}' - aborting, package "
                "left unchanged", name);
            return result; // ok == false
        }

        result.found += res.found;
        for (const UdimRewrite& rw : res.rewrites)
            IDTX_LOG(IDTX_INFO,
                "    UDIM token in package entry '{}' <{}> @{}@ -> @{}@",
                name, rw.specPath.GetString(), rw.oldPath, rw.newPath);

        totalRewritten += res.rewritten;
    }

    result.rewritten = totalRewritten;

    // 3) Nothing rewritten (or a dry-run): leave the original package untouched.
    if (dryRun || totalRewritten == 0)
    {
        result.ok = true;
        return result;
    }

    // 4) Re-zip every entry (original order) into a sibling temp package, then
    //    atomically swap it over the original.
    const fs::path newPkg = srcPath.parent_path() /
        ("." + srcPath.stem().string() + "_idtx_udim_new.usdz");
    fs::remove(newPkg, ec); // best-effort

    if (!WriteUsdzPackage(newPkg.string(), tmpDir, fileOrder))
    {
        fs::remove(newPkg, ec); // best-effort cleanup of partial output
        return result;
    }

    fs::remove(srcPath, ec);
    std::error_code moveEc;
    fs::rename(newPkg, srcPath, moveEc);
    if (moveEc)
    {
        std::error_code copyEc;
        fs::copy_file(newPkg, srcPath,
            fs::copy_options::overwrite_existing, copyEc);
        fs::remove(newPkg, ec);
        if (copyEc)
        {
            IDTX_LOG(IDTX_ERROR,
                "  Could not replace original package '{}': {}",
                usdzPath, copyEc.message());
            return result;
        }
    }

    IDTX_LOG(IDTX_INFO,
        "  Rewrote {} UDIM token(s) across package '{}'",
        totalRewritten, usdzPath);
    result.ok = true;
    return result;
}

} // namespace idtx::utils