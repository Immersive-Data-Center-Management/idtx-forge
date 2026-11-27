// Optional Signal Room exercise: spawn a prepared USD reference and receive
// its terminal result without polling. Attach to a concrete Xform prim.
use idtx_behavior_sdk::prelude::*;

#[derive(Serialize)]
#[serde(crate = "idtx_behavior_sdk::serde")]
struct SpecimenContext {
    label: &'static str,
    room: &'static str,
}

#[derive(Default)]
pub struct SpawnDemo {
    specimen: Option<Entity>,
    pending: bool,
}

#[behavior(kind = "runtime")]
impl SpawnDemo {
    fn on_start(&mut self, ctx: &Ctx) -> Result<()> {
        self.specimen = None;
        self.pending = false;
        let parent = ctx.entity();
        if parent.is_null() {
            log::error("SpawnDemo needs a concrete parent prim");
            return Err(ErrorCode::InvalidArgument.into());
        }
        let pose = Transform {
            translation: Vec3::new(1.8, 1.2, -3.0),
            ..Transform::default()
        };
        let context = SpecimenContext { label: "Signal specimen", room: "lab" };
        match ctx.stage_edit().spawn_reference_async(
            parent, "Specimen", "specimen", &pose, &context, Self::spawn_finished,
        ) {
            Ok(_ticket) => {
                self.pending = true;
                log::info("Signal specimen request accepted");
                Ok(())
            }
            Err(error) => {
                log::error("Signal specimen request rejected before queueing");
                Err(error)
            }
        }
    }

    fn spawn_finished(&mut self, _ctx: &Ctx, result: SpawnResult) -> Result<()> {
        self.pending = false;
        match result.status() {
            SpawnStatus::Ready if !result.entity.is_null() => {
                self.specimen = Some(Entity::from_wire(result.entity));
                log::info("Signal specimen is ready");
            }
            SpawnStatus::Failed => {
                self.specimen = None;
                log::warn("Signal specimen failed during apply or initialization");
            }
            _ => {
                self.specimen = None;
                log::warn("Signal specimen callback returned an unexpected state");
            }
        }
        Ok(())
    }

    fn on_update(&mut self, ctx: &Ctx, _dt: f32) -> Result<()> {
        if self.pending { return Ok(()); }
        let Some(specimen) = self.specimen else { return Ok(()); };
        match specimen.visible() {
            Ok(true) => {
                ctx.highlight().outline_with(specimen, Vec4::new(0.25, 1.0, 0.7, 1.0), 4.0)?;
            }
            Ok(false) => {}
            Err(_) => {
                self.specimen = None;
                log::warn("Signal specimen is no longer available");
            }
        }
        Ok(())
    }

    fn on_stop(&mut self, _ctx: &Ctx) -> Result<()> {
        self.pending = false;
        self.specimen = None;
        Ok(())
    }
}
