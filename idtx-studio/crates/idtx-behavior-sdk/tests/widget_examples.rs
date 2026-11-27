//! Keep the authored, reusable widget source examples on the public SDK surface.
#![allow(dead_code)]
#[path = "../../../examples/widgets/menu.rs"]
mod menu;
#[path = "../../../examples/widgets/message.rs"]
mod message;

// The macro emits its single-threaded static registries only on wasm. Native
// compilation checks author code's types; these hooks are not invoked here.
#[cfg(not(target_arch = "wasm32"))]
impl idtx_behavior_sdk::scene::WidgetHandlerRegistry for menu::Menu {
    fn __widget_registry() -> &'static idtx_behavior_sdk::scene::WidgetHandlerSlot<Self> {
        unreachable!("guest registry exists only on wasm")
    }
}
#[cfg(not(target_arch = "wasm32"))]
impl idtx_behavior_sdk::scene::WidgetHandlerRegistry for message::Message {
    fn __widget_registry() -> &'static idtx_behavior_sdk::scene::WidgetHandlerSlot<Self> {
        unreachable!("guest registry exists only on wasm")
    }
}
