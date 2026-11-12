//! Logging from a behavior.
//!
//! `log::info("hello")` etc. route to the always-linked host `idtx_log`
//! import on wasm; natively they are no-ops (author unit tests stay quiet).
//!
//! Levels mirror the common tracing levels as plain `u32`s:
//! `0 = error, 1 = warn, 2 = info, 3 = debug, 4 = trace`.

/// Log level wire values shared with the host.
pub mod level {
    pub const ERROR: u32 = 0;
    pub const WARN: u32 = 1;
    pub const INFO: u32 = 2;
    pub const DEBUG: u32 = 3;
    pub const TRACE: u32 = 4;
}

/// Emit a message at an explicit level.
pub fn log(level: u32, msg: &str) {
    #[cfg(target_arch = "wasm32")]
    {
        unsafe {
            crate::__rt::idtx_log(
                level,
                msg.as_ptr(),
                u32::try_from(msg.len()).unwrap_or(u32::MAX),
            );
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = (level, msg);
    }
}

/// Log at error level.
pub fn error(msg: &str) {
    log(level::ERROR, msg);
}

/// Log at warn level.
pub fn warn(msg: &str) {
    log(level::WARN, msg);
}

/// Log at info level.
pub fn info(msg: &str) {
    log(level::INFO, msg);
}

/// Log at debug level.
pub fn debug(msg: &str) {
    log(level::DEBUG, msg);
}

/// Log at trace level.
pub fn trace(msg: &str) {
    log(level::TRACE, msg);
}
