//! Lossless, role-preserving USD default values for live attribute sync.

#[cfg(feature = "authoring")]
use idtx_usd_sys::bridge::ffi;

/// The scalar USD type token stays explicit; array-ness belongs to `value`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SyncAttribute {
    pub name: String,
    pub type_name: String,
    pub value: SyncAttributeValue,
}

/// One populated lane with flattened components in USD memory order.
/// Matrices are row-major and quaternions use real, i, j, k order.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SyncAttributeValue {
    pub doubles: Vec<f64>,
    pub ints: Vec<i64>,
    pub uints: Vec<u64>,
    pub bools: Vec<bool>,
    pub strings: Vec<String>,
    pub is_array: bool,
}

/// Exact root property spec for restoring optimistic edits without changing
/// weaker composition, metadata, time samples, connections or value blocks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootAttributeSnapshot {
    pub prim_path: String,
    pub name: String,
    pub text: String,
}

#[cfg(feature = "authoring")]
impl crate::StageService {
    /// Read the full root property spec directly by scene path, including an
    /// inactive prim. An absent root property returns an empty snapshot.
    pub fn root_attribute_text(&self, path: &str, name: &str) -> Result<String, crate::StageError> {
        Ok(ffi::root_property_text(&self.handle, path, name)?)
    }

    /// Normalize a property edit into a metadata-only delta. Both snapshots
    /// retain identical type/custom/variability/default opinions; metadata,
    /// connections and time samples preserve their individual before/after
    /// states. Applying this delta leaves a newer authoritative default intact.
    pub fn root_attribute_metadata_delta(
        &self,
        path: &str,
        name: &str,
        expected: &str,
        desired: &str,
    ) -> Result<(String, String), crate::StageError> {
        let delta = ffi::root_attribute_metadata_delta(path, name, expected, desired)?;
        Ok((delta.expected, delta.desired))
    }

    /// Restore an exact root property spec by Sdf path, including on an inactive
    /// prim. An empty snapshot removes only that root property opinion.
    pub fn replace_root_attribute_text(
        &mut self,
        path: &str,
        name: &str,
        source: &str,
    ) -> Result<(), crate::StageError> {
        ffi::replace_root_property_text(self.handle.pin_mut(), path, name, source)?;
        Ok(())
    }

    /// Root opinions excluding supported default values carried by live sync.
    /// Metadata, connections, time samples and blocked values remain visible.
    pub fn root_non_sync_text(&self) -> Result<String, crate::StageError> {
        Ok(ffi::root_non_sync_text(&self.handle)?)
    }

    /// Enumerate root-authored attribute specs, including inactive prims and
    /// properties without concrete defaults. Variant-contained specs are
    /// excluded because the live attribute protocol addresses scene paths.
    pub fn root_attribute_snapshots(
        &self,
    ) -> Result<Vec<RootAttributeSnapshot>, crate::StageError> {
        self.attribute_snapshots(false)
    }

    /// Capture transform specs without serializing geometry or other defaults.
    pub fn root_transform_attribute_snapshots(
        &self,
    ) -> Result<Vec<RootAttributeSnapshot>, crate::StageError> {
        self.attribute_snapshots(true)
    }

    fn attribute_snapshots(
        &self,
        transforms_only: bool,
    ) -> Result<Vec<RootAttributeSnapshot>, crate::StageError> {
        Ok(
            ffi::root_attribute_snapshots(&self.handle, transforms_only)?
                .into_iter()
                .map(|raw| RootAttributeSnapshot {
                    prim_path: raw.prim_path,
                    name: raw.name,
                    text: raw.text,
                })
                .collect(),
        )
    }

    /// Read concrete root defaults on a prim, excluding transform properties.
    /// Unsupported defaults return an error so callers can report them and
    /// inspect individual properties through `read_sync_attribute`.
    pub fn read_sync_attributes(
        &self,
        path: &str,
    ) -> Result<Vec<SyncAttribute>, crate::StageError> {
        let mut attributes = Vec::new();
        for snapshot in self.root_attribute_snapshots()? {
            if snapshot.prim_path == path {
                if let Some(attribute) = self.read_sync_attribute(path, &snapshot.name)? {
                    attributes.push(attribute);
                }
            }
        }
        Ok(attributes)
    }

    /// Read only the concrete default authored in the root layer. Composed
    /// fallbacks, time samples, missing defaults and value blocks return `None`.
    pub fn read_sync_attribute(
        &self,
        path: &str,
        property: &str,
    ) -> Result<Option<SyncAttribute>, crate::StageError> {
        let raw = ffi::read_sync_attribute(&self.handle, path, property)?;
        Ok(raw.has_value.then(|| SyncAttribute {
            name: raw.name,
            type_name: raw.type_name,
            value: SyncAttributeValue {
                doubles: raw.value.doubles,
                ints: raw.value.ints,
                uints: raw.value.uints,
                bools: raw.value.bools,
                strings: raw.value.strings,
                is_array: raw.value.is_array,
            },
        }))
    }

    /// Apply authoritative defaults to the root layer in one transaction,
    /// preserving property type, metadata, connections and time samples.
    /// Asset paths are anchored to that same root layer.
    pub fn apply_sync_attributes(
        &mut self,
        path: &str,
        attributes: &[SyncAttribute],
    ) -> Result<(), crate::StageError> {
        let raw: Vec<_> = attributes
            .iter()
            .map(|attribute| ffi::SyncAttributeRaw {
                name: attribute.name.clone(),
                type_name: attribute.type_name.clone(),
                value: ffi::SyncAttributeValueRaw {
                    doubles: attribute.value.doubles.clone(),
                    ints: attribute.value.ints.clone(),
                    uints: attribute.value.uints.clone(),
                    bools: attribute.value.bools.clone(),
                    strings: attribute.value.strings.clone(),
                    is_array: attribute.value.is_array,
                },
                has_value: true,
            })
            .collect();
        ffi::apply_sync_attributes(self.handle.pin_mut(), path, &raw)?;
        Ok(())
    }
}
