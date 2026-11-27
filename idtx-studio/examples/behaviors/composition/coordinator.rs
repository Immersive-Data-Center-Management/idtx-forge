// Project state machine. Capabilities: events. Event vocabulary is authored.
use idtx_behavior_sdk::prelude::*;

#[derive(Deserialize)]
#[serde(crate = "idtx_behavior_sdk::serde")]
struct Config<'a> {
    start: &'a str,
    targeted: &'a str,
    reset: &'a str,
}

#[derive(Default)]
pub struct Coordinator {
    running: bool,
}

#[behavior(kind = "runtime")]
impl Coordinator {
    fn on_event(&mut self, ctx: &Ctx) -> Result<()> {
        let mut scratch = [0; 8192];
        let config: Config<'_> = ctx.context(&mut scratch)?;
        let mut name = [0; 256];
        let n = ctx.event().read_name(&mut name);
        let name = name.get(..n).ok_or(ErrorCode::InvalidArgument)?;
        if name == config.start.as_bytes() && !self.running {
            self.running = true;
            log::info("Hunt started"); // exactly one idle -> running transition
        } else if name == config.targeted.as_bytes() && self.running {
            log::info("Hunt target selected");
        } else if name == config.reset.as_bytes() {
            self.running = false;
        }
        Ok(())
    }
}
