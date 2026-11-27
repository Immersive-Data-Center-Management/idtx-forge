use idtx_behavior_sdk::prelude::*;

#[derive(Deserialize)]
#[serde(crate = "idtx_behavior_sdk::serde")]
struct Completion {
    clip: ContextString<256>,
    run_id: u64,
}

#[derive(Deserialize)]
#[serde(crate = "idtx_behavior_sdk::serde")]
struct CommandResult {
    applied: bool,
    error: ContextString<2048>,
}

#[derive(Default)]
pub struct ExhibitPlayer;

#[behavior(kind = "runtime")]
impl ExhibitPlayer {
    fn on_start(&mut self, ctx: &Ctx) -> Result<()> {
        ctx.animations().clip("idle").play()
    }

    fn on_event(&mut self, ctx: &Ctx) -> Result<()> {
        let mut name = [0u8; 256];
        let length = ctx.event().read_name(&mut name);
        let name = name.get(..length).ok_or(ErrorCode::InvalidArgument)?;
        match name {
            b"exhibit.react" => {
                // Stop then Play explicitly restarts a running/completed action.
                ctx.animations().clip("reaction").stop()?;
                ctx.animations().clip("reaction").play()?;
            }
            b"exhibit.pause" => {
                let mut scratch = [0u8; 8192];
                let reaction = ctx.animations().clip("reaction").status(&mut scratch)?;
                if reaction.independent {
                    ctx.animations().clip("reaction").pause()?;
                } else {
                    ctx.animations().clip("idle").pause()?;
                }
            }
            b"exhibit.release" => {
                // Restore the containing USD timeline, rather than holding a pose.
                ctx.animations().clip("reaction").release()?;
                ctx.animations().clip("idle").release()?;
            }
            b"animation.completed" => {
                let mut payload = [0u8; 2048];
                let length = ctx.event().read_payload(&mut payload);
                let bytes = payload.get(..length).ok_or(ErrorCode::InvalidArgument)?;
                let notice: Completion = idtx_behavior_sdk::context::decode(bytes)?;
                if notice.clip.as_str() == "reaction" {
                    let mut scratch = [0u8; 8192];
                    let status = ctx.animations().clip("reaction").status(&mut scratch)?;
                    // Discard a delayed notification from an earlier playback run.
                    if status.run_id == notice.run_id && status.state.as_str() == "completed" {
                        ctx.animations().clip("idle").play()?;
                    }
                }
            }
            b"animation.command" => {
                let mut payload = [0u8; 4096];
                let length = ctx.event().read_payload(&mut payload);
                let bytes = payload.get(..length).ok_or(ErrorCode::InvalidArgument)?;
                let result: CommandResult = idtx_behavior_sdk::context::decode(bytes)?;
                if !result.applied { log::warn(result.error.as_str()); }
            }
            b"animation.failed" => log::warn("Exhibit animation binding failed"),
            _ => {}
        }
        Ok(())
    }

    fn on_stop(&mut self, ctx: &Ctx) -> Result<()> {
        ctx.animations().clip("reaction").release()?;
        ctx.animations().clip("idle").release()
    }
}
