/**
 * @file CrateUpdate.h
 * @brief Detect and update the crate-file version of binary USD layers.
 *
 * OpenUSD binary files (`.usdc`, and `.usd`/`.usdz` entries encoded as crate)
 * carry a crate-file version. OpenUSD classifies these versions in three bands
 * (see pxr/usd/sdf/crateFile.cpp):
 *   - below OLDEST_SUPPORTED_VERSION ("0.0.1") the file is *obsolete* and fails
 *     to open at all - updating it requires older OpenUSD software and is out of
 *     scope here;
 *   - at or above OLDEST_SUPPORTED_VERSION but below OLDEST_CURRENT_VERSION
 *     ("0.8.0") the file is *deprecated*: it still opens, but with a warning,
 *     and future OpenUSD releases may drop support for it;
 *   - at or above OLDEST_CURRENT_VERSION the file is current.
 *
 * This utility updates *deprecated* files to a current version, mirroring what
 * the `usdupdatecrate` tool does by default (its default threshold is also
 * "0.8.0") but through the OpenUSD C++ API, so no external tool is required.
 * The migration mechanism matches `usdupdatecrate --update`: the layer is opened
 * and *re-exported*, which makes OpenUSD write it at the current crate version
 * for newly created files. (A plain SdfLayer::Save() deliberately *preserves* a
 * file's existing version and therefore cannot migrate it.)
 *
 * Non-crate inputs (e.g. `.usda` text layers) have no crate version; the
 * inspector reports @ref CrateVersionInfo::isCrate == false for them and the
 * caller is expected to leave them untouched.
 **/
#pragma once

#include <string>

namespace idtx::utils {

// ---------------------------------------------------------------------------
// CrateDeprecationThreshold
// ---------------------------------------------------------------------------
/**
 * @brief The crate version below which a file is considered deprecated.
 *
 * Mirrors OpenUSD's `OLDEST_CURRENT_VERSION` (and `usdupdatecrate`'s default
 * `--version` threshold). A crate file whose version is strictly older than
 * this is a candidate for updating. This constant tracks the vendored OpenUSD
 * release and should be revisited when that release changes.
 */
const char* CrateDeprecationThreshold();

// ---------------------------------------------------------------------------
// CrateVersionInfo
// ---------------------------------------------------------------------------
/// Result of inspecting a single file for its crate-file version.
struct CrateVersionInfo
{
    /// true when the file is a binary crate file (i.e. SdfCrateInfo could open
    /// it). Text layers (`.usda`) and non-USD files report false.
    bool        isCrate = false;
    /// The crate version stored in the file, e.g. "0.7.0" (empty when
    /// @ref isCrate is false).
    std::string fileVersion;
    /// The newest crate version the currently linked OpenUSD software can
    /// write, e.g. "0.15.0" (empty when @ref isCrate is false). Reported for
    /// context only; it is *not* the update target - OpenUSD writes the lowest
    /// version that can represent the data.
    std::string softwareVersion;
    /// true when @ref isCrate and @ref fileVersion is older than
    /// @ref CrateDeprecationThreshold - i.e. the file is deprecated and a
    /// candidate for updating.
    bool        isOutdated = false;
};

// ---------------------------------------------------------------------------
// InspectCrateVersion
// ---------------------------------------------------------------------------
/**
 * @brief Read the crate-file version of @p path without modifying it.
 *
 * @param path  Path to an on-disk file (any format).
 * @return      A populated @ref CrateVersionInfo. For non-crate files the
 *              result has @ref CrateVersionInfo::isCrate == false and the
 *              remaining fields left at their defaults.
 */
CrateVersionInfo InspectCrateVersion(const std::string& path);

// ---------------------------------------------------------------------------
// CompareCrateVersions
// ---------------------------------------------------------------------------
/**
 * @brief Compare two dotted crate version strings component-wise ("a.b.c").
 *
 * Missing trailing components are treated as zero so that "0.8" == "0.8.0".
 * Non-numeric components are treated as zero.
 *
 * @return A negative value if @p a < @p b, 0 if equal, positive if @p a > @p b.
 */
int CompareCrateVersions(const std::string& a, const std::string& b);

// ---------------------------------------------------------------------------
// UpdateCrateFileInPlace
// ---------------------------------------------------------------------------
/**
 * @brief Migrate a single on-disk deprecated crate layer to a current version,
 *        in place.
 *
 * Follows the same safe sequence as `usdupdatecrate --update`: open the layer,
 * re-export it to a sibling temporary file (which OpenUSD writes at the current
 * crate version), verify the temp file is no longer deprecated, then atomically
 * replace the original. The original is left untouched on any failure. Re-export
 * (not Save) is required because Save preserves a file's existing crate version.
 *
 * @param path  Path to the crate layer to update. Must be a plain filesystem
 *              path (not a package-relative path inside a `.usdz`); callers that
 *              need to update entries inside a package extract them first.
 * @return      true on success, false if the layer could not be opened,
 *              re-exported, verified or swapped into place.
 */
bool UpdateCrateFileInPlace(const std::string& path);

} // namespace idtx::utils
