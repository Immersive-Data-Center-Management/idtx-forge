use winit::window::Icon;

/// Decode raw icon bytes (ICO or PNG) into a winit [`Icon`] for the
/// window title-bar / taskbar.
///
/// This is the *runtime* half of the icon story: the build script
/// embeds the `.ico` into the `.exe` resource table (drives the
/// Explorer file icon on Windows), while winit needs decoded RGBA
/// pixels to paint the live window's title-bar and taskbar entry — the
/// two mechanisms are independent, which is why a binary can have the
/// right Explorer icon yet a blank title-bar icon until this runs.
///
/// Returns `None` (falling back to the platform default) when no bytes
/// were supplied or decoding fails; a bad icon must never abort
/// startup, so failures are logged and swallowed.
pub fn decode_window_icon(bytes: Option<&'static [u8]>) -> Option<Icon> {
    let bytes = bytes?;
    // `image` auto-detects ICO vs PNG from the magic bytes. For a
    // multi-resolution `.ico` this decodes the largest contained image,
    // which winit then downscales per platform as needed.
    let decoded = match image::load_from_memory(bytes) {
        Ok(img) => img.into_rgba8(),
        Err(err) => {
            tracing::warn!(?err, "failed to decode window icon; using platform default");
            return None;
        }
    };
    let (w, h) = decoded.dimensions();
    match Icon::from_rgba(decoded.into_raw(), w, h) {
        Ok(icon) => Some(icon),
        Err(err) => {
            tracing::warn!(?err, "failed to build window icon; using platform default");
            None
        }
    }
}
