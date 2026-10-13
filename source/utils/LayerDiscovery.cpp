/**
 * @file LayerDiscovery.cpp
 * @brief Implementation of the composition-target discovery helper.
 *
 * See LayerDiscovery.h for the behavioural contract. The discovery mirrors the
 * logic that previously lived inside the updateversion command: open the stage
 * with all payloads loaded and walk GetUsedLayers(), classifying each layer by
 * its identifier into a plain on-disk file or an outermost `.usdz` package.
 **/

#include "utils/LayerDiscovery.h"

#include <algorithm>
#include <filesystem>
#include <system_error>

#include <pxr/pxr.h>
#include <pxr/usd/ar/packageUtils.h>
#include <pxr/usd/sdf/layer.h>
#include <pxr/usd/usd/stage.h>

#include <idtx/utils/Logger.h>

namespace fs = std::filesystem;
PXR_NAMESPACE_USING_DIRECTIVE

IDTX_LOG_CATEGORY("LayerDiscovery")

namespace idtx::utils {

namespace {

// Normalize a path to an absolute, lexically-cleaned string for de-duplication.
std::string NormalizePath(const std::string& p)
{
    std::error_code ec;
    fs::path abs = fs::weakly_canonical(fs::path(p), ec);
    if (ec || abs.empty())
    {
        abs = fs::absolute(fs::path(p), ec);
        if (ec)
            return p;
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
        if (!outer.empty())
            out.packages.insert(NormalizePath(outer));
        return;
    }

    std::string real = layer->GetRealPath();
    if (real.empty())
        real = id;
    if (!real.empty())
        out.plainFiles.insert(NormalizePath(real));
}

} // anonymous namespace

// ---------------------------------------------------------------------------
// CollectLayerTargets
// ---------------------------------------------------------------------------
void CollectLayerTargets(const std::string&         inputPath,
                         bool                        recursive,
                         DiscoveredLayers&           out,
                         ArResolverContext&          outContext)
{
    if (!recursive)
    {
        ClassifyFile(inputPath, out);
        return;
    }

    // Load everything (payloads included) so the full dependency graph is
    // visible to GetUsedLayers().
    UsdStageRefPtr stage = UsdStage::Open(inputPath, UsdStage::LoadAll);
    if (!stage)
    {
        IDTX_LOG(IDTX_WARN,
            "  Could not open stage '{}' for dependency discovery - will still "
            "attempt the file itself", inputPath);
        ClassifyFile(inputPath, out);
        return;
    }

    // Hand back the stage's resolver context so callers can resolve relative
    // asset paths the same way this stage does.
    outContext = stage->GetPathResolverContext();

    // GetUsedLayers returns every layer participating in composition: the root
    // layer, sublayers, referenced and payloaded layers and (optionally) clip
    // layers, including those packaged inside `.usdz` archives. Anonymous
    // layers (e.g. the session layer) have no on-disk form and are skipped.
    for (const SdfLayerHandle& layer : stage->GetUsedLayers(/*includeClipLayers=*/true))
    {
        if (!layer || layer->IsAnonymous())
            continue;
        ClassifyLayer(layer, out);
    }
    // The stage (and its layer refs) are released when this scope ends, so the
    // files are no longer memory-mapped by the time a caller rewrites them.
}

} // namespace idtx::utils
