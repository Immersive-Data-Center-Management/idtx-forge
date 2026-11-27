//! Widget interaction (`widget` capability) — click events.
//!
//! Two roles, both ergonomic, both using non-capturing `fn` handlers (the guest
//! has no allocator, so a handler cannot capture — but it receives `&mut self`
//! and drives the behavior's own fields, exactly like the async-HTTP callback in
//! [`crate::net`]):
//!
//! # Panel behavior (attached to an `IdtxWidgetPanel`)
//!
//! Register a handler per child in `on_start`; the SDK-generated `_idtx_on_event`
//! dispatcher routes a host-dispatched click to it — **no `on_event`, no match,
//! no dispatcher boilerplate**:
//!
//! ```ignore
//! fn on_start(&mut self, ctx: &Ctx) -> Result<()> {
//!     ctx.widget().on_click("StartButton", Self::start_clicked)?;     // direct child
//!     ctx.widget().on_click("Footer/QuitButton", Self::quit_clicked)?; // nested child
//!     Ok(())
//! }
//! fn start_clicked(&mut self, _ctx: &Ctx, ev: &WidgetClick) -> Result<()> {
//!     self.running = true; Ok(())
//! }
//! ```
//!
//! An element is addressed by its **panel-relative namespace path** (the segment
//! below the panel, e.g. `"Footer/QuitButton"`), matching the widget-value write
//! convention.
//!
//! # Pointer / gaze behavior (attached to camera / controller)
//!
//! Opt the behavior's own raycast into [`crate::scene::RayOpts::block_widgets`]
//! so widgets occlude like geometry; hand a widget hit to
//! [`Widgets::handle`], which runs the built-in processing (cue + dispatch) and
//! reports whether the widget consumed the interaction:
//!
//! ```ignore
//! if let Some(hit) = ctx.scene().raycast(o, d, &RayOpts::new().block_widgets()) {
//!     if hit.is_widget() && ctx.widgets().handle(hit) { return Ok(()); }
//!     // else: normal world interaction on `hit`
//! }
//! ```

use core::cell::RefCell;

use arrayvec::{ArrayString, ArrayVec};

use crate::abi::{ErrorCode, WidgetEventKind};
use crate::ctx::Ctx;
use crate::result::{BehaviorError, Result};
use crate::scene::query::{RayHit, MAX_PATH_LEN};

/// Max registered widget handlers per behavior type (bounds the inline table).
const MAX_WIDGET_HANDLERS: usize = 32;

/// A widget interaction delivered to a registered handler. A zero-state handle,
/// valid only for the duration of the handler call: it reads the element path /
/// UV / kind back from the host on demand.
#[derive(Clone, Copy, Debug)]
pub struct WidgetClick {
    uv: [f32; 2],
    kind: WidgetEventKind,
}

impl WidgetClick {
    /// Source explicitly supplied by the pointer or independently resolved by
    /// the input router. Unspecified means unknown/ambiguous, never "right".
    pub fn source(&self) -> crate::abi::PointerSource {
        #[cfg(target_arch = "wasm32")]
        return crate::abi::PointerSource::from_u32(unsafe { crate::__rt::idtx_widget_event_source() });
        #[cfg(not(target_arch = "wasm32"))]
        crate::abi::PointerSource::Unspecified
    }

    pub const fn is_panel_background(&self) -> bool { matches!(self.kind, WidgetEventKind::PanelClick) }
    /// Build the handle for the current dispatched widget event. Hidden — only
    /// the generated `_idtx_on_event` dispatcher calls this.
    #[doc(hidden)]
    #[must_use]
    pub fn current() -> Self {
        #[allow(unused_mut)]
        let mut uv = [0f32; 2];
        #[cfg(target_arch = "wasm32")]
        unsafe {
            crate::__rt::idtx_widget_event_uv(uv.as_mut_ptr().cast());
        }
        let kind = WidgetEventKind::from_u32(current_kind_raw());
        Self { uv, kind }
    }

    /// The panel-relative path of the clicked element (e.g. `"Footer/Quit"`).
    #[must_use]
    pub fn element(&self) -> ArrayString<MAX_PATH_LEN> {
        current_element()
    }

    /// Child activation (`Click`) or bubbled/background activation (`PanelClick`).
    #[must_use]
    pub const fn kind(&self) -> WidgetEventKind {
        self.kind
    }

    /// Panel-local UV of the hit point (top-left origin, `(0,0)..(1,1)`).
    #[must_use]
    pub const fn uv(&self) -> (f32, f32) {
        (self.uv[0], self.uv[1])
    }
}

/// Raw `idtx_widget_event_kind` read (sentinel when the current event is not a
/// widget event). Also used by the macro dispatcher to branch.
#[doc(hidden)]
#[must_use]
pub fn current_kind_raw() -> u32 {
    #[cfg(target_arch = "wasm32")]
    {
        unsafe { crate::__rt::idtx_widget_event_kind() }
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        crate::abi::WIDGET_EVENT_NONE
    }
}

/// Read the current widget event's element path into an inline string.
#[must_use]
fn current_element() -> ArrayString<MAX_PATH_LEN> {
    #[allow(unused_mut)]
    let mut buf = [0u8; MAX_PATH_LEN];
    #[allow(unused_mut, unused_assignments)]
    let mut len = 0usize;
    #[cfg(target_arch = "wasm32")]
    {
        let true_len =
            unsafe { crate::__rt::idtx_widget_event_element(buf.as_mut_ptr(), MAX_PATH_LEN as u32) };
        len = (true_len as usize).min(MAX_PATH_LEN);
    }
    let s = core::str::from_utf8(&buf[..len]).unwrap_or("");
    ArrayString::from(s).unwrap_or_default()
}

/// A widget-click handler: a bare `fn` taking the behavior by `&mut`, the hook
/// context, and the [`WidgetClick`]. Registered in `on_start` via
/// [`Widget::on_click`]; invoked by the generated dispatcher when the host
/// dispatches a click to a registered element.
pub type WidgetHandler<B> = fn(&mut B, &Ctx, &WidgetClick) -> Result<()>;

/// A fixed-capacity, single-threaded registry mapping `(element path, kind)` to
/// the handler the author passed. One instance lives per behavior type
/// (generated by the `#[behavior]` macro) so the dispatcher can route a
/// delivered click generically through [`WidgetHandlerRegistry`].
///
/// No allocator: an inline [`ArrayVec`]. Entries are persistent (a widget fires
/// many times), keyed by path + kind, so capacity bounds *distinct* handlers.
pub struct WidgetHandlerSlot<B> {
    cell: RefCell<ArrayVec<(ArrayString<MAX_PATH_LEN>, WidgetEventKind, WidgetHandler<B>), MAX_WIDGET_HANDLERS>>,
}

// SAFETY: identical reasoning to `net::HandlerSlot` — a non-threaded WASM guest
// is single-threaded, so the `RefCell` is never accessed concurrently.
#[cfg(all(target_arch = "wasm32", not(target_feature = "atomics")))]
unsafe impl<B> Sync for WidgetHandlerSlot<B> {}

impl<B> WidgetHandlerSlot<B> {
    /// A fresh, empty registry (`const` so it can initialise a `static`).
    #[must_use]
    pub const fn new() -> Self {
        Self {
            cell: RefCell::new(ArrayVec::new_const()),
        }
    }

    /// Register (or replace) `handler` for `(path, kind)`. Returns
    /// [`ErrorCode::InvalidArgument`] when the registry is full.
    ///
    /// # Errors
    /// Full registry.
    pub fn register(&self, path: &str, kind: WidgetEventKind, handler: WidgetHandler<B>) -> Result<()> {
        let key = ArrayString::from(path).map_err(|_| BehaviorError(ErrorCode::InvalidArgument))?;
        let mut v = self.cell.borrow_mut();
        if let Some(slot) = v.iter_mut().find(|(p, k, _)| *p == key && *k == kind) {
            slot.2 = handler;
            return Ok(());
        }
        v.try_push((key, kind, handler))
            .map_err(|_| BehaviorError(ErrorCode::InvalidArgument))
    }

    /// Look up the handler for `(path, kind)` (persistent; not removed).
    #[must_use]
    pub fn get(&self, path: &str, kind: WidgetEventKind) -> Option<WidgetHandler<B>> {
        self.cell
            .borrow()
            .iter()
            .find(|(p, k, _)| p.as_str() == path && *k == kind)
            .map(|(_, _, h)| *h)
    }
}

impl<B> Default for WidgetHandlerSlot<B> {
    fn default() -> Self {
        Self::new()
    }
}

/// Implemented (by the `#[behavior]` macro) for a behavior type so the generated
/// `_idtx_on_event` dispatcher can reach that type's [`WidgetHandlerSlot`].
/// Hidden plumbing — authors never name it.
#[doc(hidden)]
pub trait WidgetHandlerRegistry: Sized + 'static {
    /// The behavior type's widget-handler registry (a generated `static`).
    fn __widget_registry() -> &'static WidgetHandlerSlot<Self>;
}

/// Panel-side widget surface, returned by [`crate::ctx::Ctx::widget`]. Registers
/// click handlers for the panel's child elements.
#[derive(Clone, Copy, Debug)]
pub struct Widget {
    _private: (),
}

impl Widget {
    /// Background handler. An enabled child with a registered click handler
    /// gets first refusal; otherwise a label or blank panel area bubbles here.
    /// The event kind distinguishes this target from authored child paths.
    pub fn on_panel_click<B: WidgetHandlerRegistry>(self, handler: WidgetHandler<B>) -> Result<()> {
        B::__widget_registry().register("", WidgetEventKind::PanelClick, handler)?;
        #[cfg(target_arch = "wasm32")]
        crate::result::check(ErrorCode::from_u32(unsafe {
            crate::__rt::idtx_widget_register_handler(0, "".as_ptr(), 0, WidgetEventKind::PanelClick.as_u32())
        }))?;
        Ok(())
    }
    /// Store a transient input value owned by this behavior (16 fields, 1024
    /// UTF-8 bytes each). Empty removes it. Secrets never become USD opinions.
    pub fn set_value(self, name: &str, value: &str) -> Result<()> {
        if name.is_empty() || name.len() > 128 || value.len() > 1024 { return Err(BehaviorError(ErrorCode::InvalidArgument)); }
        #[cfg(target_arch = "wasm32")]
        return crate::result::check(ErrorCode::from_u32(unsafe {
            crate::__rt::idtx_widget_set_value(name.as_ptr(), name.len() as u32, value.as_ptr(), value.len() as u32)
        }));
        #[cfg(not(target_arch = "wasm32"))]
        Ok(())
    }

    /// Read this behavior's transient field. Native stubs return an empty value.
    pub fn value(self, name: &str) -> Result<ArrayString<1024>> {
        if name.is_empty() || name.len() > 128 { return Err(BehaviorError(ErrorCode::InvalidArgument)); }
        #[cfg(target_arch = "wasm32")]
        {
            let mut buf = [0u8; 1024];
            let len = unsafe { crate::__rt::idtx_widget_get_value(name.as_ptr(), name.len() as u32, buf.as_mut_ptr(), 1024) } as usize;
            let bytes = buf.get(..len).ok_or(BehaviorError(ErrorCode::InvalidArgument))?;
            let text = core::str::from_utf8(bytes).map_err(|_| BehaviorError(ErrorCode::InvalidArgument))?;
            ArrayString::from(text).map_err(|_| BehaviorError(ErrorCode::InvalidArgument))
        }
        #[cfg(not(target_arch = "wasm32"))]
        Ok(ArrayString::new())
    }

    #[must_use]
    pub(crate) fn current() -> Self {
        Self { _private: () }
    }

    /// Register a click handler for the panel-relative child `path`
    /// (e.g. `"StartButton"` / `"Footer/QuitButton"`) on the panel this behavior
    /// is attached to. The handler fires (via the generated dispatcher) whenever
    /// a pointer behavior clicks that element. Call in `on_start`.
    ///
    /// # Errors
    /// Returns [`BehaviorError`] when the guest handler registry is full or the
    /// host rejects the registration (requires the `widget` capability).
    pub fn on_click<B: WidgetHandlerRegistry>(self, path: &str, handler: WidgetHandler<B>) -> Result<()> {
        // Guest side: remember the fn to dispatch to.
        B::__widget_registry().register(path, WidgetEventKind::Click, handler)?;
        // Host side: register so the host reports `consumed` synchronously and
        // only dispatches events that have a handler. NULL panel = self.
        #[cfg(target_arch = "wasm32")]
        {
            let code = ErrorCode::from_u32(unsafe {
                crate::__rt::idtx_widget_register_handler(
                    0, // NULL → the caller's own panel entity
                    path.as_ptr(),
                    u32::try_from(path.len()).unwrap_or(u32::MAX),
                    WidgetEventKind::Click.as_u32(),
                )
            });
            crate::result::check(code)?;
        }
        Ok(())
    }
}

/// Pointer-side widget surface, returned by [`crate::ctx::Ctx::widgets`]. Hands
/// a widget raycast hit to the host's built-in interaction processing.
#[derive(Clone, Copy, Debug)]
pub struct Widgets {
    _private: (),
}

impl Widgets {
    /// Shared runtime/preview navigation. `step=0` selects by stick (Y up), with
    /// the container's authored dead zone/hysteresis; +/-1 cycles for keyboard.
    /// The returned stable child path can be highlighted and explicitly activated.
    pub fn navigate(self, panel: crate::entity::Entity, container: &str, previous: &str,
        stick: [f32; 2], step: i32) -> ArrayString<MAX_PATH_LEN> {
        #[cfg(target_arch = "wasm32")]
        {
            let mut buf = [0u8; MAX_PATH_LEN];
            let len = unsafe { crate::__rt::idtx_widget_navigation(panel.wire().to_bits(), container.as_ptr(), container.len() as u32,
                previous.as_ptr(), previous.len() as u32, stick[0], stick[1], step, buf.as_mut_ptr(), MAX_PATH_LEN as u32) } as usize;
            core::str::from_utf8(buf.get(..len).unwrap_or_default()).ok().and_then(|s| ArrayString::from(s).ok()).unwrap_or_default()
        }
        #[cfg(not(target_arch = "wasm32"))]
        { let _ = (panel, container, previous, stick, step); ArrayString::new() }
    }

    /// Transient highlight, independent of the cached panel raster. Empty clears.
    pub fn highlight(self, panel: crate::entity::Entity, path: &str) -> Result<()> {
        #[cfg(target_arch = "wasm32")]
        return crate::result::check(ErrorCode::from_u32(unsafe { crate::__rt::idtx_widget_highlight(panel.wire().to_bits(), path.as_ptr(), path.len() as u32) }));
        #[cfg(not(target_arch = "wasm32"))]
        { let _ = (panel, path); Ok(()) }
    }
    /// Pointer-aware processing. `occluded` blocks world interaction regardless
    /// of whether a callback ran; `handled` reports actual event dispatch.
    pub fn handle_from(self, hit: RayHit, source: crate::abi::PointerSource, click: crate::input::Action) -> WidgetInteractionResult {
        if !hit.is_widget() { return WidgetInteractionResult::default(); }
        #[cfg(target_arch = "wasm32")]
        {
            let p = hit.position;
            let flags = unsafe { crate::__rt::idtx_widget_handle_from(hit.entity.wire().to_bits(), p.x, p.y, p.z, source as u32, click.id().0) };
            WidgetInteractionResult { occluded: flags & crate::abi::WIDGET_OCCLUDED != 0, handled: flags & crate::abi::WIDGET_HANDLED != 0 }
        }
        #[cfg(not(target_arch = "wasm32"))]
        { let _ = (source, click); WidgetInteractionResult::default() }
    }

    /// Activate a named button on an explicit confirmation edge, after the host
    /// validates current panel visibility, membership, clipping and enabled state.
    pub fn activate(self, panel: crate::entity::Entity, path: &str, source: crate::abi::PointerSource, confirm: crate::input::Action) -> bool {
        #[cfg(target_arch = "wasm32")]
        return unsafe { crate::__rt::idtx_widget_activate(panel.wire().to_bits(), path.as_ptr(), path.len() as u32, source as u32, confirm.id().0) != 0 };
        #[cfg(not(target_arch = "wasm32"))]
        { let _ = (panel, path, source, confirm); false }
    }
    #[must_use]
    pub(crate) fn current() -> Self {
        Self { _private: () }
    }

    /// Hand a widget [`RayHit`] (from a [`block_widgets`](crate::scene::RayOpts::block_widgets)
    /// cast) to host-side widget processing: it always draws the hit-point cue,
    /// and — when a `widgets.*` action edge fired on an element with a registered
    /// handler — dispatches that event to the panel behavior this frame.
    ///
    /// Returns `true` when the widget **occludes** the pointer, even without a
    /// child handler (so the pointer skips world interaction). Use `handle_from`
    /// for separate occlusion/handling flags and explicit identity. Passing a non-widget
    /// hit is a no-op returning `false`.
    #[must_use]
    pub fn handle(self, hit: RayHit) -> bool {
        if !hit.is_widget() {
            return false;
        }
        #[cfg(target_arch = "wasm32")]
        {
            let p = hit.position;
            unsafe { crate::__rt::idtx_widget_handle(hit.entity.wire().to_bits(), p.x, p.y, p.z) != 0 }
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            false
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WidgetInteractionResult {
    pub occluded: bool,
    pub handled: bool,
}
