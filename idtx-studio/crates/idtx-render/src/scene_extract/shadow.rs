//! Shadow tiers, per-frame shadow settings, and the importance-ranked shadow
//! atlas allocator.

use glam::Mat4;

use crate::convert::u32_sat;

use super::light::RenderLightDesc;

/// Which shadow-filtering algorithm the renderer runs this frame. Ordered by
/// cost so a target-hardware cap can clamp downward (`Soft → Smooth → Hard →
/// Off`) — the XR targets (Quest 2/3/Pro) may forbid the expensive tiers. See
/// the shadow plan §"target architecture". The derived `Ord` follows
/// declaration order (`Off < Hard < Smooth < Soft`), so `authored_tier.min(cap)`
/// clamps to the cheaper tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum ShadowTier {
    /// No shadow pass. The PBR shader binds a 1×1 dummy depth and short-circuits
    /// to visibility = 1.0. The default, and the forced value when the stage
    /// flag is off, the product forbids shadows, or the target caps it off.
    #[default]
    Off,
    /// True hard shadow: a **single** comparison tap (one hardware 2×2-bilinear
    /// `textureSampleCompareLevel`). A crisp ~1-texel anti-aliased edge with no
    /// 3×3 blur and none of `Soft`'s variable-radius temporal jitter — 1 tap vs
    /// `Smooth`'s 9, so the cheapest lit tier. The tier the USD
    /// `idtx:shadowMode="hard"` authoring token resolves to.
    Hard,
    /// Filtered "smooth" shadow with a fixed 3×3 PCF kernel (9 comparison taps):
    /// a slightly softened contact edge. The tier the USD `smooth` mode resolves
    /// to (formerly the `hard` mode's target before true-hard landed).
    Smooth,
    /// Percentage-closer soft shadows: blocker search → similar-triangles
    /// penumbra estimate → variable-radius PCF. The high-quality desktop tier;
    /// the USD `soft` mode.
    Soft,
}

/// Per-frame shadow configuration, resolved by the caller from three inputs:
/// the `idtx:softShadows` stage flag, the product `ProductProfile` shadow
/// capability, and the target-hardware tier cap. Threaded into
/// [`super::RenderWorldSnapshot::extract_with_shadow`] and stored on the snapshot; the
/// renderer reads only [`RenderShadowSettings::tier`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RenderShadowSettings {
    /// Resolved tier for this frame. `Off` disables the whole feature.
    pub tier: ShadowTier,
}

impl RenderShadowSettings {
    /// Shadows disabled — the default and the value used by the shadows-agnostic
    /// [`super::RenderWorldSnapshot::extract`] wrapper.
    pub const OFF: Self = Self {
        tier: ShadowTier::Off,
    };

    /// Construct from a resolved tier.
    #[must_use]
    pub const fn new(tier: ShadowTier) -> Self {
        Self { tier }
    }

    /// `true` when a shadow pass should run this frame.
    #[must_use]
    pub const fn enabled(&self) -> bool {
        !matches!(self.tier, ShadowTier::Off)
    }
}

/// Maximum number of simultaneous shadow-casting lights = shadow-atlas layers.
/// Decoupled from `MAX_LIGHTS` (16): a `[Mat4; 4]` + `[vec4; 4]` uniform stays
/// small, and the depth pre-pass cost is bounded regardless of light count. See
/// plan §4 / §7.
pub const SHADOW_BUDGET: usize = 4;

/// One resolved shadow-atlas layer: a caster light's light-space matrix plus the
/// data the depth pre-pass and the PCSS shader need. Produced by
/// [`ShadowAtlasAllocator::allocate`]; `slot` is the atlas array layer index.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShadowSlot {
    /// Atlas array layer in `[0, SHADOW_BUDGET)`.
    pub slot: u32,
    /// Index into [`super::RenderWorldSnapshot::lights`] of the source light.
    pub light_index: u32,
    /// Light-space view-projection (fit to the scene AABB).
    pub view_proj: Mat4,
    pub near: f32,
    pub far: f32,
    /// Penumbra scale (PCSS light size) copied from the source light.
    pub light_size: f32,
}

/// Importance-ranked allocator that assigns the scarce atlas layers to the most
/// important shadow-casting lights (plan §4 — "importance-ranked budget").
#[derive(Debug)]
pub struct ShadowAtlasAllocator;

impl ShadowAtlasAllocator {
    /// Assign atlas layers to the most important shadow-casting lights.
    ///
    /// Candidates are lights with a `shadow_view_proj` (Distant casters in v1).
    /// They are ranked by importance — brightness (`power`) first, then a
    /// deterministic `prim_path` tie-break so the ranking (and therefore each
    /// light's slot index) is a pure, stable function of the light set. That
    /// stability is what the Phase-6 `ShadowCache` relies on to keep atlas
    /// layers valid frame-to-frame.
    ///
    /// The top [`SHADOW_BUDGET`] candidates receive a distinct slot `[0, BUDGET)`
    /// in rank order and have their `shadow_slot` set. Overflow candidates keep
    /// `shadow_slot == None` and cast no shadow — a deterministic
    /// least-important-first drop. Returns the resolved slots in slot order.
    #[must_use]
    pub fn allocate(lights: &mut [RenderLightDesc]) -> Vec<ShadowSlot> {
        // Rank caster candidates by descending brightness; break ties on the
        // prim path so the result is deterministic (stable slot identity).
        let mut ranked: Vec<usize> = lights
            .iter()
            .enumerate()
            .filter(|(_, l)| l.shadow_view_proj.is_some())
            .map(|(i, _)| i)
            .collect();
        ranked.sort_by(|&a, &b| {
            lights[b]
                .power
                .partial_cmp(&lights[a].power)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| lights[a].prim_path.cmp(&lights[b].prim_path))
        });

        let mut slots = Vec::with_capacity(ranked.len().min(SHADOW_BUDGET));
        for (slot, &light_index) in ranked.iter().take(SHADOW_BUDGET).enumerate() {
            let l = &mut lights[light_index];
            l.shadow_slot = Some(u32_sat(slot));
            slots.push(ShadowSlot {
                slot: u32_sat(slot),
                light_index: u32_sat(light_index),
                view_proj: l
                    .shadow_view_proj
                    .expect("caster candidate always has a shadow_view_proj"),
                near: l.shadow_near,
                far: l.shadow_far,
                light_size: l.light_size,
            });
        }
        // Overflow candidates (rank ≥ SHADOW_BUDGET) keep shadow_slot == None.
        slots
    }
}
