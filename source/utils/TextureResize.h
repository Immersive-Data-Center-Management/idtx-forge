/**
 * @file TextureResize.h
 * @brief Utility for downsampling image files used as USD textures.
 *
 * Provides a single helper that decodes an in-memory image (PNG/JPEG/BMP/TGA/GIF),
 * downsamples it to a target dimension and re-encodes it in the same on-disk
 * format. Used by the usdz re-packager to shrink texture assets when emitting
 * LOD variants of a stage.
 *
 * The implementation is built on the public-domain `stb_image` / `stb_image_write` /
 * `stb_image_resize2` single-header libraries which are vendored under
 * `source/thirdparty/stb/`. No external dependency is introduced.
 **/
#pragma once

#include <cstdint>
#include <string>
#include <vector>

namespace idtx::utils {

/**
 * @brief Configuration knobs for `TryResizeImageFile`.
 *
 * A scale of 1.0 is treated as a no-op and instructs the caller to copy the
 * source bytes verbatim. Dimensions are always reduced (never upscaled).
 *
 * Aspect ratio is always preserved. Output dimensions are rounded *down* to
 * the nearest power of two (GPU-friendly mipmap chains), but never below
 * `minDimension` on either axis.
 */
struct TextureResizeOptions
{
    /// Linear scale factor for both dimensions. 1.0 == do not resize.
    float scale = 1.0f;

    /// Lower clamp for either output axis (in pixels) after scaling.
    int minDimension = 32;

    /// Re-encoding quality for JPEG output (1..100). Ignored for other formats.
    int jpegQuality = 85;
};

/**
 * @brief Decodes, resizes and re-encodes an image, if possible.
 *
 * If @p filename has an extension we recognise as an image (png/jpg/jpeg/bmp/
 * tga) AND the bytes successfully decode via stb_image, the image is
 * downsampled according to @p opts and re-encoded into @p dstBytes using the
 * same encoder family as the source extension.
 *
 * If the file is not a supported image, decoding fails, the source resolution
 * is already at-or-below the post-scale clamp, or @p opts.scale >= 1.0, the
 * function returns `false` and @p dstBytes is left untouched - the caller is
 * expected to copy the original bytes verbatim.
 *
 * @param filename   File name (used for extension detection and logging only).
 * @param srcBytes   Pointer to the source image bytes.
 * @param srcSize    Number of source bytes.
 * @param opts       Resize options.
 * @param dstBytes   Receives the re-encoded image on success.
 *
 * @return true if a resized re-encoded image was produced in @p dstBytes,
 *         false otherwise (caller should copy verbatim).
 */
bool TryResizeImageFile(
    const std::string&         filename,
    const void*                srcBytes,
    std::size_t                srcSize,
    const TextureResizeOptions& opts,
    std::vector<std::uint8_t>& dstBytes);

} // namespace idtx::utils