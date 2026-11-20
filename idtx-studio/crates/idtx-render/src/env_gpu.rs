//! GPU-side `UsdLuxDomeLight` environment: float equirect decode, mip upload, and
//! spherical-harmonic projection.
//!
//! Bridges the GPU-free [`crate::env_ibl`] math to wgpu:
//! * [`decode_env_equirect`] reads an HDR/EXR/LDR equirectangular file into a
//!   linear float [`EnvImage`] (never clamped to 8-bit, unlike the material
//!   texture path).
//! * [`upload_env`] uploads it as a filterable `Rgba16Float` texture with a
//!   box-filtered mip chain — the roughness-prefilter approximation the shader
//!   samples for specular IBL.
//! * The returned [`GpuEnv`] also carries the CPU-projected SH diffuse
//!   coefficients and average radiance (the dome's "actual" ambient colour).

use crate::convert::{AsF32, f32_to_u32, u32_sat};
use crate::env_ibl::{EnvImage, ShL2, project_sh_l2};
use glam::Vec3;
use half::f16;

/// A GPU-resident environment map plus its CPU-derived lighting summary.
#[allow(missing_debug_implementations)]
pub struct GpuEnv {
    /// `Rgba16Float` equirect texture view with a full mip chain.
    pub view: wgpu::TextureView,
    /// Number of mip levels (`>= 1`); `max_mip = mip_count - 1`.
    pub mip_count: u32,
    /// Order-2 SH radiance coefficients for directional diffuse irradiance.
    pub sh: ShL2,
    /// Solid-angle-weighted average radiance (the ambient colour the dome adds).
    pub avg: Vec3,
    pub bytes: u64,
}

/// Maximum equirect height retained for SH projection. SH is a smooth 9-coeff
/// fit, so a heavily downsampled copy yields identical coefficients far cheaper
/// than projecting a multi-megapixel source.
const SH_MAX_HEIGHT: u32 = 64;

/// Decode an HDR / EXR / LDR equirectangular environment into linear float RGB.
///
/// `reader` is the optional USD asset resolver (for textures packed inside a
/// `.usdz`); when it returns `None` the path is read from the filesystem.
pub fn decode_env_equirect(
    path: &str,
    reader: Option<&(dyn Fn(&str) -> Option<Vec<u8>> + Send + Sync)>,
) -> Result<EnvImage, String> {
    decode_env_bounded(path, reader, crate::TextureDecodeConfig::default())
}

pub(crate) fn decode_env_bounded(
    path: &str,
    reader: Option<&(dyn Fn(&str) -> Option<Vec<u8>> + Send + Sync)>,
    config: crate::TextureDecodeConfig,
) -> Result<EnvImage, String> {
    let bytes = crate::texture_cache::read_bounded(path, reader, config.max_decode_bytes)?;
    crate::texture_cache::validate_decode(&bytes, config.max_decode_bytes, config.max_dimension)?;

    // The `image` crate decodes Radiance `.hdr` and many EXRs to float directly.
    // Its OpenEXR backend rejects some channel layouts, so `.exr` falls back to
    // the `exr` crate (arbitrary channels), matching the material texture path.
    let env = match image::load_from_memory(&bytes) {
        Ok(dynamic) => {
            let rgb = dynamic.into_rgb32f();
            let (w, h) = rgb.dimensions();
            let texels = rgb.pixels().map(|p| [p[0], p[1], p[2]]).collect();
            Ok(EnvImage::new(w, h, texels))
        }
        Err(e) if is_exr(path) => {
            decode_exr_env(&bytes).map_err(|e2| format!("image: {e}; exr fallback: {e2}"))
        }
        Err(e) => Err(e.to_string()),
    }?;
    let cap = config.max_dimension.unwrap_or(u32::MAX).max(1);
    if env.width.max(env.height) <= cap { return Ok(env); }
    let scale = f64::from(cap) / f64::from(env.width.max(env.height));
    let w = ((f64::from(env.width) * scale) as u32).max(1);
    let h = ((f64::from(env.height) * scale) as u32).max(1);
    let mut texels = Vec::with_capacity(w as usize * h as usize);
    for y in 0..h { for x in 0..w {
        let sx = u64::from(x) * u64::from(env.width) / u64::from(w);
        let sy = u64::from(y) * u64::from(env.height) / u64::from(h);
        texels.push(env.texels[(sy * u64::from(env.width) + sx) as usize]);
    } }
    Ok(EnvImage::new(w, h, texels))
}

/// True for `OpenEXR` source files by extension.
fn is_exr(path: &str) -> bool {
    path.rsplit('.')
        .next()
        .unwrap_or("")
        .eq_ignore_ascii_case("exr")
}

/// Decode an `OpenEXR` file to linear float RGB via the `exr` crate, preserving the
/// full dynamic range (no `[0,1]` clamp — this is environment radiance).
fn decode_exr_env(bytes: &[u8]) -> Result<EnvImage, String> {
    use exr::prelude::*;

    let cursor = std::io::Cursor::new(bytes);
    let image = read()
        .no_deep_data()
        .largest_resolution_level()
        .all_channels()
        .first_valid_layer()
        .all_attributes()
        .from_buffered(cursor)
        .map_err(|e| e.to_string())?;

    let layer = &image.layer_data;
    let (width, height) = (layer.size.width(), layer.size.height());
    if width == 0 || height == 0 {
        return Err("exr layer has zero area".to_owned());
    }
    let channels = &layer.channel_data.list;
    if channels.is_empty() {
        return Err("exr layer has no channels".to_owned());
    }

    let find = |name: &str| {
        channels
            .iter()
            .position(|c| c.name.to_string().eq_ignore_ascii_case(name))
    };
    // Named RGB, else replicate channel 0 (single-channel env → grayscale).
    let (r_idx, g_idx, b_idx) = match (find("R"), find("G"), find("B")) {
        (Some(r), Some(g), Some(b)) => (r, g, b),
        _ => (0, 0, 0),
    };
    let sample = |ch: usize, x: usize, y: usize| -> f32 {
        channels[ch]
            .sample_data
            .value_by_flat_index(y * width + x)
            .to_f32()
    };

    let mut texels = Vec::with_capacity(width * height);
    for y in 0..height {
        for x in 0..width {
            texels.push([
                sample(r_idx, x, y),
                sample(g_idx, x, y),
                sample(b_idx, x, y),
            ]);
        }
    }
    Ok(EnvImage::new(u32_sat(width), u32_sat(height), texels))
}

/// Project the environment onto SH coefficients, downsampling first so the CPU
/// cost is bounded regardless of source resolution.
#[must_use]
pub fn project_sh(env: &EnvImage) -> ShL2 {
    if env.height > SH_MAX_HEIGHT {
        project_sh_l2(&downsample_to_height(env, SH_MAX_HEIGHT))
    } else {
        project_sh_l2(env)
    }
}

/// Box-filter an equirect down to (at most) `target_h` rows, preserving the 2:1
/// aspect ratio. Cheap nearest-block average — adequate for the smooth SH fit.
fn downsample_to_height(env: &EnvImage, target_h: u32) -> EnvImage {
    let target_h = target_h.max(1);
    let target_w = (target_h * 2).max(1);
    if env.width <= target_w || env.height <= target_h {
        return env.clone();
    }
    let mut texels = vec![[0.0f32; 3]; (target_w * target_h) as usize];
    let sx = env.width.as_f32() / target_w.as_f32();
    let sy = env.height.as_f32() / target_h.as_f32();
    for ty in 0..target_h {
        let y0 = f32_to_u32(ty.as_f32() * sy);
        let y1 = f32_to_u32((ty + 1).as_f32() * sy)
            .min(env.height)
            .max(y0 + 1);
        for tx in 0..target_w {
            let x0 = f32_to_u32(tx.as_f32() * sx);
            let x1 = f32_to_u32((tx + 1).as_f32() * sx)
                .min(env.width)
                .max(x0 + 1);
            let mut acc = Vec3::ZERO;
            let mut n = 0.0f32;
            for y in y0..y1 {
                for x in x0..x1 {
                    let t = env.texels[(y * env.width + x) as usize];
                    acc += Vec3::new(t[0], t[1], t[2]);
                    n += 1.0;
                }
            }
            let avg = if n > 0.0 { acc / n } else { Vec3::ZERO };
            texels[(ty * target_w + tx) as usize] = [avg.x, avg.y, avg.z];
        }
    }
    EnvImage::new(target_w, target_h, texels)
}

/// Upload an environment as a filterable `Rgba16Float` texture with a
/// box-filtered mip chain. Returns the view and mip count.
#[must_use]
pub fn upload_env(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    env: &EnvImage,
) -> (wgpu::TextureView, u32) {
    let mips = build_f16_mip_chain(env);
    let mip_count = u32_sat(mips.len());
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("DomeLight Env"),
        size: wgpu::Extent3d {
            width: env.width,
            height: env.height,
            depth_or_array_layers: 1,
        },
        mip_level_count: mip_count,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba16Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    for (level, (w, h, data)) in mips.iter().enumerate() {
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: u32_sat(level),
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(data),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(w * 8), // 4 channels × 2 bytes (f16)
                rows_per_image: Some(*h),
            },
            wgpu::Extent3d {
                width: *w,
                height: *h,
                depth_or_array_layers: 1,
            },
        );
    }
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    (view, mip_count)
}

/// Build a half-float RGBA mip chain from a float equirect. Each level is a 2×2
/// box average of the previous — the specular prefilter approximation. Texels are
/// packed as raw `f16` bit patterns (`u16`), which are `Pod` for GPU upload.
fn build_f16_mip_chain(env: &EnvImage) -> Vec<(u32, u32, Vec<u16>)> {
    // Pack one linear RGB texel to RGBA f16 bits (alpha = 1.0).
    let pack = |t: &[f32; 3]| -> [u16; 4] {
        [
            f16::from_f32(t[0]).to_bits(),
            f16::from_f32(t[1]).to_bits(),
            f16::from_f32(t[2]).to_bits(),
            f16::from_f32(1.0).to_bits(),
        ]
    };

    let mut levels: Vec<(u32, u32, Vec<u16>)> = Vec::new();
    // Level 0.
    let base: Vec<u16> = env.texels.iter().flat_map(&pack).collect();
    levels.push((env.width, env.height, base));

    let (mut w, mut h) = (env.width, env.height);
    // Keep the previous level's float data around for accurate averaging.
    let mut prev_f32: Vec<[f32; 3]> = env.texels.clone();
    while w > 1 || h > 1 {
        let nw = (w / 2).max(1);
        let nh = (h / 2).max(1);
        let mut next_f32 = vec![[0.0f32; 3]; (nw * nh) as usize];
        for y in 0..nh {
            for x in 0..nw {
                let mut acc = [0.0f32; 3];
                let mut n = 0.0f32;
                for dy in 0..2 {
                    for dx in 0..2 {
                        let sx = (x * 2 + dx).min(w - 1);
                        let sy = (y * 2 + dy).min(h - 1);
                        let t = prev_f32[(sy * w + sx) as usize];
                        acc[0] += t[0];
                        acc[1] += t[1];
                        acc[2] += t[2];
                        n += 1.0;
                    }
                }
                let inv = 1.0 / n;
                next_f32[(y * nw + x) as usize] = [acc[0] * inv, acc[1] * inv, acc[2] * inv];
            }
        }
        let packed: Vec<u16> = next_f32.iter().flat_map(&pack).collect();
        levels.push((nw, nh, packed));
        prev_f32 = next_f32;
        w = nw;
        h = nh;
    }
    levels
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::convert::{clamp_to_u8, floor_to_i32};

    #[test]
    fn mip_chain_reaches_1x1() {
        let env = EnvImage::new(8, 4, vec![[1.0, 0.5, 0.25]; 32]);
        let mips = build_f16_mip_chain(&env);
        // 8×4 → 4×2 → 2×1 → 1×1 = 4 levels.
        assert_eq!(mips.len(), 4);
        let (lw, lh, _) = mips.last().unwrap();
        assert_eq!((*lw, *lh), (1, 1));
    }

    #[test]
    fn constant_env_mip_preserves_value() {
        let env = EnvImage::new(4, 2, vec![[2.0, 4.0, 8.0]; 8]);
        let mips = build_f16_mip_chain(&env);
        let (_, _, top) = mips.last().unwrap();
        assert!((f16::from_bits(top[0]).to_f32() - 2.0).abs() < 0.05);
        assert!((f16::from_bits(top[1]).to_f32() - 4.0).abs() < 0.05);
        assert!((f16::from_bits(top[2]).to_f32() - 8.0).abs() < 0.05);
    }

    #[test]
    fn decode_flat_radiance_hdr_from_memory() {
        // Regression test for the missing `image` crate `hdr` feature: a textured
        // DomeLight's `.hdr` equirect silently failed to decode ("The image format
        // Hdr is not supported"), so the renderer fell back to the analytic dome.
        // This exercises `decode_env_equirect` with NO GPU, NO USD, and NO
        // filesystem (bytes fed via the reader hook). It FAILS when `hdr` is absent.

        // Encode one linear-RGB pixel to Radiance RGBE (shared exponent).
        fn to_rgbe(r: f32, g: f32, b: f32) -> [u8; 4] {
            let m = r.max(g).max(b);
            if m < 1e-32 {
                return [0, 0, 0, 0];
            }
            let e = floor_to_i32(m.log2()) + 1;
            let scale = 256.0 / 2f32.powi(e);
            let byte = |c: f32| clamp_to_u8(c * scale);
            [
                byte(r),
                byte(g),
                byte(b),
                u8::try_from(e + 128).unwrap_or(0),
            ]
        }

        // width < 8 forces the flat (uncompressed) scanline format (no RLE).
        // Power-of-two channels round-trip exactly through RGBE.
        let width: u32 = 4;
        let rows: [[f32; 3]; 4] = [
            [1.0, 1.0, 1.0],
            [0.5, 0.5, 0.5],
            [0.25, 0.25, 0.25],
            [0.125, 0.125, 0.125],
        ];
        let height = u32_sat(rows.len());
        let mut bytes: Vec<u8> = Vec::new();
        bytes.extend_from_slice(
            format!("#?RADIANCE\nFORMAT=32-bit_rle_rgbe\n\n-Y {height} +X {width}\n").as_bytes(),
        );
        for row in &rows {
            let rgbe = to_rgbe(row[0], row[1], row[2]);
            for _ in 0..width {
                bytes.extend_from_slice(&rgbe);
            }
        }

        let reader = move |_p: &str| Some(bytes.clone());
        let img = decode_env_equirect("mem://dome_equirect.hdr", Some(&reader))
            .expect("flat Radiance .hdr must decode (requires the `image` crate `hdr` feature)");

        assert_eq!((img.width, img.height), (width, height));
        assert_eq!(img.texels.len(), (width * height) as usize);
        // Row-major, top→bottom: every texel in row r equals rows[r] (± RGBE quantization).
        for (r, row) in rows.iter().enumerate() {
            for x in 0..width as usize {
                let t = img.texels[r * width as usize + x];
                for c in 0..3 {
                    assert!(
                        (t[c] - row[c]).abs() < 1e-3,
                        "row {r} texel {x} chan {c}: got {} want {}",
                        t[c],
                        row[c]
                    );
                }
            }
        }
    }
}
