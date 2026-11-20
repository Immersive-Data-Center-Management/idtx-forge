//! Hydration performance instrumentation — all gated behind the `metrics`
//! feature in one place.
//!
//! [`HydrationMetrics`] is the public counter snapshot. [`MetricsSink`] is the
//! `pub(crate)` accumulator the [`SceneHydrator`](crate::SceneHydrator) writes
//! through: its recording methods compile to **nothing** unless the `metrics`
//! feature is enabled, so the hot hydration loops call them unconditionally and
//! the compiler elides them (and the values feeding them) in release builds.
//!
//! This keeps every `#[cfg(feature = "metrics")]` confined to this module
//! instead of scattering it line-by-line through the projection hot path.

/// Performance counters accumulated over the session lifetime.
/// Only meaningful when compiled with `--features metrics`; otherwise every
/// field stays at its [`Default`] (zero) value.
#[derive(Debug, Clone, Default)]
pub struct HydrationMetrics {
    pub topology_calls: u64,
    pub topology_spawns: u64,
    pub topology_despawns: u64,
    pub viewport_calls: u64,
    pub viewport_component_upserts: u64,
    /// Cumulative wall time in microseconds (requires `metrics` feature).
    pub topology_us: u64,
    pub viewport_us: u64,
}

/// A started timer. Captures an [`Instant`](std::time::Instant) only when the
/// `metrics` feature is on; otherwise it is a zero-sized no-op whose
/// [`elapsed_us`](ScopeTimer::elapsed_us) is never called.
#[derive(Debug)]
pub(crate) struct ScopeTimer {
    #[cfg(feature = "metrics")]
    start: std::time::Instant,
}

impl ScopeTimer {
    #[inline]
    fn start() -> Self {
        Self {
            #[cfg(feature = "metrics")]
            start: std::time::Instant::now(),
        }
    }

    #[cfg(feature = "metrics")]
    #[inline]
    fn elapsed_us(&self) -> u64 {
        u64::try_from(self.start.elapsed().as_micros()).unwrap_or(u64::MAX)
    }
}

/// Accumulates [`HydrationMetrics`] for the hydrator. Every recording method has
/// a `#[cfg(feature = "metrics")]`-gated body, so callers invoke them
/// unconditionally; with the feature off they are empty and the arguments they
/// are passed become dead stores the optimizer removes.
#[derive(Debug, Default)]
pub(crate) struct MetricsSink {
    inner: HydrationMetrics,
}

impl MetricsSink {
    /// Borrow the accumulated counters (all zero unless the feature is on).
    #[inline]
    #[must_use]
    pub(crate) fn get(&self) -> &HydrationMetrics {
        &self.inner
    }

    /// Start a timing scope. No-op (ZST) unless the `metrics` feature is on.
    #[inline]
    #[must_use]
    pub(crate) fn scope() -> ScopeTimer {
        ScopeTimer::start()
    }

    /// Record `n` standalone despawns (e.g. [`despawn_subtree`]).
    ///
    /// [`despawn_subtree`]: crate::SceneHydrator::despawn_subtree
    #[inline]
    #[cfg_attr(not(feature = "metrics"), allow(unused_variables), allow(clippy::unused_self))]
    pub(crate) fn note_despawns(&mut self, n: u64) {
        #[cfg(feature = "metrics")]
        {
            self.inner.topology_despawns += n;
        }
    }

    /// Record one `apply_topology_delta` call: its spawn/despawn tallies and
    /// elapsed time.
    #[inline]
    #[cfg_attr(not(feature = "metrics"), allow(unused_variables), allow(clippy::unused_self))]
    pub(crate) fn note_topology(&mut self, timer: &ScopeTimer, spawns: u64, despawns: u64) {
        #[cfg(feature = "metrics")]
        {
            self.inner.topology_calls += 1;
            self.inner.topology_spawns += spawns;
            self.inner.topology_despawns += despawns;
            self.inner.topology_us += timer.elapsed_us();
        }
    }

    /// Record one `apply_viewport_projection` call: its component-upsert tally
    /// and elapsed time.
    #[inline]
    #[cfg_attr(not(feature = "metrics"), allow(unused_variables), allow(clippy::unused_self))]
    pub(crate) fn note_viewport(&mut self, timer: &ScopeTimer, upserts: u64) {
        #[cfg(feature = "metrics")]
        {
            self.inner.viewport_calls += 1;
            self.inner.viewport_component_upserts += upserts;
            self.inner.viewport_us += timer.elapsed_us();
        }
    }
}
