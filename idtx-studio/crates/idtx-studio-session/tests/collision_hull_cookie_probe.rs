//! Regression test for the `UsdCookie` convex-hull generation that used to fail
//! intermittently (~7/10) with the old `chull` backend. Loads the real 66 MB
//! `UsdCookie.usdz`, gathers the exact point cloud the collision inspector feeds
//! to [`idtx_hull_gen::generate_convex_hull`], and asserts the hull generates
//! reliably — every attempt, deterministically — now that the generator is
//! backed by parry3d.
//!
//! `#[ignore]` because it links the `OpenUSD` monolith (needs `IDTX_USD_DIR` + the
//! runtime libs on the loader path), spins up a headless wgpu device, and reads
//! a large asset:
//!
//! ```bash
//! IDTX_USD_DIR=… \
//!   cargo test -p idtx-studio-session --features authoring \
//!   --test collision_hull_cookie_probe -- --ignored --nocapture
//! ```

#![cfg(feature = "authoring")]

use std::path::PathBuf;

use idtx_hull_gen::HullParams;
use idtx_studio_session::{RenderWorldUnitScale, StudioSession, TextureDecodeConfig};

#[path = "../../../tests/support/mod.rs"]
mod support;

fn fixture(name: &str) -> PathBuf {
    support::usd_fixture(name)
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
                label: Some("idtx-cookie-hull-probe"),
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

#[test]
#[ignore = "requires OpenUSD link + headless GPU + the 66 MB UsdCookie.usdz"]
fn cookie_hull_generates_reliably() {
    idtx_usd_extension::install().expect("register IDTX USD schema plugin");

    let (device, queue) = headless_device();
    let session = StudioSession::open_with_gpu(
        fixture("UsdCookie.usdz"),
        device,
        queue,
        [64, 64],
        TextureDecodeConfig::FULL_RESOLUTION,
        RenderWorldUnitScale::METRE,
    )
    .expect("open UsdCookie.usdz");

    let mesh_paths: Vec<String> = session
        .stage()
        .topology()
        .iter()
        .filter(|r| r.type_name == "Mesh")
        .filter_map(|r| session.stage().path(r.path).map(str::to_owned))
        .collect();
    assert!(!mesh_paths.is_empty(), "cookie must hydrate at least one Mesh");

    for path in &mesh_paths {
        let cloud = session.gather_collision_mesh_points(path);
        if cloud.len() < 4 {
            continue;
        }
        eprintln!("{path}: {} pts", cloud.len());

        // Reliability: the old chull backend failed ~7/10 here. parry must
        // succeed on every attempt and return the same hull each time.
        let first = idtx_hull_gen::generate_convex_hull(&cloud, &HullParams::default())
            .expect("cookie hull must generate");
        assert!(first.points.len() >= 4);
        assert_eq!(first.triangles.len() % 3, 0);
        eprintln!(
            "  hull = {}v / {}t",
            first.points.len(),
            first.triangles.len() / 3
        );
        for _ in 0..19 {
            let again = idtx_hull_gen::generate_convex_hull(&cloud, &HullParams::default())
                .expect("cookie hull must generate on every attempt");
            assert_eq!(again, first, "cookie hull must be deterministic");
        }
    }
}
