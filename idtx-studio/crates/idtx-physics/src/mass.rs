//! Mass-property resolution replicating the `UsdPhysics` precedence.

/// Default collision density in SI units — ~water, 1000 kg/m³. The `UsdPhysics`
/// default used for volume→mass when no density is authored locally or via a
/// bound material (and no explicit mass is given).
pub const DEFAULT_DENSITY_SI: f32 = 1000.0;

/// The default density ([`DEFAULT_DENSITY_SI`]) converted into the stage's own
/// units: `density_stage = 1000 · metersPerUnit³ / kilogramsPerUnit`. For a stage
/// authored with `metersPerUnit = kilogramsPerUnit = 1` this is simply `1000`.
/// Non-finite / non-positive stage units fall back to `1`.
#[must_use]
pub fn default_density_in_stage_units(meters_per_unit: f32, kilograms_per_unit: f32) -> f32 {
    let mpu = if meters_per_unit.is_finite() && meters_per_unit > 0.0 {
        meters_per_unit
    } else {
        1.0
    };
    let kgpu = if kilograms_per_unit.is_finite() && kilograms_per_unit > 0.0 {
        kilograms_per_unit
    } else {
        1.0
    };
    DEFAULT_DENSITY_SI * mpu.powi(3) / kgpu
}

/// A collider's resolved mass input for the compound-body mass computation
/// (plan §5.1/§5.3). The adapter gathers the authored values (walking the body
/// subtree for an inherited `MassAPI` density and the bound-material density);
/// this applies the `UsdPhysics` precedence to produce the final per-collider
/// density and any explicit mass.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColliderMass {
    /// Effective density (always finite and `> 0`): nearest `MassAPI`
    /// `physics:density` (collider or inherited) → bound-material density → the
    /// stage default.
    pub density: f32,
    /// Explicit `physics:mass` authored on the collider prim, if any (`> 0`).
    pub explicit_mass: Option<f32>,
}

impl ColliderMass {
    /// Resolve a collider's density + explicit mass with the `UsdPhysics`
    /// precedence: a `MassAPI` `physics:density` (on the collider, or inherited
    /// from an ancestor up to the body) overrides a bound-material density, which
    /// overrides the stage default. A `physics:mass` on the collider is carried
    /// through separately (it scales that collider's contribution). Non-finite /
    /// non-positive authored values are ignored.
    #[must_use]
    pub fn resolve(
        explicit_mass: Option<f32>,
        mass_api_density: Option<f32>,
        material_density: Option<f32>,
        default_density: f32,
    ) -> Self {
        let pos = |v: Option<f32>| v.filter(|d| d.is_finite() && *d > 0.0);
        let density = pos(mass_api_density)
            .or_else(|| pos(material_density))
            .unwrap_or(default_density);
        Self { density, explicit_mass: pos(explicit_mass) }
    }
}

/// How a dynamic body's mass is determined, after applying the `UsdPhysics`
/// precedence (plan §5.3). The engine turns this into rapier collider density /
/// explicit body mass at build time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MassSpec {
    /// Explicit `physics:mass` (kg) — overrides everything.
    Explicit(f32),
    /// `MassAPI` `physics:density` (kg per volume unit³).
    Density(f32),
    /// Density from the bound physics **material**.
    MaterialDensity(f32),
    /// No authored mass or density anywhere — let `parry` derive mass properties
    /// from the collider volume at a unit reference density.
    Auto,
}

impl MassSpec {
    /// Resolve the authored inputs with the `UsdPhysics` precedence, highest
    /// first: explicit `mass` > `MassAPI` `density` > bound material `density` >
    /// geometry-volume auto. A non-finite or non-positive authored value is
    /// ignored (falls through to the next source).
    #[must_use]
    pub fn resolve(
        explicit_mass: Option<f32>,
        mass_api_density: Option<f32>,
        material_density: Option<f32>,
    ) -> Self {
        if let Some(m) = explicit_mass.filter(|m| m.is_finite() && *m > 0.0) {
            Self::Explicit(m)
        } else if let Some(d) = mass_api_density.filter(|d| d.is_finite() && *d > 0.0) {
            Self::Density(d)
        } else if let Some(d) = material_density.filter(|d| d.is_finite() && *d > 0.0) {
            Self::MaterialDensity(d)
        } else {
            Self::Auto
        }
    }

    /// The collider density to build with. An explicit body mass contributes no
    /// collider density (the mass is applied to the body directly); every other
    /// variant maps to its density, with `Auto` using unit density so `parry`
    /// derives a volume-proportional mass.
    #[must_use]
    pub fn collider_density(self) -> f32 {
        match self {
            Self::Explicit(_) => 0.0,
            Self::Density(d) | Self::MaterialDensity(d) => d,
            Self::Auto => 1.0,
        }
    }

    /// The extra body mass (kg) to apply, if the mass was given explicitly.
    #[must_use]
    pub fn explicit_body_mass(self) -> Option<f32> {
        match self {
            Self::Explicit(m) => Some(m),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn precedence_is_mass_density_material_auto() {
        // Explicit mass wins over everything.
        assert_eq!(
            MassSpec::resolve(Some(2.0), Some(500.0), Some(10.0)),
            MassSpec::Explicit(2.0)
        );
        // No mass → MassAPI density.
        assert_eq!(
            MassSpec::resolve(None, Some(500.0), Some(10.0)),
            MassSpec::Density(500.0)
        );
        // No mass/density → material density.
        assert_eq!(
            MassSpec::resolve(None, None, Some(10.0)),
            MassSpec::MaterialDensity(10.0)
        );
        // Nothing authored → auto.
        assert_eq!(MassSpec::resolve(None, None, None), MassSpec::Auto);
    }

    #[test]
    fn non_positive_or_nan_inputs_are_ignored() {
        // Zero / negative / NaN explicit mass falls through to density.
        assert_eq!(MassSpec::resolve(Some(0.0), Some(5.0), None), MassSpec::Density(5.0));
        assert_eq!(MassSpec::resolve(Some(-1.0), None, None), MassSpec::Auto);
        assert_eq!(MassSpec::resolve(Some(f32::NAN), None, None), MassSpec::Auto);
    }

    #[test]
    fn density_and_mass_mapping() {
        let approx = |a: f32, b: f32| (a - b).abs() < 1e-6;
        assert!(approx(MassSpec::Explicit(3.0).collider_density(), 0.0));
        assert_eq!(MassSpec::Explicit(3.0).explicit_body_mass(), Some(3.0));
        assert!(approx(MassSpec::Density(5.0).collider_density(), 5.0));
        assert!(MassSpec::Density(5.0).explicit_body_mass().is_none());
        assert!(approx(MassSpec::Auto.collider_density(), 1.0));
    }

    #[test]
    fn default_density_converts_stage_units() {
        // metersPerUnit = kilogramsPerUnit = 1 ⇒ plain 1000.
        assert!((default_density_in_stage_units(1.0, 1.0) - 1000.0).abs() < 1e-3);
        // centimetres (mpu = 0.01): 1000 · 0.01³ = 1e-3 kg/cm³.
        assert!((default_density_in_stage_units(0.01, 1.0) - 1.0e-3).abs() < 1e-9);
        // grams (kgpu = 0.001): 1000 / 0.001 = 1e6.
        assert!((default_density_in_stage_units(1.0, 0.001) - 1.0e6).abs() < 1.0);
        // Non-positive stage units fall back to 1 ⇒ 1000.
        assert!((default_density_in_stage_units(0.0, -1.0) - 1000.0).abs() < 1e-3);
    }

    #[test]
    fn collider_mass_precedence_density() {
        // MassAPI density wins over material density and the default.
        let m = ColliderMass::resolve(None, Some(500.0), Some(10.0), 1000.0);
        assert_eq!(m, ColliderMass { density: 500.0, explicit_mass: None });
        // No MassAPI density → bound material density.
        let m = ColliderMass::resolve(None, None, Some(10.0), 1000.0);
        assert!((m.density - 10.0).abs() < 1e-4);
        // Nothing authored → stage default.
        let m = ColliderMass::resolve(None, None, None, 1000.0);
        assert!((m.density - 1000.0).abs() < 1e-3);
        // Non-positive authored densities are ignored (fall through to default).
        let m = ColliderMass::resolve(None, Some(0.0), Some(-5.0), 1000.0);
        assert!((m.density - 1000.0).abs() < 1e-3);
    }

    #[test]
    fn collider_mass_carries_explicit_mass() {
        let m = ColliderMass::resolve(Some(2.5), Some(500.0), None, 1000.0);
        assert_eq!(m.explicit_mass, Some(2.5));
        // density is still resolved alongside an explicit mass.
        assert!((m.density - 500.0).abs() < 1e-4);
        // Non-positive explicit mass is dropped.
        assert_eq!(ColliderMass::resolve(Some(0.0), None, None, 1000.0).explicit_mass, None);
    }
}
