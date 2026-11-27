//! GPU-agnostic render material descriptor.
//!
//! [`RenderMaterialDesc`] is the renderer-facing representation of a resolved
//! USD material. It carries flat PBR scalar factors plus texture asset paths.
//! The render pipeline uploads textures and builds bind groups from this
//! descriptor.

use idtx_usd::{AlphaPolicy, MaterialRecord, MaterialSidedness, OpacityMode, SurfaceModel};

/// Renderer-facing PBR material parameters extracted from a [`MaterialRecord`].
///
/// Texture fields hold the **resolved** asset path (absolute on disk) so the
/// GPU texture loader can fetch them without another FFI call.
#[derive(Debug, Clone)]
pub struct RenderMaterialDesc {
    /// Source USD material prim path.
    pub material_path: String,
    pub model: SurfaceModel,
    // ── Scalar factors ────────────────────────────────────────────────────
    pub base_color: [f32; 3],
    pub metallic: f32,
    pub roughness: f32,
    /// Diffuse-wrap subsurface approximation (0 = off, 1 = full).
    pub subsurface: f32,
    pub emissive: [f32; 3],
    /// Beer-Lambert transmission tint (glass absorption colour). `[1,1,1]` = no
    /// tint. Non-white on a `Blend` material routes the draw to the colored-glass
    /// blend path so the background seen through the glass is tinted by this hue.
    pub transmission_color: [f32; 3],
    pub opacity: f32,
    pub opacity_threshold: f32,
    pub opacity_mode: OpacityMode,
    /// When `true` the diffuse texture's alpha channel is the opacity source.
    /// When `false` the texture alpha must be ignored; use the scalar `opacity`.
    pub uses_diffuse_texture_alpha: bool,
    // ── Texture asset paths (resolved) ────────────────────────────────────
    /// sRGB base-color / albedo texture.
    pub diffuse_texture: Option<String>,
    /// Linear tangent-space normal map.
    pub normal_texture: Option<String>,
    /// Metallic channel texture (channel index in `metallic_channel`).
    pub metallic_texture: Option<String>,
    /// Roughness channel texture (channel index in `roughness_channel`).
    pub roughness_texture: Option<String>,
    /// Linear AO texture (channel index in `occlusion_channel`).
    pub occlusion_texture: Option<String>,
    /// sRGB emissive texture.
    pub emissive_texture: Option<String>,
    pub metallic_channel: u32,
    pub roughness_channel: u32,
    pub occlusion_channel: u32,
    // ── Pipeline state ────────────────────────────────────────────────────
    pub alpha_policy: AlphaPolicy,
    pub double_sided: bool,
    /// Pre-computed fingerprint from the bridge IR (`first` XOR `second`
    /// as a simple cache key).
    pub fingerprint: u64,
    /// `MaterialX`-generated WGSL source for `MaterialXStandardSurface` materials.
    /// `None` for `UsdPreviewSurface` materials and when WGSL generation failed or
    /// the `materialx-wgsl` feature is not enabled.
    pub wgsl_source: Option<String>,
}

/// Folds a texture slot's UDIM placeholder to the canonical `<UDIM>` token as
/// it enters the render material, so a slot authored with an alternate
/// convention (`%(UDIM)d`, `<udim>`) still engages the tiled-array pipeline.
/// `None` stays `None`. See [`crate::udim::normalize_udim_token`].
fn normalize_texture(slot: Option<&str>) -> Option<String> {
    slot.map(|p| crate::udim::normalize_udim_token(p).into_owned())
}

impl RenderMaterialDesc {
    /// Builds a `RenderMaterialDesc` from the safe Rust [`MaterialRecord`].
    #[must_use]
    pub fn from_material_record(rec: &MaterialRecord) -> Self {
        let double_sided = rec.surface.sidedness == MaterialSidedness::DoubleSided;
        Self {
            material_path: rec.path.clone(),
            model: rec.surface.model,
            base_color: rec.base_color,
            metallic: rec.metallic,
            roughness: rec.roughness,
            subsurface: rec.subsurface,
            emissive: rec.emissive,
            transmission_color: rec.transmission_color,
            opacity: rec.opacity,
            opacity_threshold: rec.opacity_threshold,
            opacity_mode: rec.surface.opacity_mode,
            uses_diffuse_texture_alpha: rec.uses_diffuse_texture_alpha,
            diffuse_texture: normalize_texture(rec.diffuse_texture.as_deref()),
            normal_texture: normalize_texture(rec.normal_texture.as_deref()),
            metallic_texture: normalize_texture(rec.metallic_texture.as_deref()),
            roughness_texture: normalize_texture(rec.roughness_texture.as_deref()),
            occlusion_texture: normalize_texture(rec.occlusion_texture.as_deref()),
            emissive_texture: normalize_texture(rec.emissive_texture.as_deref()),
            metallic_channel: rec.metallic_texture_channel,
            roughness_channel: rec.roughness_texture_channel,
            occlusion_channel: rec.occlusion_texture_channel,
            alpha_policy: rec.surface.alpha_policy,
            double_sided,
            fingerprint: rec.surface.fingerprint.first ^ rec.surface.fingerprint.second,
            wgsl_source: rec.wgsl_source.clone(),
        }
    }

    /// Returns `true` when this material requires a separate alpha pass.
    #[must_use]
    pub fn is_transparent(&self) -> bool {
        matches!(self.alpha_policy, AlphaPolicy::Blend)
    }

    /// Returns `true` when this material uses alpha masking (cutout).
    #[must_use]
    pub fn is_masked(&self) -> bool {
        matches!(self.alpha_policy, AlphaPolicy::Mask)
    }

    /// Returns `true` when this is a transparent material with an authored,
    /// non-white `transmission_color` — i.e. colored glass whose absorption tint
    /// should colour the background behind it. Such draws are routed to the
    /// per-channel constant-blend pipeline instead of the plain premultiplied
    /// transparent pipeline (whose scalar dst factor cannot tint hue).
    #[must_use]
    pub fn has_colored_transmission(&self) -> bool {
        self.is_transparent()
            && self.opacity_mode == OpacityMode::Transparent
            && self.transmission_color.iter().any(|&c| c < 0.999)
    }

    /// The per-channel dst multiplier for the colored-glass blend:
    /// `transmission_color · (1 − opacity)`. Supplied per draw via
    /// [`wgpu::RenderPass::set_blend_constant`]; the blend is
    /// `out = src.rgb + dst · constant`. At `opacity == 0` the background is
    /// filtered by the full transmission tint; at `opacity == 1` it is fully
    /// occluded (constant `0`), matching the opaque limit.
    #[must_use]
    pub fn glass_blend_constant(&self) -> wgpu::Color {
        let a = f64::from(1.0 - self.opacity);
        let t = self.transmission_color;
        wgpu::Color {
            r: f64::from(t[0]) * a,
            g: f64::from(t[1]) * a,
            b: f64::from(t[2]) * a,
            a,
        }
    }

    /// The six texture slots in the canonical channel-bit order
    /// (diffuse, normal, metallic, roughness, occlusion, emissive) — the same
    /// order as [`crate::pipeline::MaterialUniforms::features`].
    fn texture_slots(&self) -> [&Option<String>; 6] {
        [
            &self.diffuse_texture,
            &self.normal_texture,
            &self.metallic_texture,
            &self.roughness_texture,
            &self.occlusion_texture,
            &self.emissive_texture,
        ]
    }

    /// Returns `true` when any texture slot references a `<UDIM>` template and
    /// therefore needs the tiled-array pipeline variant.
    #[must_use]
    pub fn is_udim(&self) -> bool {
        self.texture_slots()
            .iter()
            .filter_map(|s| s.as_deref())
            .any(crate::udim::is_udim_template)
    }

    /// Per-channel bitmask of which texture slots are `<UDIM>` templates, in the
    /// canonical channel-bit order (bit 0 = diffuse … bit 5 = emissive). A set
    /// bit means that channel samples the tiled array with per-tile UV; a clear
    /// bit (even inside a UDIM material) samples a plain single-tile texture.
    #[must_use]
    pub fn udim_channels(&self) -> u8 {
        let mut mask = 0u8;
        for (bit, slot) in self.texture_slots().iter().enumerate() {
            if slot.as_deref().is_some_and(crate::udim::is_udim_template) {
                mask |= 1 << bit;
            }
        }
        mask
    }

    /// Computes a fingerprint from all render-relevant fields of this descriptor.
    ///
    /// Used for fallback/displayColor materials that have no upstream `MaterialIrHasher`
    /// fingerprint. Call this at the end of any extraction branch that overrides fields
    /// after `fallback_material()` is returned, so adding future overridden fields
    /// (e.g. `displayOpacity`) automatically updates the dirty-check key without
    /// requiring per-attribute helper functions.
    ///
    /// For materials built via `from_material_record()` the upstream `MaterialIrHasher`
    /// fingerprint is already correct and more precise — do not replace it with this.
    #[must_use]
    pub fn compute_fingerprint(&self) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut h = DefaultHasher::new();
        for &f in &self.base_color {
            f.to_bits().hash(&mut h);
        }
        self.metallic.to_bits().hash(&mut h);
        self.roughness.to_bits().hash(&mut h);
        self.subsurface.to_bits().hash(&mut h);
        for &f in &self.emissive {
            f.to_bits().hash(&mut h);
        }
        for &f in &self.transmission_color {
            f.to_bits().hash(&mut h);
        }
        self.opacity.to_bits().hash(&mut h);
        self.opacity_threshold.to_bits().hash(&mut h);
        self.opacity_mode.hash(&mut h);
        self.uses_diffuse_texture_alpha.hash(&mut h);
        self.diffuse_texture.hash(&mut h);
        self.normal_texture.hash(&mut h);
        self.metallic_texture.hash(&mut h);
        self.roughness_texture.hash(&mut h);
        self.occlusion_texture.hash(&mut h);
        self.emissive_texture.hash(&mut h);
        self.metallic_channel.hash(&mut h);
        self.roughness_channel.hash(&mut h);
        self.occlusion_channel.hash(&mut h);
        (self.alpha_policy as u32).hash(&mut h);
        self.double_sided.hash(&mut h);
        self.wgsl_source.hash(&mut h);
        h.finish()
    }
}

/// Fallback material used when a mesh has no material binding or when the
/// material record is missing from the viewport projection.
#[must_use]
pub fn fallback_material(material_path: impl Into<String>) -> RenderMaterialDesc {
    RenderMaterialDesc {
        material_path: material_path.into(),
        model: SurfaceModel::DiagnosticFallback,
        base_color: [0.7, 0.7, 0.7],
        metallic: 0.0,
        // Matte default: unbound geometry (and displayColor-only prims) should read
        // as a diffuse surface, not semi-gloss. At 0.5 the split-sum env-BRDF grazing
        // bias reflects a uniform dome strongly enough to brighten flat faces at
        // glancing view angles ("aura"); ~0.8 flattens that grazing lift. Authored
        // UsdPreviewSurface materials keep the USD spec default (0.5) via the C++ shim.
        roughness: 0.8,
        subsurface: 0.0,
        emissive: [0.0, 0.0, 0.0],
        transmission_color: [1.0, 1.0, 1.0],
        opacity: 1.0,
        opacity_threshold: 0.5,
        opacity_mode: OpacityMode::Transparent,
        uses_diffuse_texture_alpha: false,
        diffuse_texture: None,
        normal_texture: None,
        metallic_texture: None,
        roughness_texture: None,
        occlusion_texture: None,
        emissive_texture: None,
        metallic_channel: 0,
        roughness_channel: 0,
        occlusion_channel: 0,
        alpha_policy: AlphaPolicy::Opaque,
        double_sided: false,
        fingerprint: 0,
        wgsl_source: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_is_opaque() {
        let m = fallback_material("/Looks/Missing");
        assert!(!m.is_transparent());
        assert!(!m.is_masked());
        assert_eq!(m.material_path, "/Looks/Missing");
    }

    #[test]
    fn udim_detection_and_channel_mask() {
        let mut m = fallback_material("/Looks/Udim");
        assert!(!m.is_udim());
        assert_eq!(m.udim_channels(), 0);

        // Diffuse (bit 0) is a UDIM template; occlusion (bit 4) is plain.
        m.diffuse_texture = Some("/abs/albedo.<UDIM>.png".to_owned());
        m.occlusion_texture = Some("/abs/ao.png".to_owned());
        assert!(m.is_udim());
        assert_eq!(m.udim_channels(), 1 << 0);

        // Add a UDIM normal map (bit 1).
        m.normal_texture = Some("/abs/n.<UDIM>.png".to_owned());
        assert_eq!(m.udim_channels(), (1 << 0) | (1 << 1));
    }

    #[test]
    fn colored_transmission_classifier() {
        // White transmission on a blend material → not colored glass.
        let mut m = fallback_material("/Looks/Glass");
        m.alpha_policy = AlphaPolicy::Blend;
        m.transmission_color = [1.0, 1.0, 1.0];
        assert!(!m.has_colored_transmission());

        // Colored transmission on a blend material → colored glass.
        m.transmission_color = [0.2, 1.0, 0.2];
        assert!(m.has_colored_transmission());

        let glass_fingerprint = m.compute_fingerprint();
        m.opacity_mode = OpacityMode::Presence;
        assert!(
            !m.has_colored_transmission(),
            "presence always uses scalar coverage"
        );
        assert_ne!(glass_fingerprint, m.compute_fingerprint());
        m.opacity_mode = OpacityMode::Transparent;

        // Same tint but opaque → not routed to the colored-glass path.
        m.alpha_policy = AlphaPolicy::Opaque;
        assert!(!m.has_colored_transmission());
    }

    #[test]
    fn glass_blend_constant_scales_by_alpha() {
        let mut m = fallback_material("/Looks/Glass");
        m.alpha_policy = AlphaPolicy::Blend;
        m.transmission_color = [0.2, 1.0, 0.2];

        // opacity 0 → full transmission tint, alpha 1 (fully additive over dst).
        // Tolerance is loose: transmission_color is f32, cast to f64 for wgpu::Color.
        m.opacity = 0.0;
        let c = m.glass_blend_constant();
        assert!((c.r - 0.2).abs() < 1e-6);
        assert!((c.g - 1.0).abs() < 1e-6);
        assert!((c.b - 0.2).abs() < 1e-6);
        assert!((c.a - 1.0).abs() < 1e-6);

        // opacity 0.5 → tint halved.
        m.opacity = 0.5;
        let c = m.glass_blend_constant();
        assert!((c.r - 0.1).abs() < 1e-6);
        assert!((c.g - 0.5).abs() < 1e-6);
        assert!((c.b - 0.1).abs() < 1e-6);
        assert!((c.a - 0.5).abs() < 1e-6);
    }
}
