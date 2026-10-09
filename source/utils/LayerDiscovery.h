/**
 * @file LayerDiscovery.h
 * @brief Discover the set of on-disk layers and `.usdz` packages that
 *        participate in a USD stage's composition.
 *
 * Several in-place maintenance commands (crate-version update, dangling-arc
 * cleanup, ...) need the same first step: given an input stage, enumerate every
 * layer it depends on - the root layer, its sublayers, every referenced and
 * payloaded layer and any layers packed inside `.usdz` archives - de-duplicated
 * and split into plain on-disk files vs. `.usdz` packages. This utility factors
 * that discovery out so each command only has to decide what to *do* with the
 * discovered targets.
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
 * @brief Add every composition target reachable from @p inputPath to @p out.
 *
 * When @p recursive is true the stage is opened with every payload loaded and
 * all of its used layers (sublayers, references, payloads, clips - including
 * entries inside `.usdz` packages) are classified into @p out. When false, only
 * the named input file itself is classified. May be called repeatedly to merge
 * the targets of several inputs into the same @p out.
 *
 * @param inputPath  Path to the input USD stage.
 * @param recursive  Follow reference/payload/sublayer arcs when true.
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
