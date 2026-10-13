/**
 * @file UdimNormalize.h
 * @brief Detect and normalize non-standard UDIM tokens in asset-valued attribute
 *        values of a single USD layer, in place.
 *
 * OpenUSD's asset resolver only recognises the canonical `<UDIM>` token when
 * expanding a UDIM texture set (color.<UDIM>.exr -> color.1001.exr, ...). Some
 * DCCs and renderers instead write the printf / Python-dict style token
 * `%(UDIM)d` (e.g. `tex_%(UDIM)d_diff.png`) or a lower/mixed-case `<udim>`.
 * Those forms never expand, so the texture set silently fails to tile.
 *
 * This utility rewrites the following forms to the canonical `<UDIM>`:
 *   - `%(UDIM)d` and its field-width / zero-pad variants (`%(UDIM)04d`,
 *     `%(UDIM)4d`), with a case-insensitive `UDIM` key.
 *   - an existing `<udim>` token in any letter case (canonicalises to `<UDIM>`).
 *
 * Only the authored asset-path *string* is rewritten; the surrounding file name
 * is preserved verbatim (`tex_%(UDIM)d_diff.png` -> `tex_<UDIM>_diff.png`). An
 * already-correct `<UDIM>` token and non-asset attributes are left untouched.
 * The rewrite is a pure string transform and needs no asset resolver context.
 **/
#pragma once

#include <string>
#include <vector>

#include <pxr/pxr.h>
#include <pxr/usd/sdf/layer.h>
#include <pxr/usd/sdf/path.h>

namespace idtx::utils {

// ---------------------------------------------------------------------------
// UdimRewrite
// ---------------------------------------------------------------------------
/// A single asset-path string whose UDIM token was (or would be) normalized.
struct UdimRewrite
{
    PXR_NS::SdfPath specPath; ///< attribute spec carrying the asset path
    std::string     oldPath;  ///< authored asset path with a non-standard token
    std::string     newPath;  ///< the same path with the canonical `<UDIM>` token
};

// ---------------------------------------------------------------------------
// NormalizeUdimString
// ---------------------------------------------------------------------------
/**
 * @brief Rewrite any non-standard UDIM token in @p in to the canonical `<UDIM>`.
 *
 * @param in   The authored asset-path string to inspect.
 * @param out  Receives the normalized string (equal to @p in when nothing matched).
 * @return     true iff @p out differs from @p in (i.e. a token was rewritten).
 */
bool NormalizeUdimString(const std::string& in, std::string& out);

// ---------------------------------------------------------------------------
// FindUdimRewrites
// ---------------------------------------------------------------------------
/**
 * @brief Enumerate every asset-path string in @p layer that carries a
 *        non-standard UDIM token.
 *
 * The layer is inspected purely in memory; nothing is modified. All prim specs
 * are visited (including those nested inside variant sets); for each
 * asset-valued attribute both the default value and every time sample are
 * checked.
 *
 * @param layer  The layer to inspect.
 * @return       Every rewrite that would be applied (empty if the layer is clean).
 */
std::vector<UdimRewrite> FindUdimRewrites(const PXR_NS::SdfLayerHandle& layer);

// ---------------------------------------------------------------------------
// ApplyUdimRewrites
// ---------------------------------------------------------------------------
/**
 * @brief Rewrite every non-standard UDIM token in @p layer's asset-valued
 *        attribute values to the canonical `<UDIM>`, in memory.
 *
 * @param layer  The layer to edit.
 * @return       The number of asset-path strings actually rewritten.
 */
int ApplyUdimRewrites(const PXR_NS::SdfLayerHandle& layer);

// ---------------------------------------------------------------------------
// NormalizeUdimLayerFileInPlace
// ---------------------------------------------------------------------------
/// Outcome of a UDIM-normalization pass over a single on-disk layer file.
struct UdimResult
{
    bool                     ok        = false; ///< false => the file could not be processed
    int                      found     = 0;     ///< asset paths with a non-standard token
    int                      rewritten = 0;     ///< asset paths rewritten (0 in dry-run)
    std::vector<UdimRewrite> rewrites;          ///< every rewrite found (for reporting)
};

/**
 * @brief Detect and (unless dry-run) normalize UDIM tokens in a layer file,
 *        rewriting it in place.
 *
 * The file is opened, inspected and - when at least one non-standard token is
 * found and @p dryRun is false - re-serialized to a sibling temp file which then
 * atomically replaces the original. In dry-run mode the file is only inspected.
 *
 * @param path    Plain filesystem path to the layer (not a package-relative
 *                path; `.usdz` entries must be extracted first).
 * @param dryRun  When true, inspect and report only; never write.
 * @return        A populated result. @ref UdimResult::ok is false only if the
 *                layer could not be opened or the rewrite failed.
 */
UdimResult NormalizeUdimLayerFileInPlace(const std::string& path, bool dryRun);

} // namespace idtx::utils
