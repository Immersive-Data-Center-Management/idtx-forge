//! Session-layer authoring (`stage_edit` capability).
//!
//! A behavior granted `stage_edit` may author non-destructive edits against
//! the stage's **session layer** (never dirtying the saved stage). Guests do
//! **not** mutate USD on the hot path: each call here *enqueues* a deferred
//! [`crate::abi`] session edit that the host drains after the update hooks,
//! applies to the session layer, and re-projects the same frame (so the ECS /
//! render worlds reflect it).
//!
//! On wasm the writes route to the capability-gated `idtx_session_set_*` host
//! imports (absent — and therefore link-failing — without the grant); natively
//! they are inert `Ok(())` so a behavior using session edits stays
//! unit-testable without a host.

use crate::abi::{Entity as WireEntity, Xform};
use crate::entity::{Entity, Transform};
use crate::result::{Result, check};

/// Deferred session-layer authoring surface.
///
/// Requires the behavior's manifest to grant the `stage_edit` capability
/// (otherwise the host imports are unlinked and the module fails to load).
/// Obtain one from [`crate::ctx::Ctx::stage_edit`]. Cheap to copy — holds no
/// state; every method posts a single deferred edit.
#[derive(Clone, Copy, Debug)]
pub struct StageEdit {
    _private: (),
}

impl StageEdit {
    /// Construct the handle for the current hook. Cheap; holds no state.
    #[must_use]
    pub(crate) fn current() -> Self {
        Self { _private: () }
    }

    /// Enqueue a local-transform edit against `entity`'s session-layer opinion.
    pub fn set_transform(self, entity: Entity, t: &Transform) -> Result<()> {
        Self::post_transform(entity.wire(), t)
    }

    /// Enqueue a local-transform edit against the behavior's own entity
    /// (equivalent to `set_transform(ctx.entity(), t)`, but resolved host-side
    /// so the guest need not know its own wire id).
    pub fn set_self_transform(self, t: &Transform) -> Result<()> {
        Self::post_transform(WireEntity::NULL, t)
    }

    /// Enqueue a visibility edit against `entity`'s session-layer opinion
    /// (`true` = `inherited`, `false` = `invisible`).
    pub fn set_visibility(self, entity: Entity, visible: bool) -> Result<()> {
        Self::post_visibility(entity.wire(), visible)
    }

    /// Enqueue a visibility edit against the behavior's own entity.
    pub fn set_self_visibility(self, visible: bool) -> Result<()> {
        Self::post_visibility(WireEntity::NULL, visible)
    }

    /// Enqueue creation of a new child prim under `parent`: a leaf `name` of USD
    /// type `type_name` (`""` = typeless, e.g. `"Xform"`, `"Sphere"`). Deferred
    /// and applied through the session's structural (topology) path.
    pub fn define_prim(self, parent: Entity, type_name: &str, name: &str) -> Result<()> {
        Self::post_define(parent.wire(), type_name, name)
    }

    /// Enqueue creation of a new child prim under the behavior's own entity.
    pub fn define_child(self, type_name: &str, name: &str) -> Result<()> {
        Self::post_define(WireEntity::NULL, type_name, name)
    }

    /// Enqueue removal of `entity`'s prim (and its subtree) from the session's
    /// edit target. Deferred and applied through the structural path.
    pub fn remove_prim(self, entity: Entity) -> Result<()> {
        Self::post_remove(entity.wire())
    }

    /// Enqueue removal of the behavior's own prim.
    pub fn remove_self(self) -> Result<()> {
        Self::post_remove(WireEntity::NULL)
    }

    /// Attach a screen-space HUD element to `camera` (typically the player
    /// camera): atomically define an `IdtxHud` child named `name` into the
    /// session layer and author its parameters there. `size_px` is the
    /// on-screen size in pixels (`[0, 0]` = use the image's native size — handy
    /// when the image dimensions are only known at runtime); `opacity` is in
    /// `[0, 1]`. Deferred and applied the same frame.
    ///
    /// A single op (rather than define-then-set-attrs) because the SDK cannot
    /// hand back the freshly-created prim's entity id within the same frame.
    pub fn attach_hud(
        self,
        camera: Entity,
        name: &str,
        image: &str,
        size_px: [i32; 2],
        opacity: f32,
    ) -> Result<()> {
        Self::post_attach_hud(camera.wire(), name, image, size_px, opacity)
    }

    /// Attach a HUD element to the behavior's own entity (equivalent to
    /// `attach_hud(ctx.entity(), …)`, resolved host-side).
    pub fn attach_self_hud(
        self,
        name: &str,
        image: &str,
        size_px: [i32; 2],
        opacity: f32,
    ) -> Result<()> {
        Self::post_attach_hud(WireEntity::NULL, name, image, size_px, opacity)
    }

    /// Attach a centered crosshair HUD at full opacity — the common case.
    /// `size_px` is the on-screen size in pixels (`[0, 0]` = native image size).
    pub fn attach_crosshair(self, image: &str, size_px: [i32; 2]) -> Result<()> {
        Self::post_attach_hud(WireEntity::NULL, "Crosshair", image, size_px, 1.0)
    }

    /// Author the string `value` into `idtx:widget:value` of the widget-field
    /// child `child_name` (panel-relative, e.g. `"Value"` or `"Row/Value"`) of
    /// the `IdtxWidgetPanel` this behavior is attached to.
    ///
    /// The edit is deferred to the session layer (never dirtying the saved
    /// stage). Because the authored property is in the `idtx:widget:` namespace,
    /// the session re-hydrates the panel and re-rasterizes it the same frame —
    /// so the field's readout updates live. Target an `IdtxWidgetFieldString`
    /// child (its `idtx:widget:value` is a `string`); the value is authored
    /// verbatim, so it carries numbers or text alike.
    pub fn set_widget_value(self, child_name: &str, value: &str) -> Result<()> {
        Self::post_set_widget_value(WireEntity::NULL, child_name, value)
    }

    fn post_transform(target: WireEntity, t: &Transform) -> Result<()> {
        let x: Xform = (*t).into();
        let bytes = bytemuck::bytes_of(&x);
        #[cfg(target_arch = "wasm32")]
        let code = crate::abi::ErrorCode::from_u32(unsafe {
            crate::__rt::idtx_session_set_xform(target.to_bits(), bytes.as_ptr())
        });
        #[cfg(not(target_arch = "wasm32"))]
        let code = {
            let _ = (target, bytes);
            crate::abi::ErrorCode::Ok
        };
        check(code)
    }

    fn post_visibility(target: WireEntity, visible: bool) -> Result<()> {
        #[cfg(target_arch = "wasm32")]
        let code = crate::abi::ErrorCode::from_u32(unsafe {
            crate::__rt::idtx_session_set_visibility(target.to_bits(), u32::from(visible))
        });
        #[cfg(not(target_arch = "wasm32"))]
        let code = {
            let _ = (target, visible);
            crate::abi::ErrorCode::Ok
        };
        check(code)
    }

    fn post_define(target: WireEntity, type_name: &str, name: &str) -> Result<()> {
        #[cfg(target_arch = "wasm32")]
        let code = crate::abi::ErrorCode::from_u32(unsafe {
            crate::__rt::idtx_session_define_prim(
                target.to_bits(),
                type_name.as_ptr(),
                u32::try_from(type_name.len()).unwrap_or(u32::MAX),
                name.as_ptr(),
                u32::try_from(name.len()).unwrap_or(u32::MAX),
            )
        });
        #[cfg(not(target_arch = "wasm32"))]
        let code = {
            let _ = (target, type_name, name);
            crate::abi::ErrorCode::Ok
        };
        check(code)
    }

    fn post_remove(target: WireEntity) -> Result<()> {
        #[cfg(target_arch = "wasm32")]
        let code = crate::abi::ErrorCode::from_u32(unsafe {
            crate::__rt::idtx_session_remove_prim(target.to_bits())
        });
        #[cfg(not(target_arch = "wasm32"))]
        let code = {
            let _ = target;
            crate::abi::ErrorCode::Ok
        };
        check(code)
    }

    fn post_attach_hud(
        target: WireEntity,
        name: &str,
        image: &str,
        size_px: [i32; 2],
        opacity: f32,
    ) -> Result<()> {
        #[cfg(target_arch = "wasm32")]
        let code = crate::abi::ErrorCode::from_u32(unsafe {
            crate::__rt::idtx_hud_attach(
                target.to_bits(),
                name.as_ptr(),
                u32::try_from(name.len()).unwrap_or(u32::MAX),
                image.as_ptr(),
                u32::try_from(image.len()).unwrap_or(u32::MAX),
                size_px[0],
                size_px[1],
                opacity,
            )
        });
        #[cfg(not(target_arch = "wasm32"))]
        let code = {
            let _ = (target, name, image, size_px, opacity);
            crate::abi::ErrorCode::Ok
        };
        check(code)
    }

    fn post_set_widget_value(target: WireEntity, name: &str, value: &str) -> Result<()> {
        #[cfg(target_arch = "wasm32")]
        let code = crate::abi::ErrorCode::from_u32(unsafe {
            crate::__rt::idtx_session_set_widget_value(
                target.to_bits(),
                name.as_ptr(),
                u32::try_from(name.len()).unwrap_or(u32::MAX),
                value.as_ptr(),
                u32::try_from(value.len()).unwrap_or(u32::MAX),
            )
        });
        #[cfg(not(target_arch = "wasm32"))]
        let code = {
            let _ = (target, name, value);
            crate::abi::ErrorCode::Ok
        };
        check(code)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The native stubs are inert `Ok(())` so author logic (which calls
    // `set_widget_value` from an HTTP handler) stays unit-testable without a
    // wasm host. This mirrors the net-module native-stub tests.
    #[test]
    fn set_widget_value_native_stub_is_inert() {
        let edit = StageEdit::current();
        assert!(edit.set_widget_value("Row/Value", "42.0").is_ok());
        assert!(edit.set_widget_value("", "").is_ok());
    }
}
