use idtx_behavior_sdk::prelude::*;

#[path = "../shared/motion.rs"]
mod motion;

#[derive(Default)]
pub struct SharedRoll {
    base: Transform,
    angle: f32,
}

#[behavior(kind = "runtime")]
impl SharedRoll {
    fn on_start(&mut self, ctx: &Ctx) -> Result<()> {
        let owner = ctx.entity();
        let parent_world = owner.parent()?.world_matrix()?;
        self.base = Transform::from_matrix(parent_world.inverse() * owner.world_matrix()?);
        self.angle = 0.0;
        Ok(())
    }

    fn on_update(&mut self, ctx: &Ctx, dt: f32) -> Result<()> {
        self.angle = motion::advance_angle(self.angle, 0.5, dt);
        let mut pose = self.base;
        pose.rotation = self.base.rotation * Quat::from_rotation_z(self.angle);
        ctx.stage_edit().set_self_transform(&pose)
    }

    fn on_stop(&mut self, ctx: &Ctx) -> Result<()> {
        self.angle = 0.0;
        ctx.stage_edit().set_self_transform(&self.base)
    }
}
