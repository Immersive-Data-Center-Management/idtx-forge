use idtx_behavior_sdk::prelude::*;

#[derive(Default)]
pub struct Message;

#[behavior(kind = "runtime")]
impl Message {
    fn on_start(&mut self, ctx: &Ctx) -> Result<()> {
        ctx.widget().on_panel_click(Self::dismiss)
    }

    fn dismiss(&mut self, ctx: &Ctx, _: &WidgetClick) -> Result<()> {
        // Unhandled labels bubble here, just like blank covered panel areas.
        ctx.stage_edit().set_visibility(ctx.entity(), false)
    }
}
