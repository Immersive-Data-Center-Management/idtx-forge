use idtx_behavior_sdk::prelude::*;
use idtx_behavior_sdk::scene::RelationshipTarget;

#[derive(Deserialize)]
#[serde(crate = "idtx_behavior_sdk::serde")]
struct ControlsConfig {
    actor_relationship: ContextString<64>,
}

#[derive(Default)]
pub struct ExhibitControls {
    relationship: ContextString<64>,
}

#[behavior(kind = "runtime")]
impl ExhibitControls {
    fn on_start(&mut self, ctx: &Ctx) -> Result<()> {
        let mut scratch = [0u8; 256];
        let config: ControlsConfig = ctx.context(&mut scratch)?;
        if config.actor_relationship.is_empty() {
            return Err(ErrorCode::InvalidArgument.into());
        }
        self.relationship = config.actor_relationship;
        // Reject absent, unresolved, unloaded or multi-target bindings explicitly.
        self.actor(ctx)?;
        #[cfg(target_arch = "wasm32")]
        {
            ctx.widget().on_click("React", Self::react_clicked)?;
            ctx.widget().on_click("Pause", Self::pause_clicked)?;
            ctx.widget().on_click("Release", Self::release_clicked)?;
        }
        ctx.stage_edit().set_widget_value("State", "READY")
    }

    fn on_update(&mut self, ctx: &Ctx, _dt: f32) -> Result<()> {
        let actor = self.actor(ctx)?;
        let mut scratch = [0u8; 8192];
        let reaction = ctx.animations().clip_on(actor, "reaction").status(&mut scratch)?;
        let idle = ctx.animations().clip_on(actor, "idle").status(&mut scratch)?;
        let text = if reaction.state.as_str() == "invalid" || idle.state.as_str() == "invalid" {
            "INVALID"
        } else if reaction.independent {
            reaction.state.as_str()
        } else if idle.independent {
            if idle.state.as_str() == "playing" { "IDLE" } else { idle.state.as_str() }
        } else {
            "FOLLOWING"
        };
        // This small example polls state; larger panels can update only on change.
        ctx.stage_edit().set_widget_value("State", text)
    }

    fn actor(&self, ctx: &Ctx) -> Result<Entity> {
        let mut targets = [RelationshipTarget::default(); 1];
        let result = ctx.scene().relationship_targets(
            ctx.entity(), self.relationship.as_str(), &mut targets,
        )?;
        if !result.resolved || result.total != 1 || result.written != 1 {
            return Err(ErrorCode::InvalidArgument.into());
        }
        targets[0].entity().ok_or_else(|| ErrorCode::NotFound.into())
    }

    fn react_clicked(&mut self, ctx: &Ctx, _click: &WidgetClick) -> Result<()> {
        events::post(self.actor(ctx)?, "exhibit.react", &[])
    }

    fn pause_clicked(&mut self, ctx: &Ctx, _click: &WidgetClick) -> Result<()> {
        events::post(self.actor(ctx)?, "exhibit.pause", &[])
    }

    fn release_clicked(&mut self, ctx: &Ctx, _click: &WidgetClick) -> Result<()> {
        events::post(self.actor(ctx)?, "exhibit.release", &[])
    }
}
