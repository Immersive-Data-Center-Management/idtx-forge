// Attach to the actor wrapper. Capabilities: playback, events.
use idtx_behavior_sdk::prelude::*;

#[derive(Default)]
pub struct ReactionPlayer { caught: bool }

#[behavior(kind = "runtime")]
impl ReactionPlayer {
    fn on_start(&mut self, ctx: &Ctx) -> Result<()> {
        self.caught = false;
        ctx.animations().clip("idle").play()
    }

    fn on_event(&mut self, ctx: &Ctx) -> Result<()> {
        let mut name = [0; 256];
        let n = ctx.event().read_name(&mut name);
        match name.get(..n).ok_or(ErrorCode::InvalidArgument)? {
            b"React" if !self.caught => {
                ctx.animations().clip("reaction").stop()?;
                ctx.animations().clip("reaction").play()?;
            }
            b"Catch" => {
                self.caught = true;
                ctx.animations().clip("idle").pause()?;
                ctx.animations().clip("reaction").stop()?;
            }
            b"animation.completed" if !self.caught => {
                let mut scratch = [0; 8192];
                let status = ctx.animations().clip("reaction").status(&mut scratch)?;
                if status.state.as_str() == "completed" {
                    ctx.animations().clip("idle").play()?;
                }
            }
            _ => {}
        }
        Ok(())
    }
}
