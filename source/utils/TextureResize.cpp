/**
 * @file TextureResize.cpp
 * @brief Implementation of texture downsampling for usdz LOD generation.
 *
 * The three stb single-header libraries are *implemented* (not just declared)
 * in this translation unit by defining their respective IMPLEMENTATION macros
 * before including the headers. This file is therefore the sole place where
 * stb_image*.h are instantiated; including the headers elsewhere without the
 * IMPLEMENTATION macro is safe.
 **/

#include "utils/TextureResize.h"

#include <algorithm>
#include <cctype>
#include <cstdint>
#include <cstring>
#include <string>
#include <vector>

// stb_image: decoders for PNG/JPEG/BMP/TGA/GIF/HDR/PSD/PIC.
#define STB_IMAGE_IMPLEMENTATION
#define STBI_NO_HDR    // we never need float HDR decode for texture LODs
#define STBI_NO_LINEAR // we operate in 8-bit sRGB; skip the linear-float path
#define STBI_NO_PSD
#define STBI_NO_PIC
#define STBI_NO_PNM
#include "thirdparty/stb/stb_image.h"

// stb_image_write: encoders for PNG/JPEG/BMP/TGA.
#define STB_IMAGE_WRITE_IMPLEMENTATION
#include "thirdparty/stb/stb_image_write.h"

// stb_image_resize2: high quality SIMD resampler.
#define STB_IMAGE_RESIZE_IMPLEMENTATION
#include "thirdparty/stb/stb_image_resize2.h"

#include <idtx/utils/Logger.h>

IDTX_LOG_CATEGORY("TextureResize")

namespace idtx::utils {

namespace {

/// Returns the file extension (without the dot), lower-cased. Empty if none.
std::string LowerExtension(const std::string& filename)
{
    const auto dot = filename.find_last_of('.');
    if (dot == std::string::npos || dot + 1 >= filename.size()) return {};
    std::string ext = filename.substr(dot + 1);
    std::transform(ext.begin(), ext.end(), ext.begin(),
        [](unsigned char c) { return static_cast<char>(std::tolower(c)); });
    return ext;
}

/// Highest power of two that is <= @p v. Returns 1 for v <= 1.
int FloorPowerOfTwo(int v)
{
    if (v < 2) return 1;
    int p = 1;
    while ((p << 1) <= v) p <<= 1;
    return p;
}

enum class Encoder { Png, Jpeg, Bmp, Tga, Unsupported };

Encoder EncoderFromExtension(const std::string& extLower)
{
    if (extLower == "png")                       return Encoder::Png;
    if (extLower == "jpg" || extLower == "jpeg") return Encoder::Jpeg;
    if (extLower == "bmp")                       return Encoder::Bmp;
    if (extLower == "tga")                       return Encoder::Tga;
    return Encoder::Unsupported;
}

/// stb_image_write callback: append bytes to a std::vector<uint8_t>.
void WriteToVector(void* ctx, void* data, int size)
{
    auto* vec = static_cast<std::vector<std::uint8_t>*>(ctx);
    if (size > 0 && data) {
        const auto* p = static_cast<const std::uint8_t*>(data);
        vec->insert(vec->end(), p, p + size);
    }
}

} // anonymous namespace

bool TryResizeImageFile(
    const std::string&          filename,
    const void*                 srcBytes,
    std::size_t                 srcSize,
    const TextureResizeOptions& opts,
    std::vector<std::uint8_t>&  dstBytes)
{
    if (opts.scale >= 1.0f) return false;          // explicit no-op
    if (!srcBytes || srcSize == 0) return false;

    const std::string ext = LowerExtension(filename);
    const Encoder enc = EncoderFromExtension(ext);
    if (enc == Encoder::Unsupported) {
        // Not an image format we know how to write back. Caller will copy verbatim.
        return false;
    }

    // ---- Decode ----------------------------------------------------------
    int srcW = 0, srcH = 0, srcChannels = 0;
    // Force 4 channels on load: simpler resize/encode path and PNG transparency
    // is preserved. JPEG encoder still emits 3 channels via reqComp parameter.
    constexpr int kDecodeChannels = 4;
    stbi_uc* decoded = stbi_load_from_memory(
        static_cast<const stbi_uc*>(srcBytes), static_cast<int>(srcSize),
        &srcW, &srcH, &srcChannels, kDecodeChannels);
    if (!decoded) {
        IDTX_LOG(IDTX_DEBUG, "    '{}': not a decodable image ({}), copying verbatim",
            filename, stbi_failure_reason() ? stbi_failure_reason() : "unknown");
        return false;
    }

    // ---- Target dimensions ---------------------------------------------
    int dstW = std::max(opts.minDimension,
        static_cast<int>(std::lround(srcW * opts.scale)));
    int dstH = std::max(opts.minDimension,
        static_cast<int>(std::lround(srcH * opts.scale)));

    // Always round *down* to a power of two on each axis - GPU mipmap friendly.
    dstW = FloorPowerOfTwo(dstW);
    dstH = FloorPowerOfTwo(dstH);

    // Re-clamp after PoT rounding in case the floor pushed us below the floor.
    dstW = std::max(dstW, std::min(srcW, opts.minDimension));
    dstH = std::max(dstH, std::min(srcH, opts.minDimension));

    // Never upscale.
    if (dstW >= srcW && dstH >= srcH) {
        stbi_image_free(decoded);
        IDTX_LOG(IDTX_DEBUG,
            "    '{}': already at-or-below target ({}x{} -> {}x{}), copying verbatim",
            filename, srcW, srcH, dstW, dstH);
        return false;
    }

    // ---- Resize ---------------------------------------------------------
    std::vector<std::uint8_t> resized(
        static_cast<std::size_t>(dstW) * dstH * kDecodeChannels);

    // stbir_resize_uint8_srgb treats the buffer as sRGB-encoded 8-bit RGBA,
    // which is the correct color-space assumption for the vast majority of
    // texture files inside a usdz (baseColor/emissive/etc.). For normal/
    // roughness/metallic maps this is technically incorrect but the visual
    // impact at LOD scales is negligible and matches what most game engines
    // do at mip generation time.
    unsigned char* ok = stbir_resize_uint8_srgb(
        decoded, srcW, srcH, /*input_stride=*/0,
        resized.data(), dstW, dstH, /*output_stride=*/0,
        STBIR_RGBA);

    stbi_image_free(decoded);
    if (!ok) {
        IDTX_LOG(IDTX_WARN, "    '{}': stbir_resize failed, copying verbatim", filename);
        return false;
    }

    // ---- Encode ---------------------------------------------------------
    dstBytes.clear();
    int writeOk = 0;
    switch (enc) {
        case Encoder::Png:
            writeOk = stbi_write_png_to_func(
                &WriteToVector, &dstBytes, dstW, dstH, kDecodeChannels,
                resized.data(), /*stride_in_bytes=*/dstW * kDecodeChannels);
            break;

        case Encoder::Jpeg: {
            // JPEG cannot store alpha; the encoder needs RGB-only input.
            std::vector<std::uint8_t> rgb(static_cast<std::size_t>(dstW) * dstH * 3);
            for (int i = 0, n = dstW * dstH; i < n; ++i) {
                rgb[3*i + 0] = resized[4*i + 0];
                rgb[3*i + 1] = resized[4*i + 1];
                rgb[3*i + 2] = resized[4*i + 2];
            }
            const int q = std::clamp(opts.jpegQuality, 1, 100);
            writeOk = stbi_write_jpg_to_func(
                &WriteToVector, &dstBytes, dstW, dstH, 3, rgb.data(), q);
            break;
        }

        case Encoder::Bmp:
            writeOk = stbi_write_bmp_to_func(
                &WriteToVector, &dstBytes, dstW, dstH, kDecodeChannels,
                resized.data());
            break;

        case Encoder::Tga:
            writeOk = stbi_write_tga_to_func(
                &WriteToVector, &dstBytes, dstW, dstH, kDecodeChannels,
                resized.data());
            break;

        case Encoder::Unsupported:
            break;
    }

    if (!writeOk || dstBytes.empty()) {
        IDTX_LOG(IDTX_WARN, "    '{}': image re-encode failed, copying verbatim", filename);
        dstBytes.clear();
        return false;
    }

    IDTX_LOG(IDTX_DEBUG,
        "    '{}': {}x{} ({} bytes) -> {}x{} ({} bytes, {:.1f}%)",
        filename, srcW, srcH, srcSize, dstW, dstH, dstBytes.size(),
        srcSize > 0 ? (100.0 * dstBytes.size() / srcSize) : 0.0);
    return true;
}

} // namespace idtx::utils