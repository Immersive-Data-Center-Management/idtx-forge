// Camera pointer. Capabilities: input, widget, scene_query, events.
// Declare widgets.click and select, even if both bind to the same button.
use idtx_behavior_sdk::prelude::*;
use scene::RelationshipTarget;

#[derive(Deserialize)]
#[serde(crate = "idtx_behavior_sdk::serde")]
struct Config<'a> {
    event: &'a str,
}

#[derive(Default)]
pub struct Pointer;

#[behavior(kind = "runtime")]
impl Pointer {
    fn on_update(&mut self, ctx: &Ctx, _: f32) -> Result<()> {
        let Some(camera) = ctx.camera() else {
            return Ok(());
        };
        let Some(hit) = ctx.scene().raycast(
            camera.ray.origin,
            camera.ray.forward,
            &RayOpts::new().block_widgets(),
        ) else {
            return Ok(());
        };
        // Widget consumption owns this physical edge. Never also post a world
        // selection for it, even when select and widgets.click share a binding.
        if hit.is_widget() && ctx.widgets().handle(hit) {
            return Ok(());
        }
        if input::action("select").phase() != InputPhase::Pressed {
            return Ok(());
        }
        let mut targets = [RelationshipTarget::default(); 1];
        let binding =
            ctx.scene()
                .relationship_targets(ctx.entity(), "coordinator", &mut targets)?;
        if !binding.resolved || binding.total != 1 {
            return Err(ErrorCode::InvalidArgument.into());
        }
        let coordinator = targets[0].entity().ok_or(ErrorCode::NotFound)?;
        let mut scratch = [0; 8192];
        let config: Config<'_> = ctx.context(&mut scratch)?;
        events::post(coordinator, config.event, &[])
    }
}
