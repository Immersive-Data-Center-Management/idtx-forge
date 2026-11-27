//! Multi-file authoring example. Open this entry through Conductor's Edit Code.
use idtx_behavior_sdk::prelude::*;

mod motion;

#[derive(Default)]
pub struct ModularSpin {
    angle: f32,
}

#[behavior(kind = "runtime")]
impl ModularSpin {
    fn on_update(&mut self, ctx: &Ctx, dt: f32) -> Result<()> {
        self.angle = motion::advance(self.angle, dt);
        let _ = ctx.scene();
        Ok(())
    }
}
