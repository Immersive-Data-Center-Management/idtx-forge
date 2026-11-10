//! Three delayed readback slots. Saturation drops a measurement, never waits.
use std::sync::mpsc;

pub(crate) const BYTES: u64 = 80; // 9 timestamps + one pipeline statistic.
struct Slot {
    buffer: wgpu::Buffer,
    pending: Option<mpsc::Receiver<Result<(), wgpu::BufferAsyncError>>>,
    frame: u64,
    shadow: bool,
    view: usize,
}
pub(crate) struct Readbacks { slots: [Slot; 3], next_frame: u64 }
pub(crate) struct Sample { pub ticks: [u64; 10], pub frame: u64, pub shadow: bool }

impl Readbacks {
    pub fn new(device: &wgpu::Device) -> Self {
        Self { slots: std::array::from_fn(|_| Slot {
            buffer: device.create_buffer(&wgpu::BufferDescriptor { label: Some("Delayed GPU queries"), size: BYTES,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false }),
            pending: None, frame: 0, shadow: false, view: 0,
        }), next_frame: 1 }
    }
    pub fn available(&self) -> Option<usize> { self.slots.iter().position(|s| s.pending.is_none()) }
    pub fn buffer(&self, index: usize) -> &wgpu::Buffer { &self.slots[index].buffer }
    pub fn schedule(&mut self, index: usize, shadow: bool, view: usize) {
        let slot = &mut self.slots[index];
        debug_assert!(slot.pending.is_none());
        slot.frame = self.next_frame;
        self.next_frame += 1;
        slot.shadow = shadow;
        slot.view = view;
        let (tx, rx) = mpsc::sync_channel(1);
        slot.buffer.slice(..).map_async(wgpu::MapMode::Read, move |result| { let _ = tx.send(result); });
        slot.pending = Some(rx);
    }
    /// Poll only older submissions. The caller schedules this frame afterwards.
    pub fn collect(&mut self, device: &wgpu::Device, view: usize) -> Option<Sample> {
        let _ = device.poll(wgpu::PollType::Poll);
        let mut latest: Option<Sample> = None;
        for slot in &mut self.slots {
            if slot.view != view { continue; }
            let Some(rx) = &slot.pending else { continue; };
            match rx.try_recv() {
                Err(mpsc::TryRecvError::Empty) => continue,
                Ok(Ok(())) => {
                    let mapped = slot.buffer.slice(..).get_mapped_range();
                    let ticks = std::array::from_fn(|i| u64::from_le_bytes(mapped[i * 8..i * 8 + 8].try_into().unwrap()));
                    if latest.as_ref().is_none_or(|s| s.frame < slot.frame) { latest = Some(Sample { ticks, frame: slot.frame, shadow: slot.shadow }); }
                }
                _ => {}
            }
            slot.buffer.unmap();
            slot.pending = None;
        }
        latest
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn saturated_readbacks_drop_sampling_and_keep_submission_metadata() {
        let (device, queue) = pollster::block_on(async {
            let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
            instance.request_adapter(&wgpu::RequestAdapterOptions::default()).await.unwrap()
                .request_device(&wgpu::DeviceDescriptor::default()).await.unwrap()
        });
        let mut pool = Readbacks::new(&device);
        for i in 0..3 {
            let slot = pool.available().unwrap();
            queue.write_buffer(pool.buffer(slot), 0, &[i as u8; BYTES as usize]);
            queue.submit([]);
            pool.schedule(slot, i == 2, 0);
        }
        assert!(pool.available().is_none());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        let mut last = 0;
        while last < 3 {
            if let Some(sample) = pool.collect(&device, 0) {
                last = sample.frame;
                if last == 3 { assert!(sample.shadow); assert_eq!(sample.ticks[0], u64::from_le_bytes([2; 8])); }
            }
            assert!(std::time::Instant::now() < deadline);
        }
        assert!(pool.available().is_some());
    }
}
