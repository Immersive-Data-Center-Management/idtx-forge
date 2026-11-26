use idtx_behavior_sdk::prelude::*;

#[derive(Default)]
pub struct SignalPointer;

#[behavior(kind = "runtime")]
impl SignalPointer {
    fn on_update(&mut self, ctx: &Ctx, _dt: f32) -> Result<()> {
        let Some(camera) = ctx.camera() else { return Ok(()); };
        let options = RayOpts::new().interaction_only().block_widgets().max_distance(10.0);
        if let Some(hit) = ctx.scene().raycast(camera.ray.origin, camera.ray.forward, &options) {
            if hit.is_widget() && ctx.widgets().handle(hit) { return Ok(()); }
            let target = ctx.scene().collision_body(hit.entity).unwrap_or(hit.entity);
            ctx.highlight().outline_with(target, Vec4::new(0.1, 1.0, 0.65, 1.0), 4.0)?;
        }
        Ok(())
    }
}

