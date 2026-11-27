// Doc-example single-file behavior: a spinning cube.
//
// This is the canonical single-file behavior authors write. It compiles
// to `wasm32-unknown-unknown` through the `idtx-behavior-build` skeleton
// and exports the ABI symbols the host validates. It is also the build
// pipeline's exit-criterion fixture (`build_behavior` compiles it to
// `.wasm`).
//
// The generated no_std wrapper loads this file with #[path] as a module body.

use idtx_behavior_sdk::prelude::*;

/// Rotates its prim around Y at a fixed rate.
#[derive(Default)]
pub struct SpinningCube {
    /// Accumulated angle in radians.
    angle: f32,
}

#[behavior(kind = "runtime")]
impl SpinningCube {
    fn on_update(&mut self, ctx: &Ctx, dt: f32) -> Result<()> {
        // 90 deg/s.
        self.angle += 90.0_f32.to_radians() * dt;
        let e = ctx.entity();
        let mut x = e.get_xform()?;
        x.rotation = Quat::from_rotation_y(self.angle);
        e.set_xform(&x)?;
        Ok(())
    }
}
