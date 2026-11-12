//! Whole adapter-frame timing, including early returns and transition work.
use std::{collections::VecDeque, sync::{Arc, Mutex}, time::Instant};

#[derive(Default)]
struct Window {
    frames: VecDeque<(u64, u64)>,
    last: Option<Instant>,
    peak_rss: Option<u64>,
    count: u64,
    context: serde_json::Value,
}
pub struct FrameMetrics { state: Arc<Mutex<Window>>, name: String, budget_ns: u64 }
pub struct FrameGuard { state: Arc<Mutex<Window>>, start: Instant }

impl FrameMetrics {
    pub fn new(name: impl Into<String>, budget_ns: u64) -> Self {
        Self { state: Arc::new(Mutex::new(Window::default())), name: name.into(), budget_ns }
    }
    #[must_use]
    pub fn start(&self) -> FrameGuard { FrameGuard { state: self.state.clone(), start: Instant::now() } }
    pub fn configure(&self, scene: &std::path::Path, viewport: [u32; 2], render_scale: f32, features: wgpu::Features, flags: &str) {
        if let Ok(mut state) = self.state.lock() {
            let label = scene.file_name().and_then(|s| s.to_str()).unwrap_or("scene");
            let label = label.split(['?', '#']).next().unwrap_or("scene").rsplit('@').next().unwrap_or("scene");
            state.context = serde_json::json!({ "scene": label,
                "host_viewport": viewport, "render_scale": render_scale,
                "gpu_features": format!("{features:?}"), "runtime_features": flags });
        }
    }
    /// External compositor boundary, measured before query/wait through present.
    pub fn record_external(&self, elapsed_ns: u64) {
        let now = Instant::now();
        record(&self.state, now.checked_sub(std::time::Duration::from_nanos(elapsed_ns)).unwrap_or(now), elapsed_ns);
    }
    pub fn reset_pacing(&self) { if let Ok(mut state) = self.state.lock() { state.last = None; } }
    pub fn report(&self) -> serde_json::Value {
        let state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let p95 = |values: Vec<u64>| {
            let mut values = values; values.sort_unstable();
            values.get(values.len().saturating_mul(95).div_ceil(100).saturating_sub(1)).copied().unwrap_or(0)
        };
        let cpu = p95(state.frames.iter().map(|f| f.0).collect());
        let interval = p95(state.frames.iter().filter(|f| f.1 != 0).map(|f| f.1).collect());
        serde_json::json!({ "schema_version": 1, "revision": env!("IDTX_CAPTURE_REVISION"),
            "adapter": self.name, "os": std::env::consts::OS, "arch": std::env::consts::ARCH,
            "last_frame_context": state.context,
            "frames": state.count, "window": state.frames.len(), "cpu_frame_p95_ns": cpu,
            "interval_p95_ns": interval, "peak_rss_bytes": state.peak_rss,
            "frame_budget_ns": self.budget_ns, "rss_budget_bytes": 2_u64 * 1024 * 1024 * 1024,
            "cpu_budget_met": cpu <= self.budget_ns, "pacing_budget_met": interval <= self.budget_ns,
            "rss_budget_met": state.peak_rss.map(|rss| rss <= 2_u64 * 1024 * 1024 * 1024) })
    }
}
impl Drop for FrameGuard {
    fn drop(&mut self) { record(&self.state, self.start, self.start.elapsed().as_nanos().try_into().unwrap_or(u64::MAX)); }
}
fn record(state: &Mutex<Window>, start: Instant, elapsed: u64) {
    let mut state = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let interval = state.last.map_or(0, |last| start.saturating_duration_since(last).as_nanos().try_into().unwrap_or(u64::MAX));
    state.last = Some(start); state.count += 1;
    if state.frames.len() == 300 { state.frames.pop_front(); }
    state.frames.push_back((elapsed, interval));
    #[cfg(unix)]
    {
        let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
        // SAFETY: getrusage initializes the caller-owned rusage on success.
        if unsafe { libc::getrusage(libc::RUSAGE_SELF, &raw mut usage) } == 0 {
            let bytes = u64::try_from(usage.ru_maxrss).unwrap_or(0);
            let bytes = if cfg!(target_vendor = "apple") { bytes } else { bytes.saturating_mul(1024) };
            state.peak_rss = Some(state.peak_rss.unwrap_or(0).max(bytes));
        }
    }
}
impl Drop for FrameMetrics {
    fn drop(&mut self) {
        let report = self.report();
        if report["frames"].as_u64() == Some(0) { return; }
        let dir = idtx_studio_session::xr_metrics::metrics_output_dir();
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
        let path = dir.join(format!("idtx-whole-frame-{stamp}.json"));
        if let Ok(bytes) = serde_json::to_vec_pretty(&report) { let _ = std::fs::write(path, bytes); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn entire_guard_lifetime_and_bounded_external_window_are_reported() {
        let metrics = FrameMetrics::new("test", 11_111_111);
        { let _frame = metrics.start(); std::thread::sleep(std::time::Duration::from_millis(5)); }
        assert!(metrics.report()["cpu_frame_p95_ns"].as_u64().unwrap() >= 5_000_000);
        for _ in 0..400 { metrics.record_external(20_000_000); }
        let report = metrics.report();
        assert_eq!(report["frames"], 401); assert_eq!(report["window"], 300);
        assert_eq!(report["cpu_frame_p95_ns"], 20_000_000);
        assert_eq!(report["cpu_budget_met"], false);
        assert!(!report["revision"].as_str().unwrap().is_empty());
    }
}
