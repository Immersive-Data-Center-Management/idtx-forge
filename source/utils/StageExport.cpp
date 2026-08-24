#include "utils/StageExport.h"

#include <filesystem>
#include <string>

#include "utils/UsdzRepackage.h"
#include "idtx/utils/Logger.h"


namespace fs = std::filesystem;
PXR_NAMESPACE_USING_DIRECTIVE

bool ExportStage(
    const pxr::UsdStageRefPtr &stage,
    const std::string &inputPath,
    const std::string &outputDir,
    const std::string &outputSuffix,
    const idtx::utils::TextureResizeOptions &texOpts)
{
    // Output path: <outputDir>/<stem><suffix>.<ext>
    const fs::path inPath(inputPath);
    const std::string stem = inPath.stem().string() + outputSuffix;
    const std::string ext  = inPath.extension().string();
    std::string lowerExt   = ext;
    std::transform(lowerExt.begin(), lowerExt.end(), lowerExt.begin(), ::tolower);

    // For .usdz inputs we re-package the triangulated stage together with all
    // auxiliary assets (textures, referenced layers, ...) from the source
    // package so that nothing gets lost. If re-packaging fails for any reason
    // we fall back to the legacy lossy behavior of writing a plain .usdc file
    // so that the user still gets a usable result.
    //
    // The optional @p texOpts is only meaningful for .usdz - it is forwarded
    // to RepackageUsdz so that texture downsampling happens in the same pass
    // as the asset re-packaging. The .usdc fallback path cannot embed
    // textures anyway, so passing texOpts to it would be pointless.
    if (lowerExt == ".usdz")
    {
        const fs::path outPath = fs::path(outputDir) / (stem + ".usdz");
        if (idtx::utils::RepackageUsdz(inputPath, stage, outPath.string(), texOpts))
        {
            IDTX_LOGF(IDTX_INFO, "  Exported to '{}'", outPath.string());
            return true;
        }

        IDTX_LOGF(IDTX_WARN,
            "  usdz re-packaging failed - falling back to .usdc (referenced "
            "assets such as textures will not be included)");

        const fs::path fallbackPath = fs::path(outputDir) / (stem + ".usdc");
        if (!stage->Export(fallbackPath.string()))
        {
            IDTX_LOGF(IDTX_ERROR,
                "  Failed to export stage to '{}'",
                fallbackPath.string());
            return false;
        }
        IDTX_LOGF(IDTX_INFO, "  Exported to '{}'", fallbackPath.string());
        return true;
    }

    const fs::path outPath = fs::path(outputDir) / (stem + ext);
    if (!stage->Export(outPath.string()))
    {
        IDTX_LOGF(IDTX_ERROR, "  Failed to export stage to '{}'",
            outPath.string());
        return false;
    }
    
    IDTX_LOGF(IDTX_INFO, "  Exported to '{}'", outPath.string());
    return true;
}