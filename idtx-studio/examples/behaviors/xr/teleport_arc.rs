/// Teleport-arc preview (scene-interaction SDK example).
///
/// Each frame, casts a ballistic arc from the right-hand aim pose and outlines
/// the prim the arc lands on — the classic VR teleport-pointer cue.
/// Demonstrates `ctx.scene().arc_cast(..)`: the sampled arc points are written
/// into a caller-owned, allocation-free buffer (`points[..arc.point_count]`)
/// ready to feed a line renderer, and the first hit is returned for the
/// landing highlight.
///
/// `RayOpts::require_collidable()` restricts the landing test to prims flagged
/// with the collision API schema (`idtx:collision:enabled`), so the arc only
/// snaps to intended teleport surfaces.
use idtx_behavior_sdk::prelude::*;

#[derive(Default)]
pub struct TeleportArc;

#[behavior(kind = "runtime")]
impl TeleportArc {
    fn on_update(&mut self, ctx: &Ctx, _dt: f32) -> Result<()> {
        let Some(aim) = ctx.xr().get_pose(XrSpace::RightAim) else {
            return Ok(());
        };
        let launch = ArcLaunch {
            origin: aim.translation,
            dir: aim.rotation * Vec3::NEG_Z,
            speed: 8.0,
            gravity: 9.81,
            max_time: 3.0,
        };

        // Up to 32 sampled arc points (density). The host fills the buffer; no
        // allocation on the guest.
        let mut points = [Vec3::ZERO; 32];
        let arc = ctx
            .scene()
            .arc_cast(&launch, &RayOpts::new().require_collidable(), &mut points);

        if arc.hit {
            ctx.highlight()
                .highlight(arc.entity, Vec4::new(0.2, 0.8, 1.0, 1.0))?;
        }
        // `points[..arc.point_count]` is the polyline to render the arc.
        let _ = &points[..arc.point_count];
        Ok(())
    }
}
