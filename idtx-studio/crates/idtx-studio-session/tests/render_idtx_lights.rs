//! Generates **idtx's own** renders of the UsdLux validation stages, through the
//! real editor path (`open_with_gpu` → `initialize_viewport_camera` →
//! `render_one_frame` → `read_pixels`), and writes them to
//! `docs/lights/reference/renders_idtx/<type>.png`.
//!
//! These are the images to compare against the `OpenUSD` **Storm** reference
//! renders in `docs/lights/reference/renders/` and the schematics in
//! `docs/lights/reference/*.svg`. Because the stages carry `/World/Camera` and
//! idtx now auto-selects it, the idtx framing matches the Storm/schematic view.
//! Rendered at 760×554 to match the Storm reference dimensions.
//!
//! `#[ignore]` (image-generation utility): links the `OpenUSD` monolith (needs
//! `IDTX_USD_DIR` + the runtime libs on the loader path) and spins up a
//! headless wgpu device:
//!
//! ```bash
//! IDTX_USD_DIR=… cargo test -p idtx-studio-session --test render_idtx_lights \
//!   -- --ignored --nocapture
//! ```

use std::path::PathBuf;

use idtx_studio_session::{RenderWorldUnitScale, StudioSession, TextureDecodeConfig};

const W: u32 = 760;
const H: u32 = 554;

const STAGES: &[&str] = &[
    "distant",
    "sphere",
    "rect",
    "disk",
    "cylinder",
    "spot_shaping",
    "dome_uniform",
    "dome_textured",
    "color_temp",
];

fn fixture(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/usd")
        .join(rel)
}

fn out_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/lights/reference/renders_idtx")
}

fn headless_device() -> (wgpu::Device, wgpu::Queue) {
    pollster::block_on(async {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: false,
            })
            .await
            .expect("a headless wgpu adapter");
        adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("idtx-light-render"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::default(),
                trace: wgpu::Trace::Off,
                experimental_features: wgpu::ExperimentalFeatures::default(),
            })
            .await
            .expect("a headless wgpu device")
    })
}

#[cfg(feature = "metrics")]
#[test]
#[ignore = "image-generation utility: requires OpenUSD link + headless GPU"]
fn render_idtx_light_references() {
    let dir = out_dir();
    std::fs::create_dir_all(&dir).expect("create renders_idtx dir");
    let (device, queue) = headless_device();

    for name in STAGES {
        let mut session = StudioSession::open_with_gpu(
            fixture(&format!("lighting/types/{name}.usda")),
            device.clone(),
            queue.clone(),
            [W, H],
            TextureDecodeConfig::FULL_RESOLUTION,
            RenderWorldUnitScale::METRE,
        )
        .unwrap_or_else(|e| panic!("open {name}: {e}"));

        // Open through the stage's authored /World/Camera (the editor path), so
        // the framing matches the Storm reference + schematic.
        session.initialize_viewport_camera();
        // A couple of frames so camera selection + texture prefetch settle.
        session.render_one_frame(false);
        session.render_one_frame(false);

        let pixels = session.read_pixels().expect("read_pixels");
        assert_eq!(pixels.len(), (W * H * 4) as usize, "{name}: unexpected pixel count");

        let out = dir.join(format!("{name}.png"));
        image::save_buffer(&out, &pixels, W, H, image::ColorType::Rgba8)
            .unwrap_or_else(|e| panic!("save {name}.png: {e}"));

        // Sanity: the render has shading variation (not a flat clear color).
        let first = &pixels[0..4];
        let varied = pixels.chunks_exact(4).any(|p| p != first);
        assert!(varied, "{name}: rendered image is a single flat color");

        eprintln!("wrote {}", out.display());
    }
}
