#pragma once
/**
 * @file env_provider_host.h
 * @brief C-ABI registration shim that lets a **host written in another language**
 *        (here: Rust) install an `idtx::IEnvironmentProvider` into the exec
 *        plugin's `EnvironmentProviderRegistry` without linking the plugin.
 *
 * The registry is a Meyers singleton compiled INTO this plugin DLL
 * (`EnvironmentProviderRegistry.cpp`), so only code inside the plugin can reach
 * the correct instance. The idtx-studio host is the Rust cxx bridge, which links
 * only `usd_ms` and never this plugin — it cannot call the C++ `RegisterProvider`
 * directly (unlike the Godot host in idtx-flow-idcm, which links the USD lib).
 *
 * These `extern "C"` entry points bridge that gap. They are exported from the
 * plugin DLL (`IDTX_API`); the Rust side resolves them from the already-resident
 * plugin via `libloading` (see `idtx_usd_exec::auth`). An adapter
 * (`HostCallbackProvider`, defined in the .cpp) forwards each `Resolve()` to the
 * supplied C callback, so the `IEnvironmentProvider` vtable and every
 * `std::string` stay inside this DLL — only `const char*` / length cross.
 *
 * Thread-safety / lifetime: registration is serialized by an internal mutex and
 * the adapters are owned for the plugin's process lifetime (idtx-studio runs the
 * exec compute on its main thread with no worker thread, so the "unregister
 * after the exec thread stops" caveat in EnvironmentProvider.h does not apply).
 */

#include <cstddef>

#include "./api.h"  // IDTX_API

extern "C"
{
    /**
     * Sink the resolve callback uses to hand the resolved value back to the
     * plugin. Copies @p len bytes from @p data into the plugin-side output
     * string. Never called on failure.
     */
    typedef void (*idtx_exec_str_sink)(void* sink_ctx, const char* data, std::size_t len);

    /**
     * Host resolve callback. Receives the de-prefixed key (@p key / @p key_len);
     * on success writes the value through @p sink (passing @p sink_ctx back) and
     * returns true. Returns false (writing nothing) when the key is unresolved.
     * MUST be thread-safe and MUST NOT throw across the boundary.
     */
    typedef bool (*idtx_exec_resolve_fn)(void* ctx,
                                         const char* key, std::size_t key_len,
                                         idtx_exec_str_sink sink, void* sink_ctx);

    /**
     * Register (or replace) a host provider for @p prefix. Pass an empty string
     * for the default/fallback provider. The adapter is owned by the shim.
     */
    IDTX_API void idtx_exec_env_register(const char* prefix,
                                         idtx_exec_resolve_fn fn, void* ctx);

    /** Unregister the host provider previously registered for @p prefix. */
    IDTX_API void idtx_exec_env_unregister(const char* prefix);

    /** Unregister every host provider registered through this shim. */
    IDTX_API void idtx_exec_env_clear(void);
}
