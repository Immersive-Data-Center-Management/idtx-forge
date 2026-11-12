/**
 * @file env_provider_host.cpp
 * @brief Implementation of the C-ABI environment-provider registration shim.
 *
 * Compiled into the exec plugin DLL alongside `EnvironmentProviderRegistry.cpp`,
 * so `EnvironmentProviderRegistry::Instance()` here refers to the same singleton
 * the `Compute_Environment` computation queries.
 */

#include "./env_provider_host.h"
#include "./EnvironmentProvider.h"

#include <map>
#include <memory>
#include <mutex>
#include <string>

namespace
{
    /**
     * Adapter that forwards `IEnvironmentProvider::Resolve` to a host-supplied C
     * callback. Keeps all `std::string` traffic inside the plugin: it hands the
     * callback `(key.data(), key.size())` and a sink that copies the result back
     * into @p out.
     */
    class HostCallbackProvider final : public idtx::IEnvironmentProvider
    {
    public:
        HostCallbackProvider(idtx_exec_resolve_fn fn, void* ctx) : fn_(fn), ctx_(ctx) {}

        bool Resolve(const std::string& key, std::string& out) const override
        {
            if (fn_ == nullptr)
            {
                return false;
            }
            // The sink assigns the resolved bytes into `out` (ctx = &out).
            const idtx_exec_str_sink sink = [](void* sink_ctx, const char* data, std::size_t len)
            {
                *static_cast<std::string*>(sink_ctx) = std::string(data, len);
            };
            return fn_(ctx_, key.data(), key.size(), sink, &out);
        }

    private:
        idtx_exec_resolve_fn fn_;
        void* ctx_;
    };

    // Owns the adapters for the plugin's process lifetime (the registry does NOT
    // take ownership). Guarded for the rare register/unregister races.
    std::mutex g_mutex;
    std::map<std::string, std::unique_ptr<HostCallbackProvider>> g_adapters;
}  // namespace

extern "C"
{
    void idtx_exec_env_register(const char* prefix, idtx_exec_resolve_fn fn, void* ctx)
    {
        const std::string key = (prefix != nullptr) ? std::string(prefix) : std::string();

        std::lock_guard<std::mutex> lock(g_mutex);
        if (fn == nullptr)
        {
            // Treat a null callback as an unregister (mirrors RegisterProvider).
            idtx::EnvironmentProviderRegistry::Instance().UnregisterProvider(key);
            g_adapters.erase(key);
            return;
        }
        auto adapter = std::make_unique<HostCallbackProvider>(fn, ctx);
        idtx::EnvironmentProviderRegistry::Instance().RegisterProvider(key, adapter.get());
        // Store AFTER a successful registration so the adapter outlives its use;
        // replacing an existing prefix frees the previous adapter here.
        g_adapters[key] = std::move(adapter);
    }

    void idtx_exec_env_unregister(const char* prefix)
    {
        const std::string key = (prefix != nullptr) ? std::string(prefix) : std::string();
        std::lock_guard<std::mutex> lock(g_mutex);
        idtx::EnvironmentProviderRegistry::Instance().UnregisterProvider(key);
        g_adapters.erase(key);
    }

    void idtx_exec_env_clear(void)
    {
        std::lock_guard<std::mutex> lock(g_mutex);
        idtx::EnvironmentProviderRegistry::Instance().UnregisterAll();
        g_adapters.clear();
    }
}
