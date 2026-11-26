mod easing;

pub fn advance(angle: f32, dt: f32) -> f32 {
    angle + easing::speed(dt)
}
