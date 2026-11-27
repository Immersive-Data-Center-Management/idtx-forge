/// Desktop gaze-highlight (scene-interaction SDK example).
///
/// The desktop analogue of `gaze_highlight`: instead of an XR controller aim
/// pose, it raycasts from the **player / desktop camera** — `ctx.camera()`
/// returns the active view camera as a ray (`cam.ray`, world position + look
/// direction, stage space) plus its full world transform. Each frame it
/// highlights the first prim under the view centre, a crosshair-style hover cue
/// for mouse/keyboard play.
///
/// Demonstrates the three highlight styles the renderer supports:
///   * `outline(..)` — a silhouette outline in the given colour (used here),
///   * `tint(..)`    — a flat surface tint; the colour's alpha is the strength,
///   * `highlight(..)` — the coloured wireframe-box fallback cue.
///
/// `ctx.camera()` is ambient frame state (no capability); the raycast needs
/// `scene_query` and the highlight needs `overlay`.
use idtx_behavior_sdk::prelude::*;

#[derive(Default)]
pub struct CameraGazeHighlight;

#[behavior(kind = "runtime")]
impl CameraGazeHighlight {
    fn on_update(&mut self, ctx: &Ctx, _dt: f32) -> Result<()> {
        let Some(cam) = ctx.camera() else {
            return Ok(());
        };
        if let Some(hit) = ctx
            .scene()
            .raycast(cam.ray.origin, cam.ray.forward, &RayOpts::new().interaction_only().max_distance(50.0))
        {
            // Resolve the hit gprim up to its whole-asset collision body so the
            // crosshair cue covers the whole asset; fall back to the exact prim.
            let target = ctx.scene().collision_body(hit.entity).unwrap_or(hit.entity);
            // Green silhouette outline (6 px band) on the asset under the
            // crosshair. Use `.outline(entity, color)` for the default width.
            ctx.highlight()
                .outline_with(target, Vec4::new(0.1, 1.0, 0.4, 1.0), 6.0)?;
            // Plus a soft green surface tint (alpha = 25% strength). Swap for
            // `.highlight(..)` to get the wireframe-box fallback instead.
            ctx.highlight()
                .tint(target, Vec4::new(0.1, 1.0, 0.4, 0.25))?;
        }
        Ok(())
    }
}
