//! Proc-macro companion for `idtx-behavior-sdk`.
//!
//! Provides the [`macro@behavior`] attribute macro applied to a
//! behavior's `impl` block. It generates the `extern "C"` lifecycle
//! thunks, the `_idtx_abi_version` re-export, and a `_idtx_behavior_desc`
//! descriptor export the host validates against the on-disk manifest.
//!
//! Proc-macros cannot be `#![no_std]` cdylibs, hence this lives in its own
//! crate and is re-exported as `idtx_behavior_sdk::behavior`.
//!
//! # Scope
//!
//! The generated thunks call into the SDK runtime glue
//! (`idtx_behavior_sdk::__rt`). On `wasm32` they are `#[export_name]`
//! symbols the host discovers and dispatches; natively they are absent so
//! author unit tests can drive the behavior directly.

use proc_macro::TokenStream;
use quote::quote;
use syn::{Ident, ItemImpl, LitStr, parse::Parse, parse::ParseStream, parse_macro_input};

/// Parsed `#[behavior(...)]` attribute arguments.
///
/// Currently accepts `kind = "runtime" | "editor"`. Unknown keys are a
/// compile error so a typo doesn't silently pick a default.
struct BehaviorArgs {
    kind: BehaviorKind,
}

#[derive(Clone, Copy)]
enum BehaviorKind {
    Runtime,
    Editor,
}

impl BehaviorKind {
    fn as_token(self) -> &'static str {
        match self {
            Self::Runtime => "runtime",
            Self::Editor => "editor",
        }
    }
}

impl Default for BehaviorArgs {
    fn default() -> Self {
        Self {
            kind: BehaviorKind::Runtime,
        }
    }
}

impl Parse for BehaviorArgs {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let mut args = Self::default();
        while !input.is_empty() {
            let key: Ident = input.parse()?;
            input.parse::<syn::Token![=]>()?;
            let value: LitStr = input.parse()?;
            match key.to_string().as_str() {
                "kind" => {
                    args.kind = match value.value().as_str() {
                        "runtime" => BehaviorKind::Runtime,
                        "editor" => BehaviorKind::Editor,
                        other => {
                            return Err(syn::Error::new_spanned(
                                &value,
                                format!(
                                    "unknown behavior kind `{other}` (expected \"runtime\" or \"editor\")"
                                ),
                            ));
                        }
                    };
                }
                other => {
                    return Err(syn::Error::new_spanned(
                        &key,
                        format!("unknown #[behavior] argument `{other}` (expected `kind`)"),
                    ));
                }
            }
            // Optional trailing comma between args.
            if input.peek(syn::Token![,]) {
                input.parse::<syn::Token![,]>()?;
            }
        }
        Ok(args)
    }
}

/// The lifecycle hooks the macro knows how to thunk. Only hooks actually
/// present in the `impl` get a generated `extern "C"` wrapper.
const LIFECYCLE_HOOKS: &[&str] = &[
    "on_attach",
    "on_start",
    "on_update",
    "on_late_update",
    "on_editor_update",
    "on_interact",
    "on_event",
    "on_stop",
    "on_detach",
    "on_destroy",
    "on_stage_event",
    "on_input",
];

/// The `#[behavior(kind = "...")]` attribute macro.
///
/// Applied to an `impl MyBehavior { ... }` block. Generates the guest
/// export surface the host expects.
#[proc_macro_attribute]
pub fn behavior(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = if attr.is_empty() {
        BehaviorArgs::default()
    } else {
        parse_macro_input!(attr as BehaviorArgs)
    };
    let input = parse_macro_input!(item as ItemImpl);

    // The behavior type the impl targets — used to name the singleton and
    // reach its methods.
    let self_ty = &input.self_ty;

    // Collect the set of implemented lifecycle hook names.
    let mut present: Vec<String> = Vec::new();
    for item in &input.items {
        if let syn::ImplItem::Fn(m) = item {
            let name = m.sig.ident.to_string();
            if LIFECYCLE_HOOKS.contains(&name.as_str()) {
                present.push(name);
            }
        }
    }

    let kind_token = args.kind.as_token();

    // Generate an `extern "C"` thunk per implemented hook. The thunks
    // route through the SDK runtime glue, which constructs the behavior
    // singleton + `Ctx` and invokes the author method.
    let mut thunks = proc_macro2::TokenStream::new();
    for hook in &present {
        // `on_event` is handled specially below: the generated `_idtx_on_event`
        // is ALWAYS emitted (even without an author `on_event`) so a host-
        // dispatched widget click can reach the per-behavior widget-handler
        // registry. Skip the generic thunk here.
        if hook == "on_event" {
            continue;
        }
        let hook_ident = Ident::new(hook, proc_macro2::Span::call_site());
        let export = format!("_idtx_{hook}");
        let export_lit = LitStr::new(&export, proc_macro2::Span::call_site());
        // `on_update` / `on_late_update` / `on_editor_update` take a dt.
        let takes_dt = matches!(
            hook.as_str(),
            "on_update" | "on_late_update" | "on_editor_update"
        );
        // `on_stage_event` takes a `u32` stage-event code (decoded to a
        // `StageEvent` by the SDK glue before the author method runs).
        let takes_stage_event = hook.as_str() == "on_stage_event";
        // `on_input` takes `(action_id: u32, phase: u32)` (decoded to
        // `InputActionId` + `InputPhase` by the SDK glue).
        let takes_input = hook.as_str() == "on_input";
        let thunk = if takes_stage_event {
            quote! {
                #[cfg(target_arch = "wasm32")]
                #[unsafe(export_name = #export_lit)]
                pub extern "C" fn #hook_ident(code: u32) {
                    ::idtx_behavior_sdk::__rt::dispatch_u32(
                        code,
                        |ctx, event| {
                            __IDTX_INSTANCE.with(
                                <#self_ty as ::core::default::Default>::default,
                                |inst| <#self_ty>::#hook_ident(inst, ctx, event),
                            )
                        },
                    );
                }
            }
        } else if takes_input {
            quote! {
                #[cfg(target_arch = "wasm32")]
                #[unsafe(export_name = #export_lit)]
                pub extern "C" fn #hook_ident(action_id: u32, phase: u32) {
                    ::idtx_behavior_sdk::__rt::dispatch_input(
                        action_id,
                        phase,
                        |ctx, action, phase| {
                            __IDTX_INSTANCE.with(
                                <#self_ty as ::core::default::Default>::default,
                                |inst| <#self_ty>::#hook_ident(inst, ctx, action, phase),
                            )
                        },
                    );
                }
            }
        } else if takes_dt {
            quote! {
                #[cfg(target_arch = "wasm32")]
                #[unsafe(export_name = #export_lit)]
                pub extern "C" fn #hook_ident(dt: f32) {
                    ::idtx_behavior_sdk::__rt::dispatch_dt(
                        |ctx| {
                            __IDTX_INSTANCE.with(
                                <#self_ty as ::core::default::Default>::default,
                                |inst| <#self_ty>::#hook_ident(inst, ctx, dt),
                            )
                        },
                    );
                }
            }
        } else {
            quote! {
                #[cfg(target_arch = "wasm32")]
                #[unsafe(export_name = #export_lit)]
                pub extern "C" fn #hook_ident() {
                    ::idtx_behavior_sdk::__rt::dispatch(
                        |ctx| {
                            __IDTX_INSTANCE.with(
                                <#self_ty as ::core::default::Default>::default,
                                |inst| <#self_ty>::#hook_ident(inst, ctx),
                            )
                        },
                    );
                }
            }
        };
        thunks.extend(thunk);
    }

    // Widget-click delivery (ABI 3) reuses the `_idtx_on_event` seam. When the
    // author implements `on_event`, non-widget events fall through to it.
    let has_on_event = present.iter().any(|h| h == "on_event");
    let on_event_fallback = if has_on_event {
        quote! {
            __IDTX_INSTANCE.with(
                <#self_ty as ::core::default::Default>::default,
                |inst| <#self_ty>::on_event(inst, ctx),
            )
        }
    } else {
        quote! { ::core::result::Result::Ok(()) }
    };

    let expanded = quote! {
        // The user's impl block, untouched.
        #input

        // ABI version re-export the host validates against.
        #[cfg(target_arch = "wasm32")]
        #[unsafe(export_name = "_idtx_abi_version")]
        pub extern "C" fn __idtx_abi_version() -> u32 {
            ::idtx_behavior_sdk::abi::ABI_VERSION
        }

        // The module's single behavior instance, constructed lazily on the
        // first hook and kept in linear memory across every subsequent hook /
        // frame (the host instantiates the module once per attachment). Dropped
        // by `_idtx_drop_instance` after the terminal lifecycle hook.
        #[cfg(target_arch = "wasm32")]
        static __IDTX_INSTANCE: ::idtx_behavior_sdk::__rt::BehaviorSlot<#self_ty> =
            ::idtx_behavior_sdk::__rt::BehaviorSlot::new();

        // Terminal teardown: the host calls this after the last lifecycle hook
        // (`on_detach` / `on_destroy`) — i.e. the owning prim was removed /
        // deactivated or the app is tearing down — to drop the instance (running
        // its destructor) while the store + host imports are still valid.
        #[cfg(target_arch = "wasm32")]
        #[unsafe(export_name = "_idtx_drop_instance")]
        pub extern "C" fn __idtx_drop_instance() {
            __IDTX_INSTANCE.clear();
        }

        // Per-behavior async-HTTP handler registry (`net` capability). Always
        // emitted: it is a small inline table that stays empty — and the export
        // below a silent no-op — for behaviors that never call
        // `net::request_async` / `net::post_json_async`.
        #[cfg(target_arch = "wasm32")]
        static __IDTX_HTTP_HANDLERS: ::idtx_behavior_sdk::net::HandlerSlot<#self_ty> =
            ::idtx_behavior_sdk::net::HandlerSlot::new();

        #[cfg(target_arch = "wasm32")]
        impl ::idtx_behavior_sdk::net::HttpHandlerRegistry for #self_ty {
            fn __registry() -> &'static ::idtx_behavior_sdk::net::HandlerSlot<Self> {
                &__IDTX_HTTP_HANDLERS
            }
        }

        // Async-HTTP completion dispatcher: the host calls this once per completed
        // `request_async` / `post_json_async`, passing the guest's correlation
        // `request_id` and the HTTP `status`. We look up the one-shot handler the
        // author registered for that id and invoke it against the behavior
        // instance (missing id ⇒ silent no-op).
        #[cfg(target_arch = "wasm32")]
        #[unsafe(export_name = "_idtx_on_http_response")]
        pub extern "C" fn __idtx_on_http_response(request_id: u32, status: u32) {
            if let ::core::option::Option::Some(handler) =
                <#self_ty as ::idtx_behavior_sdk::net::HttpHandlerRegistry>::__registry()
                    .take(request_id)
            {
                let resp = ::idtx_behavior_sdk::net::HttpResponse::current(status);
                let ctx = ::idtx_behavior_sdk::ctx::Ctx::current();
                __IDTX_INSTANCE.with(
                    <#self_ty as ::core::default::Default>::default,
                    |inst| {
                        let _ = handler(inst, &ctx, resp);
                    },
                );
            }
        }

        // Per-behavior widget-handler registry (`widget` capability). Always
        // emitted: the table stays empty (and the widget branch of the dispatcher
        // a no-op) for behaviors that never call `ctx.widget().on_click`.
        #[cfg(target_arch = "wasm32")]
        static __IDTX_WIDGET_HANDLERS: ::idtx_behavior_sdk::scene::WidgetHandlerSlot<#self_ty> =
            ::idtx_behavior_sdk::scene::WidgetHandlerSlot::new();

        #[cfg(target_arch = "wasm32")]
        impl ::idtx_behavior_sdk::scene::WidgetHandlerRegistry for #self_ty {
            fn __widget_registry() -> &'static ::idtx_behavior_sdk::scene::WidgetHandlerSlot<Self> {
                &__IDTX_WIDGET_HANDLERS
            }
        }

        // Event dispatcher — ALWAYS emitted. The host delivers a widget click
        // through this same `_idtx_on_event` seam with a reserved name; here we
        // route a widget event to the registered `fn` handler (by element path +
        // kind), and any other event to the author's `on_event` (when present).
        #[cfg(target_arch = "wasm32")]
        #[unsafe(export_name = "_idtx_on_event")]
        pub extern "C" fn __idtx_on_event() {
            ::idtx_behavior_sdk::__rt::dispatch(|ctx| {
                let kind_raw = ::idtx_behavior_sdk::scene::widget::current_kind_raw();
                if kind_raw != ::idtx_behavior_sdk::abi::WIDGET_EVENT_NONE {
                    let click = ::idtx_behavior_sdk::scene::WidgetClick::current();
                    let kind = ::idtx_behavior_sdk::abi::WidgetEventKind::from_u32(kind_raw);
                    let elem = click.element();
                    if let ::core::option::Option::Some(handler) =
                        <#self_ty as ::idtx_behavior_sdk::scene::WidgetHandlerRegistry>::__widget_registry()
                            .get(elem.as_str(), kind)
                    {
                        return __IDTX_INSTANCE.with(
                            <#self_ty as ::core::default::Default>::default,
                            |inst| handler(inst, ctx, &click),
                        );
                    }
                    ::core::result::Result::Ok(())
                } else {
                    #on_event_fallback
                }
            });
        }

        // Static descriptor string the host reads for manifest parity.
        // A minimal `key=value;` form the host parses without serde.
        #[cfg(target_arch = "wasm32")]
        static __IDTX_BEHAVIOR_DESC: &str = concat!(
            "kind=", #kind_token, ";",
        );

        // Return a (ptr, len) into the descriptor. The host reads `len`
        // bytes at `ptr` from guest memory to recover the descriptor.
        #[cfg(target_arch = "wasm32")]
        #[unsafe(export_name = "_idtx_behavior_desc")]
        pub extern "C" fn __idtx_behavior_desc() -> u64 {
            let bytes = __IDTX_BEHAVIOR_DESC.as_bytes();
            let ptr = bytes.as_ptr() as u64;
            let len = bytes.len() as u64;
            (ptr << 32) | len
        }

        // Panic hook installer: a panicking behavior traps (aborts) so
        // the host never observes a partially-mutated store. Installed
        // lazily on first dispatch by the SDK runtime glue.
        #[cfg(target_arch = "wasm32")]
        #[unsafe(export_name = "_idtx_install_panic_hook")]
        pub extern "C" fn __idtx_install_panic_hook() {
            ::idtx_behavior_sdk::__rt::install_panic_hook();
        }

        #thunks
    };

    expanded.into()
}
