use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
use idtx_render::texture_cache::{TextureCache, TextureDecodeConfig, TextureKind};

fn gpu() -> (wgpu::Device, wgpu::Queue) {
    pollster::block_on(async {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        instance.request_adapter(&wgpu::RequestAdapterOptions::default()).await.unwrap()
            .request_device(&wgpu::DeviceDescriptor::default()).await.unwrap()
    })
}

fn png(w: u32, h: u32) -> Vec<u8> {
    let mut bytes = std::io::Cursor::new(Vec::new());
    image::RgbaImage::from_pixel(w, h, image::Rgba([100, 120, 140, 255]))
        .write_to(&mut bytes, image::ImageFormat::Png).unwrap();
    bytes.into_inner()
}

#[test]
fn residency_rejection_is_cached_and_eviction_releases_owned_handles() {
    let (device, queue) = gpu();
    let reads = Arc::new(AtomicUsize::new(0));
    let count = reads.clone();
    let mut cache = TextureCache::new_with_reader(&device, &queue, Arc::new(move |_| {
        count.fetch_add(1, Ordering::Relaxed); Some(png(16, 16))
    }), TextureDecodeConfig { max_resident_bytes: 2048, generate_mipmaps: false, ..TextureDecodeConfig::default() });
    let first = cache.get_or_load("a.png", TextureKind::Data, &device, &queue);
    let weak = Arc::downgrade(&first);
    drop(first);
    cache.prefetch_parallel(&[("b.png".into(), TextureKind::Data), ("c.png".into(), TextureKind::Data)], &device, &queue);
    assert_eq!(cache.get_or_load("c.png", TextureKind::Data, &device, &queue).width, 1);
    let before = reads.load(Ordering::Relaxed);
    cache.get_or_load("c.png", TextureKind::Data, &device, &queue);
    assert_eq!(reads.load(Ordering::Relaxed), before);
    assert_eq!(cache.resident_bytes(), 2048);
    cache.retain_paths(&[], &[]);
    assert_eq!(cache.resident_bytes(), 0);
    assert!(weak.upgrade().is_none());
    assert_eq!(cache.get_or_load("c.png", TextureKind::Data, &device, &queue).width, 16);
}

#[test]
fn oversized_decode_and_udim_array_take_null_fallback() {
    let (device, queue) = gpu();
    let mut cache = TextureCache::new_with_reader(&device, &queue, Arc::new(|_| Some(png(128, 128))),
        TextureDecodeConfig { max_decode_bytes: 4096, ..TextureDecodeConfig::default() });
    assert_eq!(cache.get_or_load("huge.png", TextureKind::Data, &device, &queue).width, 1);
    assert_eq!(cache.resident_bytes(), 0);
    let mut cache = TextureCache::new_with_reader(&device, &queue, Arc::new(|_| Some(png(16, 16))),
        TextureDecodeConfig { max_resident_bytes: 1024, generate_mipmaps: false, ..TextureDecodeConfig::default() });
    cache.prefetch_udim_parallel(&[("tile.<UDIM>.png".into(), TextureKind::Data, vec![1002, 1001, 1001])], &device, &queue);
    assert!(cache.udim_get("tile.<UDIM>.png", TextureKind::Data).tile_to_layer.iter().all(|&v| v == u32::MAX));
    assert_eq!(cache.resident_bytes(), 0);
}

#[test]
fn upload_budget_and_device_cap_apply_to_prefetch_and_lazy_load() {
    let (device, queue) = gpu();
    let mut cache = TextureCache::new_with_reader(&device, &queue, Arc::new(|_| Some(png(64, 32))),
        TextureDecodeConfig { max_dimension: Some(16), max_upload_bytes: 512, generate_mipmaps: false, ..TextureDecodeConfig::default() });
    let paths = vec![("a.png".into(), TextureKind::Data), ("b.png".into(), TextureKind::Data)];
    cache.prefetch_parallel(&paths, &device, &queue);
    assert_eq!(cache.get_or_load("a.png", TextureKind::Data, &device, &queue).width, 16);
    assert_eq!(cache.get_or_load("b.png", TextureKind::Data, &device, &queue).width, 1);
    assert_eq!(cache.resident_bytes(), 512);
}

#[test]
fn full_resolution_still_obeys_the_device_dimension_limit() {
    let (device, queue) = gpu();
    let limit = device.limits().max_texture_dimension_2d;
    let mut cache = TextureCache::new_with_reader(&device, &queue, Arc::new(move |_| Some(png(limit + 1, 1))),
        TextureDecodeConfig::FULL_RESOLUTION);
    let texture = cache.get_or_load("wide.png", TextureKind::Data, &device, &queue);
    assert_eq!(texture.width, limit);
    assert_eq!(texture.height, 1);
}

// Regression (S21 decode budget too small for production textures): a large
// source (e.g. the SimReady toy_block ships 8192x8192 PNGs) must decode and
// downscale to the INSPECTOR cap, NOT take the grey 1x1 null fallback. Exercises
// the whole read_bounded -> validate_decode -> admit -> upload chain through the
// default-profile budget rather than the tiny explicit budgets above. A 2048
// source keeps the test cheap while still exceeding the pre-fix 128 MiB ceiling's
// effective per-side limit.
#[test]
fn large_source_decodes_and_downscales_under_inspector() {
    let (device, queue) = gpu();
    let mut cache = TextureCache::new_with_reader(&device, &queue, Arc::new(|_| Some(png(2048, 2048))),
        TextureDecodeConfig::INSPECTOR);
    let tex = cache.get_or_load("big_albedo.png", TextureKind::DiffuseSrgb, &device, &queue);
    assert_eq!(tex.width, 1024, "2048 source must decode and downscale to the 1024 cap, not null (width 1)");
    assert!(cache.resident_bytes() > 0, "a real texture must be resident, not the null fallback");
}

