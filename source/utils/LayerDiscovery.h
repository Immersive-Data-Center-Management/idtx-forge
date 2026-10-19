/**
 * @file LayerDiscovery.h
 * @brief Discover the set of on-disk layers and `.usdz` packages that a USD
 *        stage depends on.
 *
 * Several in-place maintenance commands (crate-version update, dangling-arc
 * cleanup, ...) need the same first step: given an input stage, enumerate every
 * layer it depends on - the root layer, its sublayers, every referenced and
 * payloaded layer, clip layers and any layers packed inside `.usdz` archives -
 * de-duplicated and split into plain on-disk files vs. `.usdz` packages. This
 * utility factors that discovery out so each command only has to decide what to
 * *do* with the discovered targets.
 *
 * Discovery is intentionally broader than a stage's composed layer set: it uses
 * the same dependency analysis the usdz packager relies on, so it also finds
 * layers reachable only through an *unselected* variant or a *deactivated* prim.
 * Those layers never survive composition, yet USD still opens them when
 * packaging - so a maintenance pass must cover them too.
 **/
#pragma once

#include <set>
#include <string>

#include <pxr/pxr.h>
#include <pxr/usd/ar/resolverContext.h>

namespace idtx::utils {

// ---------------------------------------------------------------------------
// DiscoveredLayers
// ---------------------------------------------------------------------------
/// The de-duplicated set of composition targets reachable from one or more
/// input stages. Paths are normalized to absolute, lexically-cleaned strings.
struct DiscoveredLayers
{
    std::set<std::string> plainFiles;  ///< on-disk layers (any format, non-package)
    std::set<std::string> packages;    ///< `.usdz` archives
};

// ---------------------------------------------------------------------------
// CollectLayerTargets
// ---------------------------------------------------------------------------
/**
 * @brief Add every layer the stage at @p inputPath depends on to @p out.
 *
 * When @p recursive is true the dependencies are gathered via the packager's
 * dependency analysis (UsdUtilsComputeAllDependencies): every referenced,
 * payloaded, sublayered and clip layer the stage can reach - across all variants
 * and regardless of prim activation or payload load state - including entries
 * inside `.usdz` packages, is classified into @p out. When false, only the named
 * input file itself is classified. May be called repeatedly to merge the targets
 * of several inputs into the same @p out.
 *
 * @param inputPath  Path to the input USD stage.
 * @param recursive  Gather the full dependency set when true; classify only the
 *                   named input when false.
 * @param out        Accumulating target set (merged across calls).
 * @param outContext Receives the stage's path-resolver context (when the stage
 *                   opened), so callers can resolve relative asset paths exactly
 *                   as the stage would. Left untouched if the stage failed to
 *                   open or @p recursive is false.
 */
void CollectLayerTargets(const std::string&            inputPath,
                         bool                          recursive,
                         DiscoveredLayers&             out,
                         PXR_NS::ArResolverContext&    outContext);

} // namespace idtx::utils
