/**
 * @file UsdzRepackage.h
 * @brief Utility for re-packaging a (modified) USD stage back into a .usdz file.
 *
 * To re-package a usdz file after a stage modification we need to preserve every
 * non-root entry of the original package (textures, referenced layers, material files,
 * etc.) and only swap the root layer with the modified version.
 *
 * The implementation works directly with the low-level zip API exposed by Sdf:
 *   1. Open the source usdz with SdfZipFile and iterate every entry, writing
 *      each one out to a temporary directory while preserving the original
 *      ordering and relative paths.
 *   2. Overwrite the extracted root layer (the first entry of the usdz) with
 *      the modified stage.
 *   3. Optionally downsample image entries (textures) in place using the
 *      `TextureResize` helper - this is what gives LOD-tier usdz packages
 *      a meaningful filesize reduction (textures typically account for the
 *      vast majority of a model's payload).
 *   4. Build a new usdz with SdfZipFileWriter, adding the (modified) root
 *      layer first followed by every other entry in the original order.
 *   5. Clean up the temporary directory.
 *
 * On any failure the destination file is removed and the function returns
 * false so that the caller can fall back to an alternative export strategy.
 **/
#pragma once

#include <string>

#include <pxr/usd/usd/stage.h>

#include "utils/TextureResize.h"

namespace idtx::utils {

/**
 * @brief Re-packages a modified stage (originally opened from a usdz) into a
 *        new usdz file at @p dstUsdz, preserving every auxiliary asset
 *        (textures, referenced layers, ...) from the source package verbatim.
 *
 * @param srcUsdz  Path to the original .usdz file the stage was opened from.
 * @param stage    The (possibly modified) USD stage to be written as the new
 *                 root layer of the destination package.
 * @param dstUsdz  Path of the destination .usdz file to create.
 * @param texOpts  Optional texture downsampling options. Defaults to a no-op
 *                 (scale == 1.0); when scale < 1.0 every recognised image
 *                 file inside the package (excluding the root layer) is
 *                 decoded, resized and re-encoded before being written into
 *                 the destination package under the same in-package name.
 *
 * @return true on success. On failure the destination file is removed and the
 *         caller is expected to fall back to a plain layer export.
 */
bool RepackageUsdz(
    const std::string&                srcUsdz,
    const PXR_NS::UsdStageRefPtr&     stage,
    const std::string&                dstUsdz,
    const TextureResizeOptions&       texOpts = {});

// ---------------------------------------------------------------------------
// UpdateUsdzCratesInPlace
// ---------------------------------------------------------------------------
/// Outcome of a crate-version update pass over a single usdz package.
struct UsdzCrateUpdateResult
{
    /// true when the whole operation succeeded (even if nothing needed
    /// updating). false indicates the package could not be processed and was
    /// left untouched.
    bool ok = false;
    /// Number of packaged crate entries that were advanced to the current
    /// software version.
    int  updated = 0;
    /// Number of packaged crate entries that were already at the current
    /// version.
    int  upToDate = 0;
    /// Number of packaged entries that are not crate files (textures, text
    /// layers, audio, ...) and were left verbatim.
    int  nonCrate = 0;
};

/**
 * @brief Advance every deprecated binary crate entry inside a `.usdz` package
 *        to the current software crate version, in place.
 *
 * The package is unpacked to a temporary directory, each entry whose crate
 * version is older than the running software is re-serialized via
 * @ref UpdateCrateFileInPlace, and - if at least one entry was updated - the
 * package is re-zipped over the original, preserving every other entry
 * (textures, text layers, ...) and the original entry ordering verbatim.
 *
 * When no entry needs updating the original package is left byte-for-byte
 * untouched.
 *
 * @param usdzPath  Path to the `.usdz` package to update in place.
 * @param dryRun    When true, only inspect and report; the package is never
 *                  rewritten.
 * @return          A populated @ref UsdzCrateUpdateResult. On failure
 *                  @ref UsdzCrateUpdateResult::ok is false and the package is
 *                  unchanged.
 */
UsdzCrateUpdateResult UpdateUsdzCratesInPlace(
    const std::string& usdzPath,
    bool               dryRun);

// ---------------------------------------------------------------------------
// CleanupUsdzReferencesInPlace
// ---------------------------------------------------------------------------
/// Outcome of a dangling-arc cleanup pass over a single usdz package.
struct UsdzCleanupResult
{
    /// true when the whole operation succeeded (even if nothing needed
    /// cleaning). false indicates the package could not be processed and was
    /// left untouched.
    bool ok = false;
    /// Number of dangling arcs found across all packaged layer entries.
    int  found = 0;
    /// Number of dangling arcs removed (0 in dry-run).
    int  removed = 0;
};

/**
 * @brief Remove dangling reference/payload/sublayer arcs from every layer
 *        entry inside a `.usdz` package, in place.
 *
 * The package is unpacked to a temporary directory, each packaged USD layer is
 * cleaned via @ref CleanupLayerFileInPlace, and - if at least one arc was
 * removed - the package is re-zipped over the original, preserving every other
 * entry (textures, ...) and the original entry ordering verbatim. When no arc
 * needs removing the original package is left byte-for-byte untouched.
 *
 * @param usdzPath  Path to the `.usdz` package to clean in place.
 * @param anonOnly  Only remove anonymous-identifier arcs.
 * @param dryRun    When true, only inspect and report; never rewrite.
 * @param includeTextures  When true, also remove unresolvable asset-valued
 *                  attributes (textures) from packaged layers.
 * @return          A populated @ref UsdzCleanupResult. On failure
 *                  @ref UsdzCleanupResult::ok is false and the package is
 *                  unchanged.
 */
UsdzCleanupResult CleanupUsdzReferencesInPlace(
    const std::string& usdzPath,
    bool               anonOnly,
    bool               dryRun,
    bool               includeTextures = false);

// ---------------------------------------------------------------------------
// NormalizeUdimUsdzInPlace
// ---------------------------------------------------------------------------
/// Outcome of a UDIM-normalization pass over a single usdz package.
struct UsdzUdimResult
{
    /// true when the whole operation succeeded (even if nothing needed
    /// rewriting). false indicates the package could not be processed and was
    /// left untouched.
    bool ok = false;
    /// Number of asset paths with a non-standard UDIM token found across all
    /// packaged layer entries.
    int  found = 0;
    /// Number of asset paths rewritten to the canonical `<UDIM>` (0 in dry-run).
    int  rewritten = 0;
};

/**
 * @brief Normalize non-standard UDIM tokens in every layer entry inside a
 *        `.usdz` package to the canonical `<UDIM>`, in place.
 *
 * The package is unpacked to a temporary directory, each packaged USD layer is
 * normalized via @ref NormalizeUdimLayerFileInPlace, and - if at least one token
 * was rewritten - the package is re-zipped over the original, preserving every
 * other entry (textures, ...) and the original entry ordering verbatim. When no
 * token needs rewriting the original package is left byte-for-byte untouched.
 *
 * @param usdzPath  Path to the `.usdz` package to normalize in place.
 * @param dryRun    When true, only inspect and report; never rewrite.
 * @return          A populated @ref UsdzUdimResult. On failure
 *                  @ref UsdzUdimResult::ok is false and the package is unchanged.
 */
UsdzUdimResult NormalizeUdimUsdzInPlace(
    const std::string& usdzPath,
    bool               dryRun);

} // namespace idtx::utils