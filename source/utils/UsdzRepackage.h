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

} // namespace idtx::utils