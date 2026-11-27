// Reusable task panel. Capabilities: widget, scene_query, events.
use idtx_behavior_sdk::prelude::*;
use scene::RelationshipTarget;

#[derive(Deserialize)]
#[serde(crate = "idtx_behavior_sdk::serde")]
struct Config<'a> {
    event: &'a str,
}

#[derive(Default)]
pub struct Panel;

#[behavior(kind = "runtime")]
impl Panel {
    fn on_start(&mut self, ctx: &Ctx) -> Result<()> {
        ctx.widget().on_click("Start", Self::start)
    }

    fn start(&mut self, ctx: &Ctx, _: &WidgetClick) -> Result<()> {
        let mut scratch = [0; 8192];
        let config: Config<'_> = ctx.context(&mut scratch)?;
        // Read the shared current snapshot, not a handle cached at attachment:
        // references, variants and payload state can change during execution.
        let mut targets = [RelationshipTarget::default(); 1];
        let binding =
            ctx.scene()
                .relationship_targets(ctx.entity(), "coordinator", &mut targets)?;
        if !binding.resolved || binding.total != 1 {
            return Err(ErrorCode::InvalidArgument.into());
        }
        let coordinator = targets[0].entity().ok_or(ErrorCode::NotFound)?;
        events::post(coordinator, config.event, &[])
    }
}
