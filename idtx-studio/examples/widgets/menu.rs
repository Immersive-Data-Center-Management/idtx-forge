// Reusable policy: item/action bindings and logical aliases live in USD context.
// The selecting controller's stick is primary; a co-located ray is not used.
use idtx_behavior_sdk::prelude::*;
use idtx_behavior_sdk::{
    arrayvec::{ArrayString, ArrayVec},
    input::{CaptureToken, PointerSource},
};

#[derive(Deserialize)]
#[serde(crate = "idtx_behavior_sdk::serde")]
struct Binding<'a> {
    path: &'a str,
    event: &'a str,
}

#[derive(Deserialize)]
#[serde(crate = "idtx_behavior_sdk::serde")]
struct Config<'a> {
    container: &'a str,
    pointer: u32,
    priority: i32,
    open: &'a str,
    navigate: &'a str,
    confirm: &'a str,
    cancel: &'a str,
    next: &'a str,
    previous: &'a str,
    #[serde(borrow)]
    capture: ArrayVec<&'a str, 16>,
    #[serde(borrow)]
    actions: ArrayVec<Binding<'a>, 16>,
}

#[derive(Default)]
pub struct Menu {
    capture: Option<CaptureToken>,
    selected: ArrayString<256>,
    open: bool,
    stick_mode: bool,
}

fn target(ctx: &Ctx, relationship: &str) -> Option<Entity> {
    let mut targets = [scene::RelationshipTarget::default(); 1];
    let result = ctx
        .scene()
        .relationship_targets(ctx.entity(), relationship, &mut targets)
        .ok()?;
    (result.resolved && result.total == 1)
        .then(|| targets[0].entity())
        .flatten()
}

#[behavior(kind = "runtime")]
impl Menu {
    fn on_attach(&mut self, ctx: &Ctx) -> Result<()> {
        let mut scratch = [0; 8192];
        let config: Config<'_> = ctx.context(&mut scratch)?;
        for binding in &config.actions {
            ctx.widget().on_click(binding.path, Self::activate)?;
        }
        self.close(ctx)
    }

    fn on_input_capture(&mut self, ctx: &Ctx) -> Result<()> {
        let mut scratch = [0; 8192];
        let config: Config<'_> = ctx.context(&mut scratch)?;
        let source = PointerSource::from_u32(config.pointer);
        if !self.open && input::action(config.open).from_source(source).pressed() {
            let actions: ArrayVec<_, 16> = config
                .capture
                .iter()
                .map(|name| input::action(name))
                .collect();
            self.capture = Some(CaptureToken::acquire(
                target(ctx, "player")
                    .unwrap_or(Entity::from_wire(idtx_behavior_sdk::abi::Entity::NULL)),
                ctx.entity(),
                source,
                config.priority,
                &actions,
            )?);
            self.open = true;
            ctx.stage_edit().set_visibility(ctx.entity(), true)?;
        }
        Ok(())
    }

    fn on_update(&mut self, ctx: &Ctx, _dt: f32) -> Result<()> {
        if !self.open {
            return Ok(());
        }
        if !self.capture.as_ref().is_some_and(CaptureToken::active) {
            return self.close(ctx);
        }
        let mut scratch = [0; 8192];
        let config: Config<'_> = ctx.context(&mut scratch)?;
        let source = PointerSource::from_u32(config.pointer);
        let action = |name| input::action(name).from_source(source);
        if action(config.cancel).pressed() {
            return self.close(ctx);
        }
        // Existing world-transform APIs, with explicit parent-relative conversion.
        if let (Some(controller), Some(offset)) =
            (target(ctx, "controller"), target(ctx, "attachmentOffset"))
        {
            let parent = ctx.entity().parent()?;
            let parent = if parent.is_null() {
                Mat4::IDENTITY
            } else {
                parent.world_matrix()?
            };
            let local = math::local_in_parent(
                controller.world_matrix()?,
                offset.get_xform()?.to_matrix(),
                parent,
            );
            ctx.entity().set_xform(&Transform::from_matrix(local))?;
        }
        let (x, y) = action(config.navigate).axis2();
        let step =
            i32::from(action(config.next).pressed()) - i32::from(action(config.previous).pressed());
        // With neutral stick, retain keyboard selection until a navigation input.
        if step != 0 {
            self.stick_mode = false;
        } else if x != 0.0 || y != 0.0 {
            self.stick_mode = true;
        }
        if step != 0 || self.stick_mode {
            self.selected = ctx.widgets().navigate(
                ctx.entity(),
                config.container,
                &self.selected,
                [x, y],
                step,
            );
        }
        ctx.widgets().highlight(ctx.entity(), &self.selected)?;
        if action(config.confirm).pressed() && !self.selected.is_empty() {
            let _ = ctx.widgets().activate(
                ctx.entity(),
                &self.selected,
                source,
                action(config.confirm),
            );
        }
        Ok(())
    }

    fn activate(&mut self, ctx: &Ctx, click: &WidgetClick) -> Result<()> {
        let mut scratch = [0; 8192];
        let config: Config<'_> = ctx.context(&mut scratch)?;
        let path = click.element();
        if let Some(binding) = config.actions.iter().find(|b| b.path == path.as_str()) {
            if let Some(coordinator) = target(ctx, "coordinator") {
                events::post(
                    coordinator,
                    binding.event,
                    &(click.source() as u32).to_le_bytes(),
                )?;
            }
        }
        self.close(ctx)
    }

    fn close(&mut self, ctx: &Ctx) -> Result<()> {
        self.capture = None;
        self.open = false;
        self.stick_mode = false;
        self.selected.clear();
        ctx.widgets().highlight(ctx.entity(), "")?;
        ctx.stage_edit().set_visibility(ctx.entity(), false)
    }

    fn on_stop(&mut self, ctx: &Ctx) -> Result<()> {
        self.close(ctx)
    }
    fn on_detach(&mut self, ctx: &Ctx) -> Result<()> {
        self.close(ctx)
    }
}
