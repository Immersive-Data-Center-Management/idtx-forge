// Attach to an existing native PointInstancer with two emissive prototypes.
// Capability: stage_edit. 256 placements fit the default 64 KiB argument budget.
// Stable signed ids, not entity handles. Parents/prototypes supply their own USD transforms.
use idtx_behavior_sdk::prelude::*;

const COUNT: usize = 256;
const WINDOW: usize = 16;

#[derive(Default)]
pub struct VisibilityWindow {
    pending: Option<StageEditTicket>,
    start: usize,
    elapsed: f32,
}

#[behavior(kind = "runtime")]
impl VisibilityWindow {
    fn on_start(&mut self, ctx: &Ctx) -> Result<()> {
        let entries: [PointInstance; COUNT] = core::array::from_fn(|i| PointInstance {
            id: 10_000 + i as i64 * 7,
            prototype_index: (i % 2) as u32,
            position: [(i % 32) as f32 * 0.15, 0.0, (i / 32) as f32 * 0.15],
            ..PointInstance::default()
        });
        self.pending = Some(
            ctx.stage_edit()
                .set_point_instances(ctx.entity(), &entries, &[])?,
        );
        Ok(())
    }

    fn on_update(&mut self, ctx: &Ctx, dt: f32) -> Result<()> {
        if let Some(ticket) = self.pending {
            let result = ctx.stage_edit().edit_status(ticket)?;
            if !result.is_terminal() {
                return Ok(());
            }
            self.pending = None;
            if !result.committed() {
                return Err(idtx_behavior_sdk::abi::ErrorCode::from_u32(result.error).into());
            }
        }
        self.elapsed += dt;
        if self.elapsed < 0.1 {
            return Ok(());
        }
        self.elapsed = 0.0;
        self.start = (self.start + 1) % COUNT;
        let mut hidden = [0i64; COUNT - WINDOW];
        let mut n = 0;
        for i in 0..COUNT {
            if (i + COUNT - self.start) % COUNT >= WINDOW {
                hidden[n] = 10_000 + i as i64 * 7;
                n += 1;
            }
        }
        self.pending = Some(
            ctx.stage_edit()
                .set_point_instancer_visibility(ctx.entity(), &hidden)?,
        );
        Ok(())
    }
}
