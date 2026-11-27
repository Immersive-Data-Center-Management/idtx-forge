//! HTTP egress (`net` capability).
//!
//! A behavior granted `net` may perform HTTP requests. Every request is
//! validated against the host's deny-by-default SSRF policy — the **same**
//! `idtx-net-guard` gate that guards `OpenExec` REST datasources (host allowlist,
//! `http`/`https` only, public-address resolution, redirect-disabled + DNS-pinned
//! client). Without a configured `[network]` allowlist a request is denied even
//! when the capability is granted, so the capability is off by default (granted
//! per behavior in the inspector's Manifest config).
//!
//! # Prefer the asynchronous API
//!
//! [`request_async`] / [`post_json_async`] are **non-blocking**: they hand the
//! request to the host and return immediately, and the host delivers the result
//! on a later frame by invoking the `fn` **handler** you passed at the call site.
//! Use these for anything that reaches a real network — the request runs on a
//! background worker, so the behavior hook never stalls.
//!
//! The older [`request`] / [`post_json`] are **blocking**: they stall the calling
//! hook for the whole round-trip, which trips the host's per-hook wall-clock
//! budget (`Trap::Interrupt`) and skips the tick. They are retained only for
//! short/native-test use; do not use them against a live endpoint.
//!
//! On wasm these route to the capability-gated host imports (absent — and
//! therefore link-failing — without the grant); natively they are inert, so a
//! behavior using them stays unit-testable without a host.
//!
//! # Per-request handlers
//!
//! The handler is the correlation: you never manage request ids. Each async call
//! registers your `fn(&mut Self, &Ctx, HttpResponse) -> Result<()>` against an
//! internal id; when the response arrives the SDK routes it to exactly that
//! handler. Different endpoints (or the same endpoint from different states) each
//! get their own handler, and a handler may fire the next request:
//!
//! ```ignore
//! fn on_stage_event(&mut self, _ctx: &Ctx, ev: StageEvent) -> Result<()> {
//!     if ev == StageEvent::PlayStart {
//!         net::post_json_async(LOGIN_URL, LOGIN_BODY, Self::on_login)?; // returns at once
//!     }
//!     Ok(())
//! }
//! fn on_login(&mut self, _ctx: &Ctx, resp: net::HttpResponse) -> Result<()> {
//!     if !resp.is_success() { return Ok(()); }
//!     let mut buf = [0u8; 4096];
//!     let n = resp.read_body(&mut buf);
//!     // … parse, store, maybe fire the next request with its own handler …
//!     Ok(())
//! }
//! ```
//!
//! A handler is a bare `fn` (the guest has no allocator, so it cannot capture an
//! environment) but it receives `&mut self`, so it reads and writes the
//! behavior's own fields — enough to drive a multi-step state machine.
//!
//! # Buffers, not allocation
//!
//! The SDK is `no_std` with no allocator, so a response body is read into a
//! caller-owned `&mut [u8]`; the read returns the body's *true* length (which may
//! exceed the buffer when truncated). This mirrors the raycast path-output and
//! event-payload conventions elsewhere in the SDK.

use core::cell::RefCell;
#[cfg(target_arch = "wasm32")]
use core::sync::atomic::{AtomicU32, Ordering};

use arrayvec::ArrayVec;

use crate::abi::ErrorCode;
use crate::ctx::Ctx;
use crate::result::{check, BehaviorError, Result};

/// Content-type sent by [`post_json`] / [`post_json_async`].
const JSON_CONTENT_TYPE: &str = "application/json";

/// Max outstanding async handlers a single behavior may have registered at once
/// (bounds the guest-side registry; the host enforces its own in-flight cap).
const MAX_HANDLERS: usize = 16;

// ===========================================================================
// Asynchronous (non-blocking) egress — the preferred path.
// ===========================================================================

/// A completed HTTP response delivered to an async handler.
///
/// A zero-state handle, valid only for the duration of the handler call: it reads
/// the body back from the host on demand via [`read_body`](Self::read_body). The
/// status is carried by value. A `status` of `0` means the request never
/// completed (transport failure) or was denied by the SSRF policy.
#[derive(Clone, Copy, Debug)]
pub struct HttpResponse {
    status: u16,
}

impl HttpResponse {
    /// Build the handle for the current callback. Hidden — only the generated
    /// `_idtx_on_http_response` dispatcher calls this.
    #[doc(hidden)]
    #[must_use]
    pub fn current(status_code: u32) -> Self {
        Self {
            status: u16::try_from(status_code).unwrap_or(u16::MAX),
        }
    }

    /// The HTTP status code (e.g. `200`), or `0` on a transport failure / policy
    /// denial.
    #[must_use]
    pub const fn status(&self) -> u16 {
        self.status
    }

    /// `true` when the status is in the `2xx` success range.
    #[must_use]
    pub const fn is_success(&self) -> bool {
        self.status >= 200 && self.status < 300
    }

    /// Copy the response body into `buf`, returning the body's *true* byte length.
    /// At most `buf.len()` bytes are written; a return value greater than
    /// `buf.len()` means the body was truncated (retry with a larger buffer if you
    /// need all of it).
    #[must_use]
    pub fn read_body(&self, buf: &mut [u8]) -> usize {
        #[cfg(target_arch = "wasm32")]
        {
            let cap = u32::try_from(buf.len()).unwrap_or(u32::MAX);
            unsafe { crate::__rt::idtx_http_response_body(buf.as_mut_ptr(), cap) as usize }
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = buf;
            0
        }
    }
}

/// An async response handler: a bare `fn` taking the behavior by `&mut`, the hook
/// context, and the completed [`HttpResponse`]. Registered at the call site of
/// [`request_async`] / [`post_json_async`] and invoked on a later frame when the
/// response arrives.
pub type HttpHandler<B> = fn(&mut B, &Ctx, HttpResponse) -> Result<()>;

/// A fixed-capacity, single-threaded registry mapping an internal request id to
/// the handler the author passed. One instance lives per behavior type (generated
/// by the `#[behavior]` macro) so [`request_async`] can reach it generically
/// through [`HttpHandlerRegistry`].
///
/// No allocator: the table is an inline [`ArrayVec`] of `MAX_HANDLERS` entries.
/// Entries are one-shot — removed when their completion is delivered — so the
/// capacity bounds *outstanding* (not total) requests.
pub struct HandlerSlot<B> {
    cell: RefCell<ArrayVec<(u32, HttpHandler<B>), MAX_HANDLERS>>,
}

// SAFETY: identical reasoning to `__rt::BehaviorSlot` — a non-threaded WASM guest
// is single-threaded, so the `RefCell` is never accessed concurrently. The impl
// exists only so the slot can back a `static`, and that static is generated only
// on non-threaded `wasm32`. Withheld elsewhere (native, threaded wasm), where
// `RefCell`'s own `!Sync` keeps the type honest.
#[cfg(all(target_arch = "wasm32", not(target_feature = "atomics")))]
unsafe impl<B> Sync for HandlerSlot<B> {}

impl<B> HandlerSlot<B> {
    /// A fresh, empty registry (`const` so it can initialise a `static`).
    #[must_use]
    pub const fn new() -> Self {
        Self {
            cell: RefCell::new(ArrayVec::new_const()),
        }
    }

    /// Register `handler` under `id`. Returns [`ErrorCode::InvalidArgument`] when
    /// the registry is full (too many requests outstanding).
    ///
    /// # Errors
    /// Full registry.
    pub fn register(&self, id: u32, handler: HttpHandler<B>) -> Result<()> {
        self.cell
            .borrow_mut()
            .try_push((id, handler))
            .map_err(|_| BehaviorError(ErrorCode::InvalidArgument))
    }

    /// Remove and return the handler registered under `id` (one-shot), or `None`
    /// when no handler matches (e.g. the request was rejected, or already
    /// delivered).
    #[must_use]
    pub fn take(&self, id: u32) -> Option<HttpHandler<B>> {
        let mut v = self.cell.borrow_mut();
        let idx = v.iter().position(|(rid, _)| *rid == id)?;
        Some(v.swap_remove(idx).1)
    }
}

impl<B> Default for HandlerSlot<B> {
    fn default() -> Self {
        Self::new()
    }
}

/// Implemented (by the `#[behavior]` macro) for a behavior type so the free
/// [`request_async`] / [`post_json_async`] functions can reach that type's
/// per-behavior [`HandlerSlot`]. Hidden plumbing — authors never name it.
#[doc(hidden)]
pub trait HttpHandlerRegistry: Sized + 'static {
    /// The behavior type's handler registry (a generated `static`).
    fn __registry() -> &'static HandlerSlot<Self>;
}

/// Monotonic source of internal request-correlation ids. `0` is skipped so a
/// never-registered id is distinguishable if ever needed. Only the wasm async
/// path allocates ids; the native stubs are inert.
#[cfg(target_arch = "wasm32")]
static NEXT_REQUEST_ID: AtomicU32 = AtomicU32::new(1);

/// Allocate the next internal request id.
#[cfg(target_arch = "wasm32")]
fn next_request_id() -> u32 {
    NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed)
}

/// Perform a non-blocking HTTP request. Registers `handler` and returns
/// immediately; `handler` fires on a later frame when the response (or a
/// transport failure, as a `0`-status [`HttpResponse`]) arrives. An empty
/// `content_type` sends no `Content-Type` header; an empty `body` sends no body.
///
/// # Errors
///
/// Returns [`crate::result::BehaviorError`] when the guest handler registry is
/// full ([`ErrorCode::InvalidArgument`]) or the host rejects the request up front
/// (malformed method, or too many requests already in flight). A transport-level
/// failure or a non-2xx HTTP status is **not** an error here — both surface
/// through the handler's [`HttpResponse`].
#[cfg(target_arch = "wasm32")]
pub fn request_async<B: HttpHandlerRegistry>(
    method: &str,
    url: &str,
    content_type: &str,
    body: &[u8],
    handler: HttpHandler<B>,
) -> Result<()> {
    let id = next_request_id();
    B::__registry().register(id, handler)?;
    let code = ErrorCode::from_u32(unsafe {
        crate::__rt::idtx_http_request_async(
            id,
            method.as_ptr(),
            u32::try_from(method.len()).unwrap_or(u32::MAX),
            url.as_ptr(),
            u32::try_from(url.len()).unwrap_or(u32::MAX),
            content_type.as_ptr(),
            u32::try_from(content_type.len()).unwrap_or(u32::MAX),
            body.as_ptr(),
            u32::try_from(body.len()).unwrap_or(u32::MAX),
        )
    });
    // The host rejected it synchronously → no completion will ever arrive for
    // `id`, so drop the handler we just registered instead of leaking a slot.
    if let Err(e) = check(code) {
        let _ = B::__registry().take(id);
        return Err(e);
    }
    Ok(())
}

/// Native stub: inert (no host), so author unit tests build without a wasm host.
/// No `HttpHandlerRegistry` bound — the registry/trait exist only on wasm.
#[cfg(not(target_arch = "wasm32"))]
pub fn request_async<B>(
    method: &str,
    url: &str,
    content_type: &str,
    body: &[u8],
    handler: HttpHandler<B>,
) -> Result<()> {
    let _ = (method, url, content_type, body, handler);
    Ok(())
}

/// Convenience: non-blocking `POST` of a JSON body
/// (`Content-Type: application/json`). See [`request_async`].
///
/// # Errors
/// See [`request_async`].
#[cfg(target_arch = "wasm32")]
pub fn post_json_async<B: HttpHandlerRegistry>(
    url: &str,
    json_body: &str,
    handler: HttpHandler<B>,
) -> Result<()> {
    request_async(
        "POST",
        url,
        JSON_CONTENT_TYPE,
        json_body.as_bytes(),
        handler,
    )
}

/// Native stub for [`post_json_async`] — inert.
#[cfg(not(target_arch = "wasm32"))]
pub fn post_json_async<B>(url: &str, json_body: &str, handler: HttpHandler<B>) -> Result<()> {
    let _ = (url, json_body, handler);
    Ok(())
}

// ===========================================================================
// Synchronous (blocking) egress — retained for compatibility; DISCOURAGED.
// ===========================================================================

/// A completed HTTP response from the **blocking** [`request`] / [`post_json`].
/// `body` borrows the written prefix of the caller's buffer; `body_len` is the
/// response's true length (which may exceed `body`'s length when the buffer was
/// too small).
#[derive(Debug)]
pub struct BlockingResponse<'a> {
    /// The HTTP status code (e.g. `200`). `0` indicates the request never
    /// completed at the transport level — but that case surfaces as an `Err` from
    /// [`request`], so a returned `BlockingResponse` always carries a real status.
    pub status: u16,
    /// The response body, truncated to the caller buffer's capacity.
    pub body: &'a [u8],
    /// The response body's true length in bytes. `body_len > body.len()` means the
    /// body was truncated.
    pub body_len: usize,
}

impl BlockingResponse<'_> {
    /// `true` when the status is in the `2xx` success range.
    #[must_use]
    pub const fn is_success(&self) -> bool {
        self.status >= 200 && self.status < 300
    }

    /// `true` when the body was larger than the supplied buffer and therefore
    /// truncated.
    #[must_use]
    pub const fn is_truncated(&self) -> bool {
        self.body_len > self.body.len()
    }
}

/// Legacy blocking HTTP ABI. The Studio behavior host rejects this operation
/// with [`ErrorCode::CapabilityDenied`] because its guest-execution budgets
/// cannot preempt blocking host I/O. Use [`request_async`] instead. The native
/// (non-WASM) author-test stub remains inert.
///
/// # Errors
///
/// Returns [`crate::result::BehaviorError`] carrying
/// [`ErrorCode::CapabilityDenied`] from Studio's WASM host. Asynchronous requests
/// report transport failures and HTTP status through their callback instead.
pub fn request<'a>(
    method: &str,
    url: &str,
    content_type: &str,
    body: &[u8],
    resp_buf: &'a mut [u8],
) -> Result<BlockingResponse<'a>> {
    let mut result = [0u8; core::mem::size_of::<crate::abi::HttpResult>()];
    let cap = u32::try_from(resp_buf.len()).unwrap_or(u32::MAX);

    #[cfg(target_arch = "wasm32")]
    let code = crate::abi::ErrorCode::from_u32(unsafe {
        crate::__rt::idtx_http_request(
            method.as_ptr(),
            u32::try_from(method.len()).unwrap_or(u32::MAX),
            url.as_ptr(),
            u32::try_from(url.len()).unwrap_or(u32::MAX),
            content_type.as_ptr(),
            u32::try_from(content_type.len()).unwrap_or(u32::MAX),
            body.as_ptr(),
            u32::try_from(body.len()).unwrap_or(u32::MAX),
            resp_buf.as_mut_ptr(),
            cap,
            result.as_mut_ptr(),
        )
    });
    #[cfg(not(target_arch = "wasm32"))]
    let code = {
        // Inert natively: no host, so report a completed-but-empty 0-status
        // response. Author unit tests build and run without a wasm host.
        let _ = (method, url, content_type, body, cap);
        result.copy_from_slice(bytemuck::bytes_of(&crate::abi::HttpResult::default()));
        crate::abi::ErrorCode::Ok
    };

    check(code)?;

    let meta: crate::abi::HttpResult = bytemuck::pod_read_unaligned(&result);
    let body_len = meta.body_len as usize;
    let written = body_len.min(resp_buf.len());
    Ok(BlockingResponse {
        status: meta.status,
        body: &resp_buf[..written],
        body_len,
    })
}

/// Legacy blocking `POST` wrapper; Studio's host rejects it. Use [`post_json_async`].
///
/// # Errors
///
/// See [`request`].
pub fn post_json<'a>(
    url: &str,
    json_body: &str,
    resp_buf: &'a mut [u8],
) -> Result<BlockingResponse<'a>> {
    request("POST", url, JSON_CONTENT_TYPE, json_body.as_bytes(), resp_buf)
}

// ===========================================================================
// Response parsing helper (shared by both paths).
// ===========================================================================

/// Extract the string value of the first `"key": "value"` pair found in a JSON
/// document.
///
/// This is a **minimal, dependency-free** scanner, not a full JSON parser: it
/// finds the literal `"<key>"`, skips the following colon and whitespace, and
/// reads the next double-quoted string, honouring `\"` and `\\` escapes but
/// returning the *raw* (still-escaped) slice. It is adequate for simple,
/// well-formed token responses (e.g. an `OAuth2` `access_token`); it does not
/// validate structure, respect nesting, or distinguish a key from a same-named
/// string value. Returns [`None`] when the key or a following string is absent.
#[must_use]
pub fn json_string_field<'a>(json: &'a [u8], key: &str) -> Option<&'a str> {
    // The needle is the quoted key: `"key"`. Build it on the stack (no alloc).
    let kb = key.as_bytes();
    let mut i = 0usize;
    'scan: while i + kb.len() + 2 <= json.len() {
        if json[i] == b'"'
            && json[i + 1..].len() >= kb.len()
            && &json[i + 1..i + 1 + kb.len()] == kb
            && json.get(i + 1 + kb.len()) == Some(&b'"')
        {
            // Positioned at the closing quote of the key. Advance past it, then
            // past whitespace and a single ':' and more whitespace.
            let mut j = i + 1 + kb.len() + 1;
            while j < json.len() && json[j].is_ascii_whitespace() {
                j += 1;
            }
            if json.get(j) != Some(&b':') {
                i += 1;
                continue 'scan;
            }
            j += 1;
            while j < json.len() && json[j].is_ascii_whitespace() {
                j += 1;
            }
            // Expect the opening quote of the value.
            if json.get(j) != Some(&b'"') {
                i += 1;
                continue 'scan;
            }
            j += 1;
            let start = j;
            // Read until the unescaped closing quote.
            while j < json.len() {
                match json[j] {
                    b'\\' => j += 2, // skip the escaped byte
                    b'"' => return core::str::from_utf8(&json[start..j]).ok(),
                    _ => j += 1,
                }
            }
            return None; // unterminated string
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_access_token() {
        let body = br#"{"access_token":"abc.def.ghi","token_type":"Bearer","expires_in":3600}"#;
        assert_eq!(json_string_field(body, "access_token"), Some("abc.def.ghi"));
        assert_eq!(json_string_field(body, "token_type"), Some("Bearer"));
    }

    #[test]
    fn tolerates_whitespace_and_order() {
        let body = br#"{ "token_type" : "Bearer" ,  "access_token" :   "xyz" }"#;
        assert_eq!(json_string_field(body, "access_token"), Some("xyz"));
    }

    #[test]
    fn missing_or_non_string_is_none() {
        assert_eq!(json_string_field(br#"{"expires_in":3600}"#, "access_token"), None);
        // Value is a number, not a string → no quoted value to return.
        assert_eq!(json_string_field(br#"{"access_token":42}"#, "access_token"), None);
        assert_eq!(json_string_field(b"", "access_token"), None);
    }

    #[test]
    fn honours_escaped_quote_in_value() {
        // The scanner returns the raw (still-escaped) slice.
        let body = br#"{"scope":"a\"b"}"#;
        assert_eq!(json_string_field(body, "scope"), Some(r#"a\"b"#));
    }

    #[test]
    fn native_blocking_request_is_inert_zero_status() {
        let mut buf = [0u8; 64];
        let resp = request("POST", "https://example.invalid/x", "application/json", b"{}", &mut buf)
            .expect("native stub is Ok");
        assert_eq!(resp.status, 0);
        assert!(resp.body.is_empty());
        assert_eq!(resp.body_len, 0);
    }

    // A dummy behavior type standing in for an author's type, so the native
    // (no-bound) async stubs + the registry can be exercised without a wasm host.
    #[derive(Default)]
    struct Dummy {
        hits: u32,
    }

    // Signature is dictated by the `HttpHandler` fn-pointer alias (`&Ctx`,
    // `-> Result<()>`), so the by-ref / wrapping-return pedantic lints don't apply.
    #[allow(clippy::trivially_copy_pass_by_ref, clippy::unnecessary_wraps)]
    fn dummy_handler(d: &mut Dummy, _ctx: &Ctx, _resp: HttpResponse) -> Result<()> {
        d.hits += 1;
        Ok(())
    }

    #[test]
    fn native_async_request_is_inert_ok() {
        // No host, no registry on native: the call is an inert `Ok(())`.
        assert!(request_async("POST", "https://example.invalid/x", "", b"", dummy_handler).is_ok());
        assert!(post_json_async("https://example.invalid/x", "{}", dummy_handler).is_ok());
    }

    #[test]
    fn handler_registry_round_trips_and_is_one_shot() {
        let slot: HandlerSlot<Dummy> = HandlerSlot::new();
        slot.register(7, dummy_handler).expect("register");
        // Wrong id misses; correct id hits exactly once (one-shot removal).
        assert!(slot.take(99).is_none());
        let h = slot.take(7).expect("handler present");
        assert!(slot.take(7).is_none(), "one-shot: gone after take");
        let mut d = Dummy::default();
        let ctx = Ctx::current();
        h(&mut d, &ctx, HttpResponse::current(200)).expect("handler ok");
        assert_eq!(d.hits, 1);
    }

    #[test]
    fn handler_registry_rejects_when_full() {
        let slot: HandlerSlot<Dummy> = HandlerSlot::new();
        let cap = u32::try_from(MAX_HANDLERS).expect("capacity fits u32");
        for id in 0..cap {
            slot.register(id, dummy_handler).expect("within capacity");
        }
        // One past capacity is rejected rather than panicking.
        assert_eq!(
            slot.register(999, dummy_handler).unwrap_err().code(),
            ErrorCode::InvalidArgument
        );
    }

    #[test]
    fn http_response_status_helpers() {
        assert!(HttpResponse::current(204).is_success());
        assert!(!HttpResponse::current(0).is_success());
        assert!(!HttpResponse::current(500).is_success());
        assert_eq!(HttpResponse::current(200).status(), 200);
        // A status that overflows u16 clamps rather than truncating.
        assert_eq!(HttpResponse::current(u32::MAX).status(), u16::MAX);
    }
}
