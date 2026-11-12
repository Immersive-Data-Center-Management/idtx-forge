//! Execution limits: fuel metering, epoch interruption, and a linear-memory cap.
//!
//! Every hook is bounded by a **per-hook budget** enforced two ways that
//! back each other up:
//!
//! * **Fuel metering** (deterministic, phase-independent): the store is
//!   topped up to at most [`Limits::hook_fuel`] units before every hook, so a
//!   runaway guest traps after a reproducible amount of work regardless
//!   of wall-clock scheduling. This is the primary guard.
//! * **Fine-grained epoch interruption** (cooperative wall-clock guard):
//!   a background ticker advances the engine epoch at a *fine*
//!   granularity ([`EPOCH_TICK`]) and the dispatcher sets the deadline
//!   to `ceil(hook_budget / EPOCH_TICK)` ticks per hook. Because the
//!   deadline is expressed in many small ticks rather than a single
//!   coarse one, a hook reliably gets ~`hook_budget` of wall-clock time
//!   no matter where it starts relative to the ticker's phase. Rounding/phase
//!   error is at most one [`EPOCH_TICK`]; OS scheduling can delay the ticker.
//!   Epochs interrupt guest code at safepoints, not a blocking host import.
//!
//! Module instantiation (including its start function) and the ABI-version call
//! share a separate **startup budget**, armed once before instantiation. Module
//! compilation and import linking happen before that budget is armed.
//! Hook allowances are additionally clamped to one [`WorkLimits`] dispatch
//! budget shared by updates, callbacks, and event fan-out. Queue credits remain
//! occupied until the corresponding work is drained, delivered, or cancelled.
//!
//! Linear memory is capped per instance via [`wasmtime::StoreLimits`].

use std::time::Duration;

/// Default per-hook wall-clock budget in the conductor (authoring).
pub const DEFAULT_CONDUCTOR_BUDGET: Duration = Duration::from_millis(5);

/// Default per-hook wall-clock budget in the standalone runtime.
pub const DEFAULT_RUNTIME_BUDGET: Duration = Duration::from_millis(20);

/// Combined initialization + ABI-probe budget for both host profiles. Startup
/// gets more scheduling headroom than a frame hook, while remaining bounded.
pub const DEFAULT_STARTUP_BUDGET: Duration = Duration::from_millis(100);

/// Combined deterministic fuel budget for initialization + the ABI probe.
pub const DEFAULT_STARTUP_FUEL: u64 = 50_000_000;

/// Default linear-memory cap per instance (16 MiB).
pub const DEFAULT_MEMORY_CAP: usize = 16 * 1024 * 1024;

/// Fine granularity at which the background ticker advances the engine
/// epoch. The per-hook deadline is expressed as a whole number of these
/// ticks. This bounds rounding/phase error, not OS scheduling delay.
pub const EPOCH_TICK: Duration = Duration::from_millis(1);

/// Default deterministic fuel budget per hook.
///
/// Fuel is consumed per wasm operation; this cap is generous enough for
/// a normal per-frame hook (a few hundred thousand ops) yet still trips
/// a genuine infinite loop quickly. Tuneable per profile via
/// [`Limits::hook_fuel`].
pub const DEFAULT_HOOK_FUEL: u64 = 50_000_000;

/// Deferred-work and aggregate dispatch limits for one behavior host.
/// Queue byte accounting includes inline values and owned buffer capacities.
#[derive(Debug, Clone, Copy)]
pub struct WorkLimits {
    /// Combined pending operations across all channels of one instance.
    pub queue_items: usize,
    /// Combined retained/reserved bytes across an instance's channels.
    pub queue_bytes: usize,
    /// Pending operations across the whole host, including widget state.
    pub host_queue_items: usize,
    /// Retained/reserved bytes across the whole host.
    pub host_queue_bytes: usize,
    /// Maximum variable-length guest argument / deferred payload size.
    pub argument_bytes: usize,
    /// Shared widget registration count (one entry per handler owner).
    pub widget_handlers: usize,
    /// Shared widget registration byte cap.
    pub widget_handler_bytes: usize,
    /// Host ceiling on an asynchronous HTTP response, also clamped by policy.
    pub http_response_bytes: usize,
    /// Maximum live instances; bounds frame bookkeeping and broadcast targets.
    pub instances: usize,
    /// Guest hook invocations per dispatch batch (frame, interaction, lifecycle).
    pub dispatch_hooks: usize,
    /// Export lookups / delivery recipient visits per dispatch batch.
    pub dispatch_visits: usize,
    /// Combined guest fuel across all hooks in a dispatch batch.
    pub dispatch_fuel: u64,
    /// Combined cooperative wall-clock budget of a dispatch batch.
    pub dispatch_budget: Duration,
}

impl Default for WorkLimits {
    fn default() -> Self {
        Self {
            queue_items: 256,
            queue_bytes: 2 * 1024 * 1024,
            host_queue_items: 4096,
            host_queue_bytes: 16 * 1024 * 1024,
            argument_bytes: 64 * 1024,
            widget_handlers: 512,
            widget_handler_bytes: 256 * 1024,
            http_response_bytes: 1024 * 1024,
            instances: 256,
            dispatch_hooks: 1024,
            dispatch_visits: 8192,
            dispatch_fuel: 200_000_000,
            dispatch_budget: DEFAULT_RUNTIME_BUDGET,
        }
    }
}

/// Per-instance startup, hook, and memory limits.
///
/// See the module docs for the fuel + fine-grained epoch guards and the
/// distinction between the combined startup budget and per-hook budgets.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Deferred-work and aggregate dispatch limits.
    pub work: WorkLimits,
    /// Combined startup wall-clock budget (enforced cooperatively via epochs).
    pub startup_budget: Duration,
    /// Combined startup fuel; not replenished between initialization and ABI.
    pub startup_fuel: u64,
    /// Per-hook wall-clock budget (enforced via fine-grained epochs).
    pub hook_budget: Duration,
    /// Per-hook deterministic fuel budget (enforced via fuel metering).
    pub hook_fuel: u64,
    /// Linear-memory cap in bytes.
    pub memory_cap: usize,
}

impl Limits {
    /// Conductor (authoring) defaults.
    #[must_use]
    pub fn conductor() -> Self {
        Self {
            work: WorkLimits {
                dispatch_budget: DEFAULT_CONDUCTOR_BUDGET,
                ..WorkLimits::default()
            },
            startup_budget: DEFAULT_STARTUP_BUDGET,
            startup_fuel: DEFAULT_STARTUP_FUEL,
            hook_budget: DEFAULT_CONDUCTOR_BUDGET,
            hook_fuel: DEFAULT_HOOK_FUEL,
            memory_cap: DEFAULT_MEMORY_CAP,
        }
    }

    /// Standalone-runtime defaults.
    #[must_use]
    pub fn runtime() -> Self {
        Self {
            work: WorkLimits::default(),
            startup_budget: DEFAULT_STARTUP_BUDGET,
            startup_fuel: DEFAULT_STARTUP_FUEL,
            hook_budget: DEFAULT_RUNTIME_BUDGET,
            hook_fuel: DEFAULT_HOOK_FUEL,
            memory_cap: DEFAULT_MEMORY_CAP,
        }
    }

    /// Number of [`EPOCH_TICK`]s that make up one hook budget, rounded
    /// up and clamped to at least 1. This is the value the dispatcher
    /// passes to `store.set_epoch_deadline`. The quantization error is at most
    /// one tick; interruption is cooperative and subject to ticker scheduling.
    #[must_use]
    pub fn epoch_deadline_ticks(&self) -> u64 {
        epoch_ticks(self.hook_budget)
    }

    /// Relative epoch deadline for the combined startup budget, with the same
    /// rounding and minimum tick as [`Self::epoch_deadline_ticks`].
    #[must_use]
    pub fn startup_epoch_deadline_ticks(&self) -> u64 {
        epoch_ticks(self.startup_budget)
    }
}

pub(crate) fn epoch_ticks(budget: Duration) -> u64 {
    let tick = EPOCH_TICK.as_nanos().max(1);
    // Wasmtime adds this relative deadline to the current epoch. Leave headroom
    // for that addition even if an embedder supplies an enormous duration.
    u64::try_from(budget.as_nanos().div_ceil(tick))
        .unwrap_or(u64::MAX / 2)
        .clamp(1, u64::MAX / 2)
}

impl Default for Limits {
    fn default() -> Self {
        Self::conductor()
    }
}

/// Spawn a background thread that advances `engine`'s epoch every
/// [`EPOCH_TICK`], returning a handle that stops the ticker on drop.
///
/// The dispatcher sets the deadline from the smaller of the hook allowance and
/// remaining batch time, so guest code that overruns traps with `Trap::Interrupt`. Running
/// the ticker at a fine granularity (rather than at the budget period)
/// means the deadline is phase-independent: a hook gets ~`hook_budget`
/// of wall-clock time no matter when it starts relative to a tick.
#[must_use]
pub fn spawn_epoch_ticker(engine: wasmtime::Engine) -> EpochTicker {
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_clone = stop.clone();
    let handle = std::thread::spawn(move || {
        while !stop_clone.load(std::sync::atomic::Ordering::Relaxed) {
            std::thread::sleep(EPOCH_TICK);
            engine.increment_epoch();
        }
    });
    EpochTicker {
        stop,
        handle: Some(handle),
    }
}

/// Stops the background epoch ticker when dropped.
#[derive(Debug)]
pub struct EpochTicker {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Drop for EpochTicker {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_deadline_ticks_is_budget_over_tick_rounded_up() {
        // 5 ms budget / 1 ms tick = 5 ticks.
        let conductor = Limits::conductor();
        assert_eq!(conductor.hook_budget, DEFAULT_CONDUCTOR_BUDGET);
        assert_eq!(conductor.epoch_deadline_ticks(), 5);

        // 20 ms budget / 1 ms tick = 20 ticks.
        let runtime = Limits::runtime();
        assert_eq!(runtime.epoch_deadline_ticks(), 20);
    }

    #[test]
    fn epoch_deadline_ticks_rounds_up_and_floors_at_one() {
        // A sub-tick budget must still yield at least one tick, never
        // zero (a zero deadline would trap immediately).
        let tiny = Limits {
            hook_budget: Duration::from_micros(1),
            ..Limits::conductor()
        };
        assert_eq!(tiny.epoch_deadline_ticks(), 1);

        // 1.5 ms rounds up to 2 ticks.
        let one_and_half = Limits {
            hook_budget: Duration::from_micros(1_500),
            ..Limits::conductor()
        };
        assert_eq!(one_and_half.epoch_deadline_ticks(), 2);
    }

    #[test]
    fn startup_defaults_are_explicit_in_both_profiles() {
        for limits in [Limits::conductor(), Limits::runtime()] {
            assert_eq!(limits.startup_budget, Duration::from_millis(100));
            assert_eq!(limits.startup_fuel, 50_000_000);
            assert_eq!(limits.startup_epoch_deadline_ticks(), 100);
        }
    }

    #[test]
    fn epoch_deadlines_handle_zero_fractional_and_extreme_budgets() {
        for (budget, ticks) in [
            (Duration::ZERO, 1),
            (Duration::from_micros(1), 1),
            (Duration::from_micros(1_500), 2),
            (Duration::MAX, u64::MAX / 2),
        ] {
            let limits = Limits {
                startup_budget: budget,
                hook_budget: budget,
                ..Limits::conductor()
            };
            assert_eq!(limits.startup_epoch_deadline_ticks(), ticks);
            assert_eq!(limits.epoch_deadline_ticks(), ticks);
        }
    }
}
