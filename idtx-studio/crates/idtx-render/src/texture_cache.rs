//! GPU texture cache for the PBR material pipeline.
//!
//! Loads PNG/JPEG/EXR images from disk and uploads them to wgpu textures,
//! caching by resolved file path to avoid redundant GPU uploads within a
//! single render session.
//!
//! # sRGB vs linear
//!
//! Diffuse and emissive textures are uploaded as `Rgba8UnormSrgb` so the
//! hardware linearises them on read.  Normal, metallic, roughness, and
//! occlusion textures are uploaded as `Rgba8Unorm` to avoid unintended gamma
//! correction on data-channel values.
//!
//! # Null textures
//!
//! Three 1×1 "null" textures cover absent slots without requiring per-material
//! branching in the bind-group creation code:
//!
//! * `null_srgb` — white `Rgba8UnormSrgb` — used for absent diffuse / emissive
//! * `null_linear` — white `Rgba8Unorm` — used for absent metallic / roughness / occlusion
//! * `null_normal` — `[128, 128, 255, 255]` `Rgba8Unorm` — flat (0,0,1) normal map

use std::collections::HashMap;
use std::sync::Arc;

use wgpu::util::DeviceExt;

use crate::convert::{AsF32, clamp_to_u8, f32_to_u32, u32_sat};

// ── Public types ──────────────────────────────────────────────────────────────

/// Selects the filter algorithm used for both resolution-cap downscaling and
/// per-level mip-chain generation inside [`decode_image_cpu`].
///
/// [`DownscaleAlgo::Box`] is the handrolled integer 2×2 box-average; all other
/// variants delegate to `image::imageops::resize` with the corresponding
/// [`image::imageops::FilterType`].
///
/// # Choosing an algorithm
///
/// | Variant | Speed | Quality | Typical use |
/// |---|---|---|---|
/// | `Box` | fastest | viewer-quality | real-time inspector / poser |
/// | `Nearest` | fast | low (may alias) | pixel-art / data textures |
/// | `Triangle` | moderate | good | authoring workflows |
/// | `CatmullRom` | moderate | sharp | authoring, minimal ringing |
/// | `Gaussian` | moderate | soft | aliasing-suppression |
/// | `Lanczos3` | slowest | highest | conductor packaging passes |
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DownscaleAlgo {
    /// Integer 2×2 box-average (handrolled). ~10–15× faster per level than
    /// floating-point filters. Recommended for all real-time viewer profiles.
    #[default]
    Box,
    /// Nearest-neighbour via `image::imageops`. Fastest image-crate filter;
    /// may show aliasing on non-power-of-two downscale steps.
    Nearest,
    /// Bilinear (triangle) filter via `image::imageops`. Good balance of
    /// quality and speed for authoring workflows.
    Triangle,
    /// Catmull-Rom cubic via `image::imageops`. Sharper than `Triangle` with
    /// minimal ringing.
    CatmullRom,
    /// Gaussian filter via `image::imageops`. Softer; suppresses aliasing at
    /// the cost of sharpness.
    Gaussian,
    /// Lanczos3 via `image::imageops`. Highest quality; slowest. Recommended
    /// for conductor packaging passes where quality trumps throughput.
    Lanczos3,
}

impl DownscaleAlgo {
    /// Maps image-crate variants to [`image::imageops::FilterType`].
    /// Returns `None` for the handrolled [`DownscaleAlgo::Box`] variant.
    #[must_use]
    pub fn as_filter_type(self) -> Option<image::imageops::FilterType> {
        match self {
            Self::Box => None,
            Self::Nearest => Some(image::imageops::FilterType::Nearest),
            Self::Triangle => Some(image::imageops::FilterType::Triangle),
            Self::CatmullRom => Some(image::imageops::FilterType::CatmullRom),
            Self::Gaussian => Some(image::imageops::FilterType::Gaussian),
            Self::Lanczos3 => Some(image::imageops::FilterType::Lanczos3),
        }
    }
}

/// Controls how images are decoded and mipmapped during texture loading.
///
/// Pass this to [`TextureCache`] at construction time to tune quality vs.
/// throughput for the running product profile.
///
/// # Conductor packaging scaffold
///
/// A `pre_encode_for_runtime` flag is reserved for an `idtx-build` step that
/// pre-encodes textures to KTX2 with GPU-compressed mip chains for the
/// runtime player; it is not implemented.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextureDecodeConfig {
    /// Clamp the longest image edge to this many pixels before mip generation.
    /// `None` = no cap (full source resolution — use for conductor/packaging).
    /// `Some(1024)` is recommended for viewer/inspector profiles.
    pub max_dimension: Option<u32>,
    /// Generate the full mip chain on the CPU.  Set `false` only when textures
    /// are already mip-encoded (e.g. KTX2 runtime packages — not yet used).
    pub generate_mipmaps: bool,
    /// Filter algorithm applied during resolution-cap downscaling and each
    /// mip-chain generation step. Defaults to [`DownscaleAlgo::Box`] for all
    /// built-in presets. Override per profile or conductor packaging config.
    pub downscale_algo: DownscaleAlgo,
    /// Per-texture CPU decode working-set ceiling (one texture's source pixels
    /// plus its capped copy and mips). Bounds a SINGLE decode, not the stage.
    pub max_decode_bytes: u64,
    /// Upload budget admitted in ONE prepare pass (`prefetch_*` plus any lazy
    /// `get_or_load`s); reset to zero at the start of every
    /// [`TextureCache::retain_paths`]. A whole stage is prefetched in a single
    /// pass, so this is effectively the cold-load ceiling for the stage's
    /// combined textures. It MUST be `>= max_resident_bytes`: if it is smaller, a
    /// cold prepare stops admitting before residency is full and every
    /// still-unloaded texture is permanently replaced with the grey null fallback
    /// — the 128-vs-256 MiB asymmetry that nulled payload-composed assets such as
    /// the lobby `toy_drummer` body.
    pub max_upload_bytes: u64,
    /// Combined GPU residency ceiling for ALL of a stage's material textures at
    /// once (plus external env maps), excluding the four 1x1 fallbacks. This is
    /// the single shared pool every texture in the composed stage competes for;
    /// recomputed from the live cache on each [`TextureCache::retain_paths`].
    pub max_resident_bytes: u64,
}

impl TextureDecodeConfig {
    /// Read-only viewer: 1024 px cap, mipmaps on, box filter.
    ///
    /// Memory-constrained standalone target (mobile / Quest). At the 1024 cap a
    /// texture is ~5 MiB with mips, so the 512 MiB shared pool holds ~96 of them.
    /// `max_upload_bytes == max_resident_bytes` so one cold prefetch can fill
    /// residency to the cap without nulling late-sorted textures.
    pub const INSPECTOR: Self = Self {
        max_dimension: Some(1024),
        generate_mipmaps: true,
        downscale_algo: DownscaleAlgo::Box,
        // Per-texture CPU decode ceiling. The `image` crate materialises the
        // full-resolution source before the `max_dimension` downscale, so this
        // must accommodate the largest source a real asset ships: an 8K RGBA
        // decode alone is 256 MiB. Decoding is sequential (one texture at a
        // time), so the peak is a single source buffer plus its encoded bytes.
        max_decode_bytes: 512 * 1024 * 1024,
        // Combined per-stage budget (see the field docs). Upload == resident so
        // a cold whole-stage prefetch can fill residency in one pass.
        max_upload_bytes: 512 * 1024 * 1024,
        max_resident_bytes: 512 * 1024 * 1024,
    };
    /// Authoring / desktop preview (Poser): 2048 px cap, mipmaps on, box filter.
    ///
    /// Desktop and PCVR have gigabytes of VRAM, so the combined per-stage budget
    /// is far above the standalone viewer's — a dense level with several textured
    /// payloads (the lobby carries a biplane + drummer + widgets, ~14 textures ≈
    /// 300 MiB at this cap) must not starve whichever material sorts last. 2048
    /// textures are ~21 MiB each, so 2 GiB holds ~95 of them.
    pub const AUTHORING: Self = Self {
        max_dimension: Some(2048),
        max_upload_bytes: 2 * 1024 * 1024 * 1024,
        max_resident_bytes: 2 * 1024 * 1024 * 1024,
        ..Self::INSPECTOR
    };
    /// Full resolution, mipmaps on — conductor authoring or build pipeline on a
    /// desktop workstation. Uncapped sources are large, so both the per-texture
    /// decode working set and the combined residency budget are the most generous.
    pub const FULL_RESOLUTION: Self = Self {
        max_dimension: None,
        // Uncapped: an 8K RGBA source decode (256 MiB) plus `validate_decode`'s
        // transient estimate needs well over the capped-profile 512 MiB ceiling.
        max_decode_bytes: 2 * 1024 * 1024 * 1024,
        max_upload_bytes: 4 * 1024 * 1024 * 1024,
        max_resident_bytes: 4 * 1024 * 1024 * 1024,
        ..Self::INSPECTOR
    };
    /// Runtime player: no resize, no mip generation (pre-encoded KTX2 assumed).
    /// Inherits the desktop/PCVR full-resolution residency pool.
    pub const RUNTIME: Self = Self {
        max_dimension: None,
        generate_mipmaps: false,
        ..Self::FULL_RESOLUTION
    };

    /// Stable lowercase token naming one of the three user-selectable packaged
    /// profiles — the contract carried through `run.toml`
    /// (`[experience].texture_quality`) and the project's per-target build
    /// settings. Returns `None` for a config that matches no named preset (e.g.
    /// [`RUNTIME`](Self::RUNTIME) or a hand-tuned value).
    ///
    /// The three named profiles differ by their resolution cap, which is what
    /// bounds per-texture GPU residency on a memory-constrained standalone
    /// device: `full_resolution` keeps the source size (a 4K texture stays
    /// ~85 MiB with mips), `authoring` caps the longest edge to 2048 (~21 MiB),
    /// and `inspector` caps to 1024 (~5 MiB).
    #[must_use]
    pub fn profile_token(self) -> Option<&'static str> {
        if self == Self::FULL_RESOLUTION {
            Some("full_resolution")
        } else if self == Self::AUTHORING {
            Some("authoring")
        } else if self == Self::INSPECTOR {
            Some("inspector")
        } else {
            None
        }
    }

    /// Parse a [`profile_token`](Self::profile_token) back into its preset.
    /// Case-insensitive and whitespace-trimmed; unknown tokens return `None` so
    /// the caller can fall back to a default. Accepts a few friendly aliases
    /// (`full`, `high`, `balanced`) alongside the canonical tokens.
    #[must_use]
    pub fn from_profile_token(token: &str) -> Option<Self> {
        match token.trim().to_ascii_lowercase().as_str() {
            "full_resolution" | "full" => Some(Self::FULL_RESOLUTION),
            "authoring" | "high" => Some(Self::AUTHORING),
            "inspector" | "balanced" => Some(Self::INSPECTOR),
            _ => None,
        }
    }
}

impl Default for TextureDecodeConfig {
    fn default() -> Self {
        Self::AUTHORING
    }
}

/// Accumulated CPU time for each sub-phase of texture decoding across all
/// textures decoded in a single [`TextureCache::prefetch_parallel`] call.
///
/// All durations are summed across all worker threads (total CPU work),
/// not wall-clock time. Divide by worker count for per-texture averages.
#[derive(Debug, Default, Clone, Copy)]
pub struct TextureDecodeTimings {
    /// Time reading raw bytes from the resolver or filesystem.
    pub file_read_ms: f64,
    /// Time for `image::load_from_memory` (JPEG/PNG decode).
    pub image_decode_ms: f64,
    /// Time for `DynamicImage::into_rgba8()` color-space conversion.
    pub rgba_convert_ms: f64,
    /// Time for resolution-cap box downscale (zero when already within cap).
    pub cap_resize_ms: f64,
    /// Time for mip chain generation (algorithm selected by `downscale_algo`).
    pub mip_chain_ms: f64,
}

impl TextureDecodeTimings {
    fn add(&mut self, other: &Self) {
        self.file_read_ms += other.file_read_ms;
        self.image_decode_ms += other.image_decode_ms;
        self.rgba_convert_ms += other.rgba_convert_ms;
        self.cap_resize_ms += other.cap_resize_ms;
        self.mip_chain_ms += other.mip_chain_ms;
    }
}


type ReadFnCallback = dyn Fn(&str) -> Option<Vec<u8>> + Send + Sync;
/// Callback for reading asset bytes by resolved path (e.g. from USD `AssetResolver`).
///
/// Decouples `idtx-render` from `idtx-usd`: the caller wraps `StageService::read_asset`
/// in a closure and passes it here. `None` return means "not found / use filesystem fallback".
pub type AssetReadFn = Arc<ReadFnCallback>;

/// A single GPU texture and its default view.
#[allow(missing_debug_implementations)]
pub struct GpuTexture {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub format: wgpu::TextureFormat,
    pub width: u32,
    pub height: u32,
}

/// Sentinel layer index meaning "this tile has no layer in the array" — the
/// shader falls back to the material's scalar factor (`base_color`, `metallic`, …)
/// for that channel. Distinct from a valid `0..layer_count` index.
pub const UDIM_LAYER_SENTINEL: u32 = 0xFFFF_FFFF;

/// A UDIM texture array for one material channel: a `D2Array` texture whose
/// layers are the tiles that loaded successfully for one `<UDIM>` template,
/// plus a `tile_to_layer` map (indexed by `tile - 1001`, `0..100`) giving each
/// tile's layer index or [`UDIM_LAYER_SENTINEL`] when that tile is absent.
///
/// Layer ordering is the sorted set of loaded tiles for **this template alone**
/// — each channel's array is built independently, so a tile missing for this
/// channel maps to the sentinel and the shader keeps the scalar fallback rather
/// than sampling a wrong (e.g. white) null layer.
#[derive(Debug)]
pub struct GpuUdimTexture {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub format: wgpu::TextureFormat,
    pub width: u32,
    pub height: u32,
    pub layer_count: u32,
    pub tile_to_layer: [u32; 100],
}

/// Timings for one [`TextureCache::prefetch_udim_parallel`] call.
///
/// Durations mix wall-clock (parallel decode) and summed main-thread work
/// (resample/mip/upload run sequentially after the join).
#[derive(Debug, Default, Clone, Copy)]
pub struct UdimTimings {
    /// Number of tiles across all templates that were attempted.
    pub tiles_probed: usize,
    /// Tiles whose file read + decode succeeded (became array layers).
    pub tiles_found: usize,
    /// Tiles whose file was absent or failed to decode (→ sentinel).
    pub tiles_missing: usize,
    /// Wall-clock time reading + decoding all tiles (parallel).
    pub discovery_ms: f64,
    /// Summed time resampling loaded tiles to their per-array common size.
    pub resample_ms: f64,
    /// Summed time building per-layer mip chains.
    pub mip_ms: f64,
    /// Wall-clock time uploading the assembled arrays to the GPU.
    pub array_upload_ms: f64,
}

/// Texture kind — controls which GPU format and null fallback are chosen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TextureKind {
    /// Base colour / diffuse map (sRGB encoded).
    DiffuseSrgb,
    /// Emissive map (sRGB encoded).
    EmissiveSrgb,
    /// Tangent-space normal map (linear, decoded as 2*sample - 1).
    Normal,
    /// Scalar data channel (metallic, roughness, occlusion — linear).
    Data,
}

/// Shared GPU texture managed by the cache.
pub type SharedTexture = Arc<GpuTexture>;

/// Per-session GPU texture cache.
///
/// Keyed by `(resolved_path, kind)` so the same image can be stored in both
/// sRGB and linear formats when used with different semantic roles.
#[allow(missing_debug_implementations)]
pub struct TextureCache {
    cache: HashMap<(String, TextureKind), SharedTexture>,
    /// UDIM arrays keyed by `(template_path, kind)`. The template path contains
    /// `<UDIM>` so keys can never collide with the plain `cache`.
    udim_cache: HashMap<(String, TextureKind), Arc<GpuUdimTexture>>,
    null_srgb: SharedTexture,
    null_linear: SharedTexture,
    null_normal: SharedTexture,
    /// 1×1×1 `D2Array` bound for UDIM channels that have no template / no tiles;
    /// every `tile_to_layer` entry is the sentinel, so the shader never samples it.
    null_udim: Arc<GpuUdimTexture>,
    reader: Option<AssetReadFn>,
    config: TextureDecodeConfig,
    resident_bytes: u64,
    upload_bytes: u64,
    external_bytes: u64,
    decode_jobs: usize,
    #[cfg(feature = "metrics")]
    pub stats: TextureBudgetStats,
}

#[cfg(feature = "metrics")]
#[derive(Debug, Default, Clone, Copy)]
pub struct TextureBudgetStats {
    pub peak_resident_bytes: u64,
    pub uploaded_bytes: u64,
    pub rejected: u64,
    pub evicted: u64,
    pub peak_decoded_output_bytes: u64,
}

impl TextureCache {
    /// Initialises the cache with filesystem-only image loading.
    #[must_use]
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, config: TextureDecodeConfig) -> Self {
        Self::new_inner(device, queue, None, config)
    }

    /// Initialises the cache with a USD AssetResolver-backed reader.
    ///
    /// `reader` is called first for every texture path; filesystem loading is
    /// the fallback when it returns `None`.
    pub fn new_with_reader(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        reader: AssetReadFn,
        config: TextureDecodeConfig,
    ) -> Self {
        Self::new_inner(device, queue, Some(reader), config)
    }

    fn new_inner(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        reader: Option<AssetReadFn>,
        config: TextureDecodeConfig,
    ) -> Self {
        let mut config = config;
        config.max_dimension = Some(config.max_dimension.unwrap_or(u32::MAX)
            .min(device.limits().max_texture_dimension_2d).max(1));
        let null_srgb = Arc::new(create_1x1(
            device,
            queue,
            wgpu::TextureFormat::Rgba8UnormSrgb,
            [255, 255, 255, 255],
            "null_srgb",
        ));
        let null_linear = Arc::new(create_1x1(
            device,
            queue,
            wgpu::TextureFormat::Rgba8Unorm,
            [255, 255, 255, 255],
            "null_linear",
        ));
        let null_normal = Arc::new(create_1x1(
            device,
            queue,
            wgpu::TextureFormat::Rgba8Unorm,
            [128, 128, 255, 255],
            "null_normal",
        ));
        let null_udim = Arc::new(create_null_udim(device, queue));
        Self {
            cache: HashMap::new(),
            udim_cache: HashMap::new(),
            null_srgb,
            null_linear,
            null_normal,
            null_udim,
            reader,
            config,
            resident_bytes: 0,
            upload_bytes: 0,
            external_bytes: 0,
            decode_jobs: 0,
            #[cfg(feature = "metrics")]
            stats: TextureBudgetStats::default(),
        }
    }

    pub fn config(&self) -> TextureDecodeConfig { self.config }
    pub fn resident_bytes(&self) -> u64 { self.resident_bytes }

    fn begin_decode(&mut self) -> bool {
        if self.decode_jobs >= 256 { return false; }
        self.decode_jobs += 1;
        true
    }

    pub(crate) fn admit_external(&mut self, bytes: u64) -> bool {
        if !self.admit(bytes) { return false; }
        self.external_bytes += bytes;
        true
    }
    pub(crate) fn release_external(&mut self, bytes: u64) {
        self.external_bytes = self.external_bytes.saturating_sub(bytes);
        self.resident_bytes = self.resident_bytes.saturating_sub(bytes);
    }

    fn admit(&mut self, bytes: u64) -> bool {
        if bytes > self.config.max_resident_bytes.saturating_sub(self.resident_bytes)
            || bytes > self.config.max_upload_bytes.saturating_sub(self.upload_bytes) {
            #[cfg(feature = "metrics")]
            { self.stats.rejected += 1; }
            return false;
        }
        self.resident_bytes += bytes;
        self.upload_bytes += bytes;
        #[cfg(feature = "metrics")]
        {
            self.stats.uploaded_bytes += bytes;
            self.stats.peak_resident_bytes = self.stats.peak_resident_bytes.max(self.resident_bytes);
        }
        true
    }

    /// Evict paths absent from the new snapshot. The renderer releases its
    /// material bind groups before calling this; external Arc holders own their
    /// own lifetime. Resident scenes never evict still-used textures to admit
    /// new ones: over-budget materials use the stable null fallback.
    pub fn retain_paths(&mut self, paths: &[(String, TextureKind)], udim: &[(String, TextureKind, Vec<u32>)]) -> bool {
        let plain: std::collections::HashSet<_> = paths.iter().cloned().collect();
        let arrays: std::collections::HashSet<_> = udim.iter().map(|(p, k, _)| (p.clone(), *k)).collect();
        let before = self.cache.len() + self.udim_cache.len();
        self.cache.retain(|key, _| plain.contains(key));
        self.udim_cache.retain(|key, _| arrays.contains(key));
        self.resident_bytes = self.cache.values().filter(|t| !Arc::ptr_eq(t, &self.null_srgb)
            && !Arc::ptr_eq(t, &self.null_linear) && !Arc::ptr_eq(t, &self.null_normal))
            .map(|t| rgba_bytes(t.width, t.height, t.texture.mip_level_count(), 1).unwrap_or(0)).sum::<u64>()
            + self.udim_cache.values().filter(|t| !Arc::ptr_eq(t, &self.null_udim))
            .map(|t| rgba_bytes(t.width, t.height, t.texture.mip_level_count(), t.layer_count).unwrap_or(0)).sum::<u64>() + self.external_bytes;
        self.upload_bytes = 0;
        self.decode_jobs = 0;
        let removed = before - self.cache.len() - self.udim_cache.len();
        #[cfg(feature = "metrics")]
        { self.stats.evicted += removed as u64; }
        removed != 0
    }

    pub fn has_unused_paths(&self, paths: &[(String, TextureKind)], udim: &[(String, TextureKind, Vec<u32>)]) -> bool {
        self.cache.keys().any(|key| !paths.contains(key))
            || self.udim_cache.keys().any(|(p, k)| !udim.iter().any(|(t, kind, _)| p == t && k == kind))
    }

    /// Decodes all `paths` in parallel (one thread per path) and uploads the
    /// results to GPU. Paths already in the cache are skipped. Call this before
    /// the first [`Self::get_or_load`] to front-load disk I/O off the render
    /// hot path.
    ///
    /// Returns `(loaded_count, decode_elapsed, upload_elapsed, sub_phase_timings)`.
    /// `sub_phase_timings` contains summed CPU time per decode sub-phase across
    /// all textures (use it alongside `decode_elapsed` to find bottlenecks).
    pub fn prefetch_parallel(
        &mut self,
        paths: &[(String, TextureKind)],
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> (
        usize,
        std::time::Duration,
        std::time::Duration,
        TextureDecodeTimings,
    ) {
        // Stream one bounded decode into one upload. Joining whole worker
        // slices retained every decoded mip chain simultaneously.
        let mut count = 0;
        let mut decode_elapsed = std::time::Duration::ZERO;
        let mut upload_elapsed = std::time::Duration::ZERO;
        let mut agg_timings = TextureDecodeTimings::default();
        // Textures substituted with the null fallback because the budget (not a
        // decode error) ran out this pass — surfaced below so the failure mode is
        // never silent.
        let mut starved = 0u32;
        for (path, kind) in paths {
            let key = (path.clone(), *kind);
            if self.cache.contains_key(&key) { continue; }
            if self.cache.len() + self.udim_cache.len() >= 4096 { break; }
            let budget_hit = self.upload_bytes >= self.config.max_upload_bytes
                || self.resident_bytes >= self.config.max_resident_bytes;
            if budget_hit || !self.begin_decode() {
                if budget_hit { starved += 1; }
                self.cache.insert(key, Arc::clone(self.null(*kind)));
                continue;
            }
            let start = std::time::Instant::now();
            let decoded = decode_image_cpu(path, self.reader.as_deref(), self.config);
            decode_elapsed += start.elapsed();
            if let Ok((w, h, mc, pixels, timings)) = decoded {
                #[cfg(feature = "metrics")]
                { self.stats.peak_decoded_output_bytes = self.stats.peak_decoded_output_bytes.max(pixels.len() as u64); }
                agg_timings.add(&timings);
                if self.admit(pixels.len() as u64) {
                    let start = std::time::Instant::now();
                    let tex = upload_decoded(device, queue, path, gpu_format(*kind, path), w, h, mc, &pixels);
                    upload_elapsed += start.elapsed();
                    self.cache.insert(key, Arc::new(tex));
                    count += 1;
                    continue;
                }
                // Decoded fine but could not fit the residency/upload budget.
                starved += 1;
            }
            self.cache.insert(key, Arc::clone(self.null(*kind)));
        }
        if starved > 0 {
            // Always-on (not metrics-gated): a budget-starved texture renders as
            // a flat grey/white surface, which reads as a content bug. Name the
            // knob so it is actionable.
            eprintln!(
                "[idtx-render] texture budget exhausted: {starved} texture(s) fell back to the null \
                 placeholder (resident {} MiB / upload {} MiB vs cap {} MiB / {} MiB). Raise the \
                 profile's max_resident_bytes/max_upload_bytes or lower texture_quality.",
                self.resident_bytes >> 20,
                self.upload_bytes >> 20,
                self.config.max_resident_bytes >> 20,
                self.config.max_upload_bytes >> 20,
            );
        }
        (count, decode_elapsed, upload_elapsed, agg_timings)
    }

    /// The null texture for a given kind.
    #[must_use]
    pub fn null(&self, kind: TextureKind) -> &SharedTexture {
        match kind {
            TextureKind::DiffuseSrgb | TextureKind::EmissiveSrgb => &self.null_srgb,
            TextureKind::Normal => &self.null_normal,
            TextureKind::Data => &self.null_linear,
        }
    }

    /// Whether a single-tile texture has been loaded and cached for `(path, kind)`.
    /// Introspection hook used to assert that `<UDIM>` template paths are never
    /// routed through the plain single-tile loader.
    #[must_use]
    pub fn is_cached(&self, path: &str, kind: TextureKind) -> bool {
        self.cache.contains_key(&(path.to_owned(), kind))
    }

    /// The USD asset-resolver reader, if one was installed. Used by the `DomeLight`
    /// environment loader to read equirect textures packed inside a `.usdz`.
    #[must_use]
    pub fn reader(&self) -> Option<&AssetReadFn> {
        self.reader.as_ref()
    }

    /// Get or load a texture from `path`.
    ///
    /// Returns the appropriate null texture when `path` is empty or loading fails.
    pub fn get_or_load(
        &mut self,
        path: &str,
        kind: TextureKind,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> SharedTexture {
        if path.is_empty() {
            return Arc::clone(self.null(kind));
        }

        let key = (path.to_owned(), kind);
        if let Some(cached) = self.cache.get(&key) {
            return Arc::clone(cached);
        }

        self.prefetch_parallel(&[(path.to_owned(), kind)], device, queue);
        Arc::clone(self.cache.get(&key).unwrap_or_else(|| self.null(kind)))
    }

    /// The 1×1×1 null UDIM array (all tiles map to the sentinel). Bound for a
    /// UDIM material's channels that reference no `<UDIM>` template.
    #[must_use]
    pub fn udim_null(&self) -> &Arc<GpuUdimTexture> {
        &self.null_udim
    }

    /// Looks up a previously prefetched UDIM array by `(template, kind)`.
    /// Returns [`Self::udim_null`] when the template was never loaded.
    #[must_use]
    pub fn udim_get(&self, template: &str, kind: TextureKind) -> &Arc<GpuUdimTexture> {
        self.udim_cache
            .get(&(template.to_owned(), kind))
            .unwrap_or(&self.null_udim)
    }

    /// Decodes and uploads UDIM tile arrays in parallel, one `D2Array` per
    /// `(template, kind)` request. Each request supplies the set of tile numbers
    /// (from extract-time mesh-UV discovery) to probe for that template.
    ///
    /// Layers are the sorted subset of requested tiles whose files decoded
    /// successfully; the resulting `tile_to_layer` maps loaded tiles to their
    /// layer index and every other tile to [`UDIM_LAYER_SENTINEL`]. Templates
    /// with no loadable tile get a clone of [`Self::udim_null`] cached so the
    /// probe is not retried. Already-cached `(template, kind)` pairs are skipped.
    ///
    /// Returns `(tiles_found, tiles_missing, timings)`.
    pub fn prefetch_udim_parallel(
        &mut self,
        requests: &[(String, TextureKind, Vec<u32>)],
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> (usize, usize, UdimTimings) {
        struct Loaded {
            tile: u32,
            img: image::RgbaImage,
        }
        let mut timings = UdimTimings::default();
        let reader = self.reader.clone();
        let cap = self.config.max_dimension.unwrap_or(u32::MAX);
        let algo = self.config.downscale_algo;
        let gen_mips = self.config.generate_mipmaps;
        for (template, kind, tiles) in requests {
            let key = (template.clone(), *kind);
            if self.udim_cache.contains_key(&key) { continue; }
            if self.cache.len() + self.udim_cache.len() >= 4096 { break; }
            let tiles: std::collections::BTreeSet<_> = tiles.iter().copied().filter(|t| (1001..=1100).contains(t)).collect();
            let mut group = Vec::new();
            let mut retained = 0u64;
            let start = std::time::Instant::now();
            // At most one array is being assembled; source images use at most
            // one quarter of the CPU budget, leaving room for decode and mips.
            for tile in tiles {
                timings.tiles_probed += 1;
                if !self.begin_decode() { timings.tiles_missing += 1; continue; }
                let path = crate::udim::expand_template(template, tile);
                let remaining = (self.config.max_decode_bytes / 4).saturating_sub(retained);
                match decode_rgba_bounded(&path, reader.as_deref(), &mut TextureDecodeTimings::default(), remaining, self.config.max_dimension) {
                    Ok(img) => { retained += img.as_raw().len() as u64; group.push(Loaded { tile, img }); }
                    Err(_) => { timings.tiles_missing += 1; }
                }
            }
            timings.discovery_ms += start.elapsed().as_secs_f64() * 1000.0;
            timings.tiles_found += group.len();
            if group.is_empty() {
                // No tile loaded — cache the null array so we do not re-probe.
                self.udim_cache.insert(key, Arc::clone(&self.null_udim));
                continue;
            }
            group.sort_by_key(|l| l.tile);

            // Common resolution: largest present tile dim, capped by config.
            let common_w = group
                .iter()
                .map(|l| l.img.width())
                .max()
                .unwrap_or(1)
                .min(cap)
                .max(1);
            let common_h = group
                .iter()
                .map(|l| l.img.height())
                .max()
                .unwrap_or(1)
                .min(cap)
                .max(1);

            let levels = if gen_mips { full_mip_count(common_w, common_h) } else { 1 };
            let bytes = rgba_bytes(common_w, common_h, levels, u32_sat(group.len()));
            if group.len() > device.limits().max_texture_array_layers as usize
                || !bytes.is_some_and(|n| n <= self.config.max_decode_bytes / 4 && self.admit(n)) {
                self.udim_cache.insert(key, Arc::clone(&self.null_udim));
                continue;
            }

            let mut tile_to_layer = [UDIM_LAYER_SENTINEL; 100];
            let mut mip_count = 1u32;
            let mut layer_blobs: Vec<Vec<u8>> = Vec::with_capacity(group.len());
            for (layer, l) in group.iter().enumerate() {
                tile_to_layer[(l.tile - 1001) as usize] = u32_sat(layer);

                let t_rs = std::time::Instant::now();
                let resized = resample_common(l.img.clone(), common_w, common_h, algo);
                timings.resample_ms += t_rs.elapsed().as_secs_f64() * 1000.0;

                let t_mip = std::time::Instant::now();
                let (levels, pixels) = if gen_mips {
                    build_mip_chain_with_algo(resized, algo)
                } else {
                    (1, resized.into_raw())
                };
                timings.mip_ms += t_mip.elapsed().as_secs_f64() * 1000.0;
                mip_count = levels;
                layer_blobs.push(pixels);
            }

            // Concatenate layer blobs (each a full mip chain) for LayerMajor upload.
            let mut all: Vec<u8> = Vec::with_capacity(layer_blobs.iter().map(Vec::len).sum());
            for b in &layer_blobs {
                all.extend_from_slice(b);
            }
            #[cfg(feature = "metrics")]
            { self.stats.peak_decoded_output_bytes = self.stats.peak_decoded_output_bytes.max(all.len() as u64); }

            let t_up = std::time::Instant::now();
            let arr = upload_udim_array(
                device,
                queue,
                template,
                gpu_format(*kind, template),
                common_w,
                common_h,
                mip_count,
                u32_sat(group.len()),
                &tile_to_layer,
                &all,
            );
            timings.array_upload_ms += t_up.elapsed().as_secs_f64() * 1000.0;
            self.udim_cache.insert(key, Arc::new(arr));
        }

        #[cfg(feature = "metrics")]
        eprintln!(
            "[idtx-udim] probed={} found={} missing={} discovery={:.1}ms resample={:.1}ms mip={:.1}ms upload={:.1}ms",
            timings.tiles_probed,
            timings.tiles_found,
            timings.tiles_missing,
            timings.discovery_ms,
            timings.resample_ms,
            timings.mip_ms,
            timings.array_upload_ms
        );

        (timings.tiles_found, timings.tiles_missing, timings)
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Checked RGBA8 mip/array size, also used for admission before array assembly.
pub(crate) fn rgba_bytes(mut w: u32, mut h: u32, levels: u32, layers: u32) -> Option<u64> {
    let mut bytes = 0u64;
    for _ in 0..levels {
        bytes = bytes.checked_add(u64::from(w).checked_mul(u64::from(h))?.checked_mul(4)?)?;
        w = (w / 2).max(1); h = (h / 2).max(1);
    }
    bytes.checked_mul(u64::from(layers))
}

pub(crate) fn read_bounded(path: &str, reader: Option<&(dyn Fn(&str) -> Option<Vec<u8>> + Send + Sync)>, budget: u64) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let bytes = if let Some(bytes) = reader.and_then(|r| r(path)) { bytes } else {
        let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
        if file.metadata().map_err(|e| e.to_string())?.len() > budget / 4 { return Err("encoded texture budget exceeded".into()); }
        let mut bytes = Vec::new();
        file.take(budget / 4 + 1).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
        bytes
    };
    // AssetReadFn is a legacy owned-Vec callback: its provider owns the read
    // allocation. Admission here bounds all subsequent renderer work.
    if bytes.len() as u64 > budget / 4 { return Err("encoded texture budget exceeded".into()); }
    Ok(bytes)
}

pub(crate) fn validate_decode(bytes: &[u8], budget: u64, cap: Option<u32>) -> Result<(), String> {
    // The resolution cap (`max_dimension`) is applied immediately after decode,
    // so the resident footprint is bounded by the *capped* size, not the source
    // size. Estimate the transient peak as the full-resolution source decode
    // (the `image` crate materialises full resolution before downscaling) plus
    // the capped RGBA copy, its mip chain and upload scratch (~3×). Clamping the
    // working-set term by the cap admits realistic 2K/4K source textures whose
    // capped footprint is a few MB, while still rejecting pathological sources
    // whose full-resolution decode alone would blow the budget. EXR arbitrary
    // channel layouts participate rather than assuming RGBA.
    let cap = u64::from(cap.unwrap_or(u32::MAX)).max(1);
    let fits = |w: u64, h: u64, channels: u64| {
        let source = w.checked_mul(h).and_then(|n| n.checked_mul(channels.max(4)));
        let capped = w.min(cap).checked_mul(h.min(cap))
            .and_then(|n| n.checked_mul(4)).and_then(|n| n.checked_mul(3));
        source.zip(capped).and_then(|(s, c)| s.checked_add(c))
            .is_some_and(|n| n <= budget.saturating_sub(bytes.len() as u64))
    };
    if bytes.starts_with(&[0x76, 0x2f, 0x31, 0x01]) {
        let meta = exr::meta::MetaData::read_from_buffered(std::io::Cursor::new(bytes), false).map_err(|e| e.to_string())?;
        for header in &meta.headers {
            if header.deep || !fits(header.layer_size.width() as u64, header.layer_size.height() as u64, header.channels.list.len() as u64) {
                return Err("decoded EXR budget exceeded".into());
            }
        }
    } else {
        let reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().map_err(|e| e.to_string())?;
        let (w, h) = reader.into_dimensions().map_err(|e| e.to_string())?;
        if w == 0 || h == 0 || !fits(u64::from(w), u64::from(h), 4) { return Err("decoded texture budget exceeded".into()); }
    }
    Ok(())
}

fn gpu_format(kind: TextureKind, path: &str) -> wgpu::TextureFormat {
    match kind {
        TextureKind::DiffuseSrgb | TextureKind::EmissiveSrgb => {
            if source_is_linear(path) {
                // HDR/EXR colour source: the decoded bytes are already linear
                // scene-referred values, so upload as UNORM. Uploading to an
                // `*Srgb` format would apply a spurious sRGB→linear decode on
                // sample, darkening the texture (roughly value^2.2). Data
                // textures are unaffected — they are UNORM regardless.
                wgpu::TextureFormat::Rgba8Unorm
            } else {
                wgpu::TextureFormat::Rgba8UnormSrgb
            }
        }
        TextureKind::Normal | TextureKind::Data => wgpu::TextureFormat::Rgba8Unorm,
    }
}

/// The real asset path for format detection, unwrapping the USD **package-path**
/// syntax `package.usdz[inner/asset.ext]` to the innermost bracketed entry.
///
/// Textures packed inside a `.usdz` arrive as e.g.
/// `C:\…\barScene.usdz[140/foo_Roughness.exr]`. A naive `rsplit('.')` on that yields
/// `exr]` (trailing bracket), so extension checks silently fail for packed assets —
/// which is why the single-channel EXR fallback and the linear-colour-space handling
/// worked on the unpacked scene but not the packaged one. This strips a trailing
/// `[...]` (handling nested packages like `a.usdz[b.usdz[c.exr]]`) so the extension
/// is read off the inner path. Plain filesystem paths pass through unchanged.
fn effective_asset_path(path: &str) -> &str {
    match path.rfind('[') {
        Some(i) => path[i + 1..].trim_end_matches(']'),
        None => path,
    }
}

/// Lowercased file extension of the effective asset path (package-path aware).
fn asset_extension(path: &str) -> &str {
    effective_asset_path(path).rsplit('.').next().unwrap_or("")
}

/// True for source files whose pixel data is already linear (not sRGB-encoded).
///
/// HDR container formats (`OpenEXR` `.exr`, Radiance `.hdr`) store linear
/// scene-referred values; an sRGB-tagged GPU format would double-decode a colour
/// texture loaded from one. Only affects the colour [`TextureKind`]s — normal and
/// data textures are linear in every container.
fn source_is_linear(path: &str) -> bool {
    let ext = asset_extension(path);
    ext.eq_ignore_ascii_case("exr") || ext.eq_ignore_ascii_case("hdr")
}

fn full_mip_count(width: u32, height: u32) -> u32 {
    let max_dim = width.max(height);
    if max_dim <= 1 {
        1
    } else {
        u32::BITS - max_dim.leading_zeros()
    }
}

/// Generates all mip levels using an integer 2×2 box-average filter.
///
/// All levels are packed consecutively (mip 0 first) as required by
/// `create_texture_with_data` with `LayerMajor` ordering.
/// The box filter is ~10-15× faster than a floating-point bilinear pass
/// per level, and produces visually equivalent results for PBR material
/// textures at viewer quality.
fn build_mip_chain(base: image::RgbaImage) -> (u32, Vec<u8>) {
    let (w, h) = base.dimensions();
    let levels = full_mip_count(w, h);
    let base_bytes = (w as usize) * (h as usize) * 4;
    let mut all_pixels: Vec<u8> = Vec::with_capacity(base_bytes * 4 / 3 + base_bytes);
    let base_raw = base.into_raw();
    all_pixels.extend_from_slice(&base_raw);

    let mut src = base_raw;
    let mut src_w = w;
    let mut src_h = h;

    for _ in 1..levels {
        let dst_w = (src_w / 2).max(1);
        let dst_h = (src_h / 2).max(1);
        let mut dst = vec![0u8; dst_w as usize * dst_h as usize * 4];

        for y in 0..dst_h as usize {
            for x in 0..dst_w as usize {
                // 2×2 box: clamp to avoid out-of-bounds on odd-dimension images.
                let x0 = x * 2;
                let x1 = (x * 2 + 1).min(src_w as usize - 1);
                let y0 = y * 2;
                let y1 = (y * 2 + 1).min(src_h as usize - 1);
                let i00 = (y0 * src_w as usize + x0) * 4;
                let i01 = (y0 * src_w as usize + x1) * 4;
                let i10 = (y1 * src_w as usize + x0) * 4;
                let i11 = (y1 * src_w as usize + x1) * 4;
                let out = (y * dst_w as usize + x) * 4;
                for c in 0..4 {
                    let sum = u32::from(src[i00 + c])
                        + u32::from(src[i01 + c])
                        + u32::from(src[i10 + c])
                        + u32::from(src[i11 + c]);
                    // box-filter average of 0..=255 bytes stays in range
                    dst[out + c] = u8::try_from(sum / 4).unwrap_or(255);
                }
            }
        }

        all_pixels.extend_from_slice(&dst);
        src = dst;
        src_w = dst_w;
        src_h = dst_h;
    }

    (levels, all_pixels)
}

/// Fast integer box-filter downscale for arbitrary target dimensions.
///
/// Each output pixel is the average of all source pixels that map into its
/// region. For a 2:1 downscale this is a 2×2 box (identical to one step of
/// [`build_mip_chain`]), but works for any scale ratio. ~4–6× faster than
/// `image::imageops::resize` with `FilterType::Triangle` for typical 2:1
/// inspector downscales.
fn box_downscale(src: image::RgbaImage, dst_w: u32, dst_h: u32) -> image::RgbaImage {
    let (src_w, src_h) = src.dimensions();
    let raw = src.into_raw();
    let sw = src_w as usize;
    let sh = src_h as usize;
    let dw = dst_w as usize;
    let dh = dst_h as usize;
    let mut dst = vec![0u8; dw * dh * 4];

    for dy in 0..dh {
        let y0 = dy * sh / dh;
        let y1 = ((dy + 1) * sh / dh).max(y0 + 1).min(sh);
        for dx in 0..dw {
            let x0 = dx * sw / dw;
            let x1 = ((dx + 1) * sw / dw).max(x0 + 1).min(sw);
            let n = u32_sat((y1 - y0) * (x1 - x0));
            let mut sums = [0u32; 4];
            for sy in y0..y1 {
                for sx in x0..x1 {
                    let i = (sy * sw + sx) * 4;
                    sums[0] += u32::from(raw[i]);
                    sums[1] += u32::from(raw[i + 1]);
                    sums[2] += u32::from(raw[i + 2]);
                    sums[3] += u32::from(raw[i + 3]);
                }
            }
            let out = (dy * dw + dx) * 4;
            for c in 0..4 {
                // box-filter average of 0..=255 bytes stays in range
                dst[out + c] = u8::try_from(sums[c] / n).unwrap_or(255);
            }
        }
    }

    image::RgbaImage::from_raw(dst_w, dst_h, dst).unwrap()
}

/// Dispatches to the configured downscale algorithm for the resolution cap.
fn do_downscale(
    src: image::RgbaImage,
    dst_w: u32,
    dst_h: u32,
    algo: DownscaleAlgo,
) -> image::RgbaImage {
    match algo.as_filter_type() {
        None => box_downscale(src, dst_w, dst_h),
        Some(filter) => image::imageops::resize(&src, dst_w, dst_h, filter),
    }
}

/// Generates all mip levels using the configured algorithm.
///
/// For [`DownscaleAlgo::Box`] the handrolled integer 2×2 path is used.
/// All other variants call `image::imageops::resize` per level.
/// All levels are packed consecutively (mip 0 first) as required by
/// `create_texture_with_data` with `LayerMajor` ordering.
fn build_mip_chain_with_algo(base: image::RgbaImage, algo: DownscaleAlgo) -> (u32, Vec<u8>) {
    if algo == DownscaleAlgo::Box {
        return build_mip_chain(base);
    }
    let filter = algo
        .as_filter_type()
        .expect("non-Box algo always has FilterType");
    let (w, h) = base.dimensions();
    let levels = full_mip_count(w, h);
    let base_bytes = (w as usize) * (h as usize) * 4;
    let mut all_pixels: Vec<u8> = Vec::with_capacity(base_bytes * 4 / 3 + base_bytes);
    all_pixels.extend_from_slice(base.as_raw());
    let mut src = base;
    for _ in 1..levels {
        let dst_w = (src.width() / 2).max(1);
        let dst_h = (src.height() / 2).max(1);
        let dst = image::imageops::resize(&src, dst_w, dst_h, filter);
        all_pixels.extend_from_slice(dst.as_raw());
        src = dst;
    }
    (levels, all_pixels)
}

/// Reads and decodes an image to an RGBA8 buffer with **no** resolution cap or
/// mip generation — the shared front half of [`decode_image_cpu`], reused by the
/// UDIM tile loader which needs the raw decoded pixels before per-array common
/// resizing. Fills `file_read_ms` / `image_decode_ms` / `rgba_convert_ms` of
/// `timings`; leaves the cap/mip fields zero.
#[cfg(test)]
fn decode_rgba_raw(
    path: &str,
    reader: Option<&ReadFnCallback>,
    timings: &mut TextureDecodeTimings,
) -> Result<image::RgbaImage, String> {
    decode_rgba_bounded(path, reader, timings, TextureDecodeConfig::default().max_decode_bytes, TextureDecodeConfig::default().max_dimension)
}

fn decode_rgba_bounded(
    path: &str,
    reader: Option<&ReadFnCallback>,
    timings: &mut TextureDecodeTimings,
    budget: u64,
    cap: Option<u32>,
) -> Result<image::RgbaImage, String> {
    // Step 1: Read raw bytes from resolver or filesystem.
    let t = std::time::Instant::now();
    let bytes = read_bounded(path, reader, budget)?;
    validate_decode(&bytes, budget, cap)?;
    timings.file_read_ms += t.elapsed().as_secs_f64() * 1000.0;

    // Step 2: Decode image format (JPEG, PNG, EXR, …) from in-memory bytes.
    //
    // The `image` crate's OpenEXR backend only accepts EXRs with flat RGB/RGBA
    // channels, so single-channel data maps (roughness/metallic/occlusion, often
    // authored grayscale) fail with "image does not contain non-deep rgb
    // channels". For `.exr` inputs we fall back to the `exr` crate, which reads
    // arbitrary channel layouts. Non-EXR failures propagate the original error.
    let t = std::time::Instant::now();
    let img = match image::load_from_memory(&bytes) {
        Ok(dynamic) => {
            timings.image_decode_ms += t.elapsed().as_secs_f64() * 1000.0;
            // Step 3: Convert to RGBA8 (adds alpha, linearises palette/gray).
            let t = std::time::Instant::now();
            let img = dynamic.into_rgba8();
            timings.rgba_convert_ms += t.elapsed().as_secs_f64() * 1000.0;
            img
        }
        Err(e) if is_exr(path) => {
            let img =
                decode_exr_rgba(&bytes).map_err(|e2| format!("image: {e}; exr fallback: {e2}"))?;
            timings.image_decode_ms += t.elapsed().as_secs_f64() * 1000.0;
            #[cfg(feature = "metrics")]
            eprintln!(
                "[idtx-render] exr fallback decoded {}x{} ({:.1}ms)",
                img.width(),
                img.height(),
                timings.image_decode_ms
            );
            img
        }
        Err(e) => return Err(e.to_string()),
    };

    Ok(img)
}

/// True for `OpenEXR` source files (by extension), which route to the `exr`-crate
/// fallback when the `image` crate's backend rejects the channel layout.
///
/// Package-path aware (see [`effective_asset_path`]): a `.usdz`-embedded
/// `…[tile/foo_Metallic.exr]` is correctly recognised as EXR.
fn is_exr(path: &str) -> bool {
    asset_extension(path).eq_ignore_ascii_case("exr")
}

/// Decode an `OpenEXR` image via the `exr` crate into an 8-bit `RgbaImage`.
///
/// Handles channel layouts the `image` crate rejects — in particular the
/// single-channel grayscale data maps (roughness/metallic/occlusion) common in
/// USD asset libraries. Channel mapping:
/// * named `R`/`G`/`B` (case-insensitive) → those channels; `A` if present else 1.0.
/// * otherwise the first channel is replicated into R, G, and B (A = 1.0), so the
///   value reads correctly regardless of which channel index the material's
///   swizzle selects (single-channel data maps normally report channel 0 = R).
///
/// EXR samples are already linear scene-referred values (matching how
/// [`source_is_linear`] uploads `.exr` colour textures as UNORM); each is clamped
/// to `[0, 1]` and quantised to u8, matching `DynamicImage::into_rgba8`.
fn decode_exr_rgba(bytes: &[u8]) -> Result<image::RgbaImage, String> {
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

    // Locate named colour channels case-insensitively; fall back to positional.
    let find = |name: &str| {
        channels
            .iter()
            .position(|c| c.name.to_string().eq_ignore_ascii_case(name))
    };
    let (r_idx, g_idx, b_idx) = match (find("R"), find("G"), find("B")) {
        (Some(r), Some(g), Some(b)) => (r, g, b),
        // Single-/unnamed-channel data map: replicate channel 0 into RGB.
        _ => (0, 0, 0),
    };
    let a_idx = find("A");

    let sample = |ch: usize, x: usize, y: usize| -> f32 {
        channels[ch]
            .sample_data
            .value_by_flat_index(y * width + x)
            .to_f32()
    };
    let quantise = |v: f32| -> u8 { clamp_to_u8((v.clamp(0.0, 1.0) * 255.0).round()) };

    let mut out = image::RgbaImage::new(u32_sat(width), u32_sat(height));
    for y in 0..height {
        for x in 0..width {
            let a = a_idx.map_or(255, |ai| quantise(sample(ai, x, y)));
            out.put_pixel(
                u32_sat(x),
                u32_sat(y),
                image::Rgba([
                    quantise(sample(r_idx, x, y)),
                    quantise(sample(g_idx, x, y)),
                    quantise(sample(b_idx, x, y)),
                    a,
                ]),
            );
        }
    }
    Ok(out)
}

/// CPU-only image decode: reads from disk (or resolver), applies the resolution
/// cap and mip-generation settings from `config`, then returns
/// `(width, height, mip_count, packed_pixels, timings)`. No GPU resources required.
fn decode_image_cpu(
    path: &str,
    reader: Option<&ReadFnCallback>,
    config: TextureDecodeConfig,
) -> Result<(u32, u32, u32, Vec<u8>, TextureDecodeTimings), String> {
    let mut timings = TextureDecodeTimings::default();

    // Steps 1–3: read + decode + RGBA convert.
    let img = decode_rgba_bounded(path, reader, &mut timings, config.max_decode_bytes, config.max_dimension)?;

    // Step 4: Apply resolution cap using the configured downscale algorithm.
    let t = std::time::Instant::now();
    let img = if let Some(max_dim) = config.max_dimension {
        let (w, h) = img.dimensions();
        if w.max(h) > max_dim {
            let scale = max_dim.as_f32() / w.max(h).as_f32();
            let nw = f32_to_u32((w.as_f32() * scale).round()).max(1);
            let nh = f32_to_u32((h.as_f32() * scale).round()).max(1);
            do_downscale(img, nw, nh, config.downscale_algo)
        } else {
            img
        }
    } else {
        img
    };
    timings.cap_resize_ms = t.elapsed().as_secs_f64() * 1000.0;

    let (width, height) = img.dimensions();

    // Step 5: Build mip chain using the configured algorithm.
    let t = std::time::Instant::now();
    let (mip_count, pixels) = if config.generate_mipmaps {
        build_mip_chain_with_algo(img, config.downscale_algo)
    } else {
        (1, img.into_raw())
    };
    timings.mip_chain_ms = t.elapsed().as_secs_f64() * 1000.0;

    Ok((width, height, mip_count, pixels, timings))
}

/// GPU upload of already-decoded pixel data. Fast — no disk I/O.
#[allow(clippy::too_many_arguments)]
fn upload_decoded(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    label: &str,
    format: wgpu::TextureFormat,
    width: u32,
    height: u32,
    mip_level_count: u32,
    pixels: &[u8],
) -> GpuTexture {
    let texture = device.create_texture_with_data(
        queue,
        &wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        },
        wgpu::util::TextureDataOrder::LayerMajor,
        pixels,
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    GpuTexture {
        texture,
        view,
        format,
        width,
        height,
    }
}

/// Resamples an image to `(w, h)` for UDIM common-resolution array layers.
/// Handles both up- and down-scaling (tiles may be smaller or larger than the
/// array's common size); the integer box path only downscales, so upscaling
/// always uses a filtered `image::imageops::resize`.
fn resample_common(img: image::RgbaImage, w: u32, h: u32, algo: DownscaleAlgo) -> image::RgbaImage {
    let (sw, sh) = img.dimensions();
    if sw == w && sh == h {
        return img;
    }
    if w <= sw && h <= sh {
        return do_downscale(img, w, h, algo);
    }
    let filter = algo
        .as_filter_type()
        .unwrap_or(image::imageops::FilterType::Triangle);
    image::imageops::resize(&img, w, h, filter)
}

/// GPU upload of an already-assembled UDIM `D2Array`. `pixels` is the layer-major
/// concatenation of each layer's full mip chain (all layers share `width`,
/// `height`, `mip_level_count`). Creates a `D2Array` view over the array.
#[allow(clippy::too_many_arguments)]
fn upload_udim_array(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    label: &str,
    format: wgpu::TextureFormat,
    width: u32,
    height: u32,
    mip_level_count: u32,
    layer_count: u32,
    tile_to_layer: &[u32; 100],
    pixels: &[u8],
) -> GpuUdimTexture {
    let texture = device.create_texture_with_data(
        queue,
        &wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: layer_count,
            },
            mip_level_count,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        },
        wgpu::util::TextureDataOrder::LayerMajor,
        pixels,
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor {
        dimension: Some(wgpu::TextureViewDimension::D2Array),
        ..Default::default()
    });
    GpuUdimTexture {
        texture,
        view,
        format,
        width,
        height,
        layer_count,
        tile_to_layer: *tile_to_layer,
    }
}

/// Builds the 1×1×1 null UDIM array: a single white layer with every tile
/// mapped to the sentinel, so bound-but-unused UDIM channels never sample it.
fn create_null_udim(device: &wgpu::Device, queue: &wgpu::Queue) -> GpuUdimTexture {
    upload_udim_array(
        device,
        queue,
        "null_udim",
        wgpu::TextureFormat::Rgba8Unorm,
        1,
        1,
        1,
        1,
        &[UDIM_LAYER_SENTINEL; 100],
        &[255, 255, 255, 255],
    )
}

fn create_1x1(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    format: wgpu::TextureFormat,
    rgba: [u8; 4],
    label: &str,
) -> GpuTexture {
    let texture = device.create_texture_with_data(
        queue,
        &wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        },
        wgpu::util::TextureDataOrder::LayerMajor,
        &rgba,
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    GpuTexture {
        texture,
        view,
        format,
        width: 1,
        height: 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_token_round_trips_named_presets() {
        for cfg in [
            TextureDecodeConfig::FULL_RESOLUTION,
            TextureDecodeConfig::AUTHORING,
            TextureDecodeConfig::INSPECTOR,
        ] {
            let token = cfg.profile_token().expect("named preset has a token");
            assert_eq!(
                TextureDecodeConfig::from_profile_token(token),
                Some(cfg),
                "token {token:?} must round-trip"
            );
        }
        // Friendly aliases and case-insensitivity.
        assert_eq!(
            TextureDecodeConfig::from_profile_token("  Full  "),
            Some(TextureDecodeConfig::FULL_RESOLUTION)
        );
        assert_eq!(
            TextureDecodeConfig::from_profile_token("HIGH"),
            Some(TextureDecodeConfig::AUTHORING)
        );
        assert_eq!(TextureDecodeConfig::from_profile_token("nonsense"), None);
        // RUNTIME matches no named token (resolution-capless + mipless).
        assert_eq!(TextureDecodeConfig::RUNTIME.profile_token(), None);
    }

    // Regression (payload-composed toy_drummer body nulled to grey): a cold
    // whole-stage prefetch admits against max_upload_bytes in a single pass, so
    // if it is smaller than max_resident_bytes the stage can never fill residency
    // and whichever textures sort last are permanently replaced with the null
    // fallback. Every preset must keep upload >= resident.
    #[test]
    fn preset_upload_budget_is_at_least_resident_budget() {
        for cfg in [
            TextureDecodeConfig::INSPECTOR,
            TextureDecodeConfig::AUTHORING,
            TextureDecodeConfig::FULL_RESOLUTION,
            TextureDecodeConfig::RUNTIME,
        ] {
            assert!(
                cfg.max_upload_bytes >= cfg.max_resident_bytes,
                "upload budget ({}) must be >= resident budget ({}) so a cold \
                 prefetch can fill residency without nulling late-sorted textures",
                cfg.max_upload_bytes,
                cfg.max_resident_bytes,
            );
        }
    }

    // Desktop/PCVR preview (AUTHORING) and packaging (FULL_RESOLUTION) must give a
    // stage's combined textures far more room than the memory-constrained
    // standalone viewer (INSPECTOR), so dense multi-asset levels do not starve.
    #[test]
    fn desktop_presets_have_larger_combined_budget_than_the_constrained_viewer() {
        assert!(
            TextureDecodeConfig::AUTHORING.max_resident_bytes
                > TextureDecodeConfig::INSPECTOR.max_resident_bytes
        );
        assert!(
            TextureDecodeConfig::FULL_RESOLUTION.max_resident_bytes
                >= TextureDecodeConfig::AUTHORING.max_resident_bytes
        );
    }

    /// Encode a single-channel (named `Y`) `OpenEXR` to an in-memory buffer.
    /// This is exactly the layout the `image` crate's `OpenEXR` backend rejects
    /// ("image does not contain non-deep rgb channels"), forcing the fallback.
    fn single_channel_exr(w: usize, h: usize, value: f32) -> Vec<u8> {
        use exr::prelude::*;
        let pixels = SpecificChannels::build()
            .with_channel("Y")
            .with_pixel_fn(move |_pos| (value,));
        let image = Image::from_channels((w, h), pixels);
        let mut cursor = std::io::Cursor::new(Vec::new());
        image
            .write()
            .to_buffered(&mut cursor)
            .expect("write single-channel exr");
        cursor.into_inner()
    }

    /// Encode a three-channel R/G/B `OpenEXR` to an in-memory buffer.
    fn rgb_exr(w: usize, h: usize, r: f32, g: f32, b: f32) -> Vec<u8> {
        use exr::prelude::*;
        let pixels = SpecificChannels::build()
            .with_channel("R")
            .with_channel("G")
            .with_channel("B")
            .with_pixel_fn(move |_pos| (r, g, b));
        let image = Image::from_channels((w, h), pixels);
        let mut cursor = std::io::Cursor::new(Vec::new());
        image
            .write()
            .to_buffered(&mut cursor)
            .expect("write rgb exr");
        cursor.into_inner()
    }

    fn q(v: f32) -> u8 {
        clamp_to_u8((v.clamp(0.0, 1.0) * 255.0).round())
    }

    /// Encode an `w`x`h` RGBA PNG whose header advertises the given dimensions.
    /// Only the header is inspected by `validate_decode`, so a flat fill keeps
    /// the encoded buffer tiny regardless of the advertised source size.
    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut cursor = std::io::Cursor::new(Vec::new());
        image::RgbaImage::from_pixel(w, h, image::Rgba([120, 130, 140, 255]))
            .write_to(&mut cursor, image::ImageFormat::Png)
            .expect("write png");
        cursor.into_inner()
    }

    // Regression (S21 decode-budget over-rejection): `validate_decode` must
    // estimate the transient working set against the *capped* resident size, not
    // the uncapped source size. A 2048/4096/8192 source (the toy_biplane and
    // SimReady toy_block cases) decodes to a ~1024px capped footprint of a few
    // MB and MUST be admitted under the INSPECTOR budget — the previous
    // `w*h*channels*16` on uncapped dims rejected it and silently substituted a
    // grey 1x1 null. 8K in particular is the common production/SimReady maximum.
    #[test]
    fn validate_decode_admits_capped_production_sources() {
        let cfg = TextureDecodeConfig::INSPECTOR;
        for dim in [2048u32, 4096, 8192] {
            let bytes = png(dim, dim);
            assert!(
                validate_decode(&bytes, cfg.max_decode_bytes, cfg.max_dimension).is_ok(),
                "{dim}x{dim} source must be admitted (cap={:?}, budget={} MiB)",
                cfg.max_dimension,
                cfg.max_decode_bytes >> 20,
            );
        }
    }

    // The cap only bounds the resident/mips term; a source whose full-resolution
    // decode alone exceeds the budget is still rejected, so the ceiling is not
    // removed, merely corrected. 16K RGBA source decode is 1 GiB (> 512 MiB).
    #[test]
    fn validate_decode_still_rejects_pathological_source() {
        let cfg = TextureDecodeConfig::INSPECTOR;
        let bytes = png(16384, 16384);
        assert!(validate_decode(&bytes, cfg.max_decode_bytes, cfg.max_dimension).is_err());
    }

    // Without a cap (conductor/full-resolution packaging) the resident term uses
    // the source size, so the estimate scales with the real footprint.
    #[test]
    fn validate_decode_uncapped_scales_with_source() {
        let bytes = png(4096, 4096);
        // 4096^2 RGBA source (64 MiB) + uncapped resident/mips (~192 MiB) > 128 MiB.
        assert!(validate_decode(&bytes, 128 * 1024 * 1024, None).is_err());
        // Generous budget admits it.
        assert!(validate_decode(&bytes, 512 * 1024 * 1024, None).is_ok());
    }

    // A single-channel EXR (the roughness/metallic case) decodes via the exr
    // fallback and replicates its lone channel into R, G, and B so the shader's
    // channel swizzle reads the correct value on any of r/g/b.
    #[test]
    fn exr_fallback_single_channel_replicates_to_rgb() {
        let bytes = single_channel_exr(4, 4, 0.5);
        let img = decode_exr_rgba(&bytes).expect("single-channel exr should decode via fallback");
        assert_eq!(img.dimensions(), (4, 4));
        let px = img.get_pixel(1, 2).0;
        assert_eq!(px[0], q(0.5));
        assert_eq!(px[1], px[0], "G must equal R (replicated)");
        assert_eq!(px[2], px[0], "B must equal R (replicated)");
        assert_eq!(px[3], 255, "alpha defaults opaque when absent");
    }

    // A named-RGB EXR maps R/G/B to the matching output channels (order-agnostic,
    // by name) with an opaque alpha when no `A` channel is present.
    #[test]
    fn exr_fallback_rgb_maps_named_channels() {
        let bytes = rgb_exr(2, 2, 0.1, 0.5, 0.9);
        let img = decode_exr_rgba(&bytes).expect("rgb exr should decode via fallback");
        assert_eq!(img.dimensions(), (2, 2));
        let px = img.get_pixel(0, 0).0;
        assert_eq!(px, [q(0.1), q(0.5), q(0.9), 255]);
    }

    // Full CPU decode path: a resolver serving single-channel EXR bytes for an
    // `.exr` path succeeds because `decode_rgba_raw` falls back from `image` to
    // the `exr` crate. This reproduces the originally reported load failure.
    #[test]
    fn decode_rgba_raw_falls_back_for_exr_path() {
        let bytes = single_channel_exr(8, 8, 0.25);
        let reader = move |_p: &str| Some(bytes.clone());
        let mut timings = TextureDecodeTimings::default();
        let img = decode_rgba_raw("/assets/foo_Roughness.exr", Some(&reader), &mut timings)
            .expect("exr path should fall back and decode");
        assert_eq!(img.dimensions(), (8, 8));
        assert_eq!(img.get_pixel(3, 3).0[0], q(0.25));
    }

    // The fallback is EXR-only: a non-EXR path whose bytes fail to decode must
    // surface the original `image` error rather than being swallowed.
    #[test]
    fn decode_rgba_raw_propagates_non_exr_error() {
        let reader = |_p: &str| Some(b"not a real png".to_vec());
        let mut timings = TextureDecodeTimings::default();
        let err = decode_rgba_raw("/assets/broken.png", Some(&reader), &mut timings)
            .expect_err("corrupt non-exr bytes must error");
        assert!(
            !err.contains("exr fallback"),
            "must not attempt exr fallback for .png: {err}"
        );
    }

    // USD package-path syntax: `.usdz`-embedded assets arrive as
    // `pkg.usdz[inner/asset.ext]`. Extension detection must see through the trailing
    // `]` or the EXR fallback / linear-colour handling silently disengage for packed
    // scenes (the reported bar-scene regression: works unpacked, fails packaged).
    #[test]
    fn asset_extension_unwraps_usd_package_path() {
        assert_eq!(asset_extension("/assets/foo_Roughness.exr"), "exr");
        assert_eq!(
            asset_extension(r"C:\p\barScene.usdz[140/bar_island_1001_Metallic.exr]"),
            "exr"
        );
        // Nested packages unwrap to the innermost entry.
        assert_eq!(asset_extension("a.usdz[b.usdz[c.png]]"), "png");
        assert_eq!(asset_extension("plain.png"), "png");
    }

    #[test]
    fn is_exr_and_linear_see_through_package_path() {
        let packed = r"C:\p\barScene.usdz[140/bar_island_1001_Metallic.exr]";
        assert!(is_exr(packed), "packed .exr must route to the exr fallback");
        assert!(
            source_is_linear(packed),
            "packed .exr must be treated as linear"
        );
        let packed_png = "scene.usdz[tex/albedo.png]";
        assert!(!is_exr(packed_png));
        assert!(!source_is_linear(packed_png));
    }

    // End-to-end: a single-channel EXR served for a `.usdz`-embedded path now falls
    // back and decodes (previously the trailing `]` defeated `is_exr`, so the
    // `image`-crate error propagated and the texture failed to load).
    #[test]
    fn decode_rgba_raw_falls_back_for_packed_exr_path() {
        let bytes = single_channel_exr(8, 8, 0.5);
        let reader = move |_p: &str| Some(bytes.clone());
        let mut timings = TextureDecodeTimings::default();
        let img = decode_rgba_raw(
            r"C:\p\barScene.usdz[140/bar_island_1001_Roughness.exr]",
            Some(&reader),
            &mut timings,
        )
        .expect("packed exr path should fall back and decode");
        assert_eq!(img.dimensions(), (8, 8));
        assert_eq!(img.get_pixel(1, 1).0[0], q(0.5));
    }
}
