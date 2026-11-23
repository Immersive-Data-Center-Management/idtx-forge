// Menu action sender. The selected action's event name is authored project data.
// Capabilities: widget, scene_query, events.
use idtx_behavior_sdk::prelude::*;
use scene::RelationshipTarget;

#[derive(Deserialize)]
#[serde(crate = "idtx_behavior_sdk::serde")]
struct Config<'a> {
    reset: &'a str,
    help: &'a str,
}

#[derive(Default)]
pub struct Menu;

#[behavior(kind = "runtime")]
impl Menu {
    fn on_start(&mut self, ctx: &Ctx) -> Result<()> {
        ctx.widget().on_click("Reset", Self::reset)?;
        ctx.widget().on_click("Help", Self::help)
    }
    fn reset(&mut self, ctx: &Ctx, _: &WidgetClick) -> Result<()> {
        self.select(ctx, true)
    }
    fn help(&mut self, ctx: &Ctx, _: &WidgetClick) -> Result<()> {
        self.select(ctx, false)
    }
    fn select(&mut self, ctx: &Ctx, reset: bool) -> Result<()> {
        let mut scratch = [0; 8192];
        let config: Config<'_> = ctx.context(&mut scratch)?;
        let mut targets = [RelationshipTarget::default(); 1];
        let binding =
            ctx.scene()
                .relationship_targets(ctx.entity(), "coordinator", &mut targets)?;
        if !binding.resolved || binding.total != 1 {
            return Err(ErrorCode::InvalidArgument.into());
        }
        events::post(
            targets[0].entity().ok_or(ErrorCode::NotFound)?,
            if reset { config.reset } else { config.help },
            &[],
        )
    }
}
