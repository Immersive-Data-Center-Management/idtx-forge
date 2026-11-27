pub fn speed(dt: f32) -> f32 {
    dt.clamp(0.0, 0.1)
}
