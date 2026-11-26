# Optional spawn extension: complete files

These files are included in every reconstructed copy but are disabled in the base project. Follow [the spawn exercise](../tutorial/05-spawning.md) to add the Lab sublayer and prototype registration. Both are needed for the exercise to run.

The prototype is local, has an explicit root prim and uses the stable ID `specimen`. The source requests a reference once and waits for terminal readiness before outlining the result.

## `extensions/specimen.usda`

<!-- example-file: extensions/specimen.usda -->

```usda
#usda 1.0
(
    defaultPrim = "Specimen"
    metersPerUnit = 1
    upAxis = "Y"
    customLayerData = {
        dictionary idtx = {
            string displayName = "Signal Specimen"
            string schemaVersion = "1"
            token stageKind = "asset"
        }
    }
)

# Registration prepares this asset; it is not instantiated until spawn.
def Xform "Specimen"
{
    def Sphere "Shape" (prepend apiSchemas = ["PhysicsCollisionAPI", "IdtxCollisionAPI"])
    {
        double radius = 0.25
        color3f[] primvars:displayColor = [(0.25, 1, 0.7)]
        uniform token[] idtx:collision:channel = ["Interaction"]
    }
}
```

## `extensions/spawn-demo.usda`

<!-- example-file: extensions/spawn-demo.usda -->

```usda
#usda 1.0
(
    defaultPrim = "World"
    metersPerUnit = 1
    upAxis = "Y"
)

# Opt in by sublayering this file into levels/lab.usda.
over "World"
{
    def Xform "Spawner" (prepend apiSchemas = ["IdtxBehaviorAPI"])
    {
        token idtx:behavior:kind = "runtime"
        string idtx:behavior:name = "signal_spawn_demo"
        string idtx:behavior:version = "0.1.0"
        string idtx:behavior:uuid = "signal-room-spawn-demo-v1"
        asset idtx:behavior:source = @spawn_demo.rs@
        uniform token[] idtx:behavior:capabilities = ["stage_edit", "scene_query", "overlay"]
    }
}
```

## `extensions/spawn_demo.rs`

<!-- example-file: extensions/spawn_demo.rs -->

```rust,ignore
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
```
