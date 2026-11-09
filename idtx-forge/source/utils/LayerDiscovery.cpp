/**
 * @file LayerDiscovery.cpp
 * @brief Implementation of the composition-target discovery helper.
 *
 * See LayerDiscovery.h for the behavioural contract. Discovery uses the same
 * dependency analysis the usdz packager relies on - UsdUtilsComputeAllDependencies
 * - which walks every composition arc in the raw layers (references, payloads,
 * sublayers and clips) across ALL variants and regardless of prim activation or
 * payload load state. Each discovered layer is classified by its identifier into
 * a plain on-disk file or an outermost `.usdz` package.
 *
 * This is deliberately broader than stage->GetUsedLayers(), which only returns
 * the layers that survive composition under the current variant selections and
 * active state. A layer reachable solely through an unselected variant (or a
 * deactivated prim) is not a "used layer", yet USD still opens it when packaging
 * - so the maintenance commands built on this helper (crate-version update,
 * dangling-arc cleanup) must see it too.
 **/

#include "utils/LayerDiscovery.h"

#include <algorithm>
#include <filesystem>
#include <system_error>
#include <string>
#include <vector>

#include <pxr/pxr.h>
#include <pxr/usd/ar/packageUtils.h>
#include <pxr/usd/ar/resolverContextBinder.h>
#include <pxr/usd/sdf/assetPath.h>
#include <pxr/usd/sdf/layer.h>
#include <pxr/usd/usd/stage.h>
#include <pxr/usd/usdUtils/dependencies.h>

#include <idtx/utils/Logger.h>

namespace fs = std::filesystem;
PXR_NAMESPACE_USING_DIRECTIVE

IDTX_LOG_CATEGORY("LayerDiscovery")

namespace idtx::utils
{

namespace
{

// Normalize a path to an absolute, lexically-cleaned string for de-duplication.
std::string NormalizePath(const std::string& p)
{
    std::error_code ec;
    fs::path abs = fs::weakly_canonical(fs::path(p), ec);
    if (ec || abs.empty())
    {
        abs = fs::absolute(fs::path(p), ec);
        if (ec) return p;
    }
    return abs.string();
}

std::string LowerExtension(const std::string& path)
{
    std::string ext = fs::path(path).extension().string();
    std::transform(ext.begin(), ext.end(), ext.begin(), ::tolower);
    return ext;
}

// Classify a bare input file path (used in non-recursive mode).
void ClassifyFile(const std::string& path, DiscoveredLayers& out)
{
    if (LowerExtension(path) == ".usdz")
        out.packages.insert(NormalizePath(path));
    else
        out.plainFiles.insert(NormalizePath(path));
}

// Classify a composed layer by its identifier: a package-relative identifier
// (an entry inside a `.usdz`) resolves to the outermost package file; any other
// identifier resolves to a plain on-disk layer.
void ClassifyLayer(const SdfLayerHandle& layer, DiscoveredLayers& out)
{
    const std::string id = layer->GetIdentifier();

    if (ArIsPackageRelativePath(id))
    {
        // Peel nested packages down to the outermost real file on disk.
        std::string outer = id;
        while (ArIsPackageRelativePath(outer))
            outer = ArSplitPackageRelativePathOuter(outer).first;
        if (!outer.empty()) out.packages.insert(NormalizePath(outer));
        return;
    }

    std::string real = layer->GetRealPath();
    if (real.empty()) real = id;
    if (!real.empty()) out.plainFiles.insert(NormalizePath(real));
}

} // anonymous namespace

// ---------------------------------------------------------------------------
// CollectLayerTargets
// ---------------------------------------------------------------------------
void CollectLayerTargets(const std::string& inputPath, bool recursive, DiscoveredLayers& out,
                         ArResolverContext& outContext)
{
    if (!recursive)
    {
        ClassifyFile(inputPath, out);
        return;
    }

    // Open the stage only to capture the resolver context callers use to resolve
    // relative asset paths exactly as the stage would. LoadNone keeps this cheap:
    // the dependency analysis below reads the raw layers directly and needs no
    // payloads composed.
    UsdStageRefPtr stage = UsdStage::Open(inputPath, UsdStage::LoadNone);
    if (!stage)
    {
        IDTX_LOG(IDTX_WARN,
                 "  Could not open stage '{}' for dependency discovery - will still "
                 "attempt the file itself",
                 inputPath);
        ClassifyFile(inputPath, out);
        return;
    }

    // Hand back the stage's resolver context so callers can resolve relative
    // asset paths the same way this stage does.
    outContext = stage->GetPathResolverContext();

    // Discover dependencies the way the usdz packager does. Unlike
    // GetUsedLayers(), UsdUtilsComputeAllDependencies walks every composition
    // arc in the raw layers - including arcs inside unselected variants and
    // under deactivated prims - so layers that never survive composition (but
    // that USD still opens when packaging) are discovered here as well.
    //
    // 'layers' holds every dependency openable as an SdfLayer; 'assets' holds
    // non-layer dependencies (textures, ...) and 'unresolved' the paths that
    // could not be resolved. Both commands operate on layers only, so the latter
    // two are intentionally ignored.
    std::vector<SdfLayerRefPtr> layers;
    std::vector<std::string> assets;
    std::vector<std::string> unresolved;
    {
        ArResolverContextBinder binder(outContext);
        UsdUtilsComputeAllDependencies(SdfAssetPath(inputPath), &layers, &assets, &unresolved);
    }

    // On success the input layer itself is included in 'layers'. If analysis
    // yielded nothing (e.g. the asset could not be resolved as a layer), fall
    // back to classifying the named input so the caller still processes it.
    if (layers.empty())
    {
        ClassifyFile(inputPath, out);
        return;
    }

    for (const SdfLayerRefPtr& layer: layers)
    {
        if (!layer || layer->IsAnonymous()) continue;
        ClassifyLayer(layer, out);
    }
    // The stage (and the discovered layer refs) are released when this scope
    // ends, so the files are no longer memory-mapped by the time a caller
    // rewrites them.
}

} // namespace idtx::utils
