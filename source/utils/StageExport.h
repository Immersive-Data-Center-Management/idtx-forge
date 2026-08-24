/**
 * @file StageExport.h
 * @brief Helper that writes a (possibly modified) USD stage to an output directory.
 *
 * For `.usdz` inputs the helper preserves all auxiliary assets (textures, sub-layers,
 * ...) via `idtx::utils::RepackageUsdz`. The optional @p texOpts parameter is
 * forwarded to the repackager so callers can request texture downsampling at the
 * same time (e.g. LOD generation). For non-`.usdz` inputs @p texOpts is ignored.
 **/
#pragma once

#include <pxr/usd/usd/stage.h>

#include "utils/TextureResize.h"

bool ExportStage(
    const pxr::UsdStageRefPtr &stage,
    const std::string &inputPath,
    const std::string &outputDir,
    const std::string &outputSuffix,
    const idtx::utils::TextureResizeOptions &texOpts = {});