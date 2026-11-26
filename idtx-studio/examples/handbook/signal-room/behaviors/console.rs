use idtx_behavior_sdk::prelude::*;

/// Owned configuration: it outlives the scratch buffer used to decode it.
#[derive(Deserialize)]
#[serde(crate = "idtx_behavior_sdk::serde")]
pub struct ConsoleConfig {
    pub speed_radians: f32,
    pub target_level: ContextString<24>,
}

#[derive(Default)]
pub struct SignalConsole {
    beacon: Option<Entity>,
    base: Transform,
    angle: f32,
    speed: f32,
    running: bool,
    target: ContextString<24>,
}

/// Clamp long frames so a debugger pause does not cause a large visual jump.
pub fn advance_angle(angle: f32, speed: f32, dt: f32) -> f32 {
    (angle + speed * dt.max(0.0).min(0.1)) % core::f32::consts::TAU
}

#[behavior(kind = "runtime")]
impl SignalConsole {
    fn on_start(&mut self, ctx: &Ctx) -> Result<()> {
        let mut scratch = [0u8; 256];
        let config: ConsoleConfig = ctx.context(&mut scratch)?;
        if !config.speed_radians.is_finite() || config.speed_radians < 0.0
            || config.target_level.is_empty() {
            return Err(ErrorCode::InvalidArgument.into());
        }
        self.speed = config.speed_radians;
        self.target = config.target_level;
        // Named sibling lookup survives level re-rooting during composition.
        self.beacon = ctx.scene().child(ctx.entity().parent()?, "Beacon")?;
        let beacon = self.beacon.ok_or(ErrorCode::NotFound)?;
        // Hydrated USD prims have world transforms. Derive the local pose
        // instead of assuming a guest-written local ECS transform exists.
        let parent_world = beacon.parent()?.world_matrix()?;
        self.base = Transform::from_matrix(parent_world.inverse() * beacon.world_matrix()?);
        self.running = true;
        let mut state = [0u8; 1];
        if ctx.level().read_state("beacon.running", &mut state)? == Some(1) {
            self.running = state[0] != 0;
        }
        // The macro generates its callback registry on the WASM target.
        #[cfg(target_arch = "wasm32")]
        {
            ctx.widget().on_click("Toggle", Self::toggle_clicked)?;
            ctx.widget().on_click("Travel", Self::travel_clicked)?;
        }
        self.refresh_readout(ctx)
    }

    fn on_update(&mut self, ctx: &Ctx, dt: f32) -> Result<()> {
        if self.running {
            self.angle = advance_angle(self.angle, self.speed, dt);
            let mut pose = self.base;
            pose.rotation = self.base.rotation * Quat::from_rotation_y(self.angle);
            if let Some(beacon) = self.beacon {
                // Deferred session-layer pose: the source files stay unchanged.
                ctx.stage_edit().set_transform(beacon, &pose)?;
            }
        }
        Ok(())
    }

    fn on_stop(&mut self, ctx: &Ctx) -> Result<()> {
        if let Some(beacon) = self.beacon { ctx.stage_edit().set_transform(beacon, &self.base)?; }
        self.angle = 0.0;
        Ok(())
    }

    fn toggle_clicked(&mut self, ctx: &Ctx, _click: &WidgetClick) -> Result<()> {
        self.running = !self.running;
        ctx.level().set_state("beacon.running", &[u8::from(self.running)])?;
        self.refresh_readout(ctx)
    }

    fn travel_clicked(&mut self, ctx: &Ctx, _click: &WidgetClick) -> Result<()> {
        ctx.level().set_state("beacon.running", &[u8::from(self.running)])?;
        ctx.level().goto(self.target.as_str())
    }

    fn refresh_readout(&self, ctx: &Ctx) -> Result<()> {
        ctx.stage_edit().set_widget_value("State", if self.running { "RUNNING" } else { "PAUSED" })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotation_matches_across_frame_rates() {
        let mut at_10_hz = 0.0;
        let mut at_40_hz = 0.0;
        for _ in 0..10 { at_10_hz = advance_angle(at_10_hz, 1.5, 0.1); }
        for _ in 0..40 { at_40_hz = advance_angle(at_40_hz, 1.5, 0.025); }
        assert!((at_10_hz - at_40_hz).abs() < 0.00001);
    }

    #[test]
    fn debugger_pause_is_bounded_and_negative_dt_is_ignored() {
        assert!((advance_angle(0.0, 2.0, 30.0) - 0.2).abs() < 0.00001);
        assert_eq!(advance_angle(1.0, 2.0, -1.0), 1.0);
    }

    #[test]
    fn typed_context_rejects_wrong_types_and_trailing_data() {
        let config: ConsoleConfig = idtx_behavior_sdk::context::decode(
            br#"{"speed_radians":1.5,"target_level":"lab"}"#
        ).unwrap();
        assert_eq!(config.target_level.as_str(), "lab");
        assert!(idtx_behavior_sdk::context::decode::<ConsoleConfig>(
            br#"{"speed_radians":"fast","target_level":"lab"}"#
        ).is_err());
        assert!(idtx_behavior_sdk::context::decode::<ConsoleConfig>(
            br#"{"speed_radians":1.5,"target_level":"lab"} false"#
        ).is_err());
    }
}

