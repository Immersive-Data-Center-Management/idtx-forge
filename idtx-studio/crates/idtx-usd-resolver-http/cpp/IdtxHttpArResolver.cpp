#include <pxr/usd/ar/asset.h>
#include <pxr/usd/ar/defineResolver.h>
#include <pxr/usd/ar/filesystemAsset.h>
#include <pxr/usd/ar/resolver.h>
#include <pxr/usd/ar/resolvedPath.h>
#include <pxr/usd/ar/timestamp.h>
#include <pxr/base/plug/registry.h>
#include <pxr/base/tf/registryManager.h>
#include <pxr/base/tf/type.h>

#include <atomic>
#include <cstdint>
#include <cstdio>
#include <filesystem>
#include <mutex>
#include <string>
#include <iostream>

// ---------------------------------------------------------------------------
// Callback type and process-global install state (global scope, no namespace)
// ---------------------------------------------------------------------------

// Downloads `url` to `dest_path`. Returns true on success. Thread-safe.
// Installed once via idtx_http_resolver_install().
using IdtxFetchFn = bool (*)(const char *url, const char *dest_path);

static struct IdtxResolverState {
    std::atomic<IdtxFetchFn> fetch_fn{nullptr};
    std::string               cache_dir;
    std::mutex                cache_dir_mu;
    // When true, `_OpenAsset` re-fetches (and overwrites) every asset it opens
    // even if a cached copy exists. Armed around a "Redownload" so the whole
    // stage — root layer plus all referenced sublayers, references, and
    // textures — is refreshed from the server. Left false for normal opens so
    // they stay offline and serve the cache.
    std::atomic<bool>         force_refresh{false};
    // When true, `_Resolve` reports every http(s) URI as UNRESOLVABLE (empty
    // ArResolvedPath). Armed around a cook/package operation so OpenUSD's
    // dependency localizer leaves remote references AUTHORED (external) in the
    // produced usdz instead of fetching + embedding them (which would rewrite
    // the original remote path and, for an unreachable host, abort the pack).
    // Left false for normal opens so remote stages still resolve + fetch.
    std::atomic<bool>         packaging_mode{false};
} g_resolver_state;

// ---------------------------------------------------------------------------
// URL utilities (global scope)
// ---------------------------------------------------------------------------

static bool idtx_is_http_uri(const std::string &s) {
    return s.compare(0, 7, "http://") == 0 ||
           s.compare(0, 8, "https://") == 0;
}

// FNV-1a 64-bit hash → 16-char hex string (cache subdirectory name).
static std::string idtx_fnv1a_hex(const std::string &s) {
    uint64_t h = 14695981039346656037ULL;
    for (unsigned char c : s) {
        h ^= c;
        h *= 1099511628211ULL;
    }
    char buf[17];
    std::snprintf(buf, sizeof(buf), "%016llx",
                  static_cast<unsigned long long>(h));
    return buf;
}

// Extracts the bare filename from a URL (last path component, strips query).
static std::string idtx_url_filename(const std::string &url) {
    auto q     = url.find('?');
    auto clean = (q != std::string::npos) ? url.substr(0, q) : url;
    auto slash = clean.rfind('/');
    auto name  = (slash != std::string::npos) ? clean.substr(slash + 1) : clean;
    return name.empty() ? "asset" : name;
}

// Resolves `ref` relative to `base` (RFC 3986, simplified for http/https).
static std::string idtx_resolve_relative(const std::string &base,
                                          const std::string &ref) {
    if (idtx_is_http_uri(ref) || ref.empty()) return ref;
    if (ref[0] == '/') {
        auto p = base.find("//");
        if (p == std::string::npos) return ref;
        auto host_end = base.find('/', p + 2);
        auto prefix   = (host_end != std::string::npos)
                          ? base.substr(0, host_end)
                          : base;
        return prefix + ref;
    }
    auto slash = base.rfind('/');
    auto dir   = (slash != std::string::npos) ? base.substr(0, slash + 1)
                                              : base + "/";
    return dir + ref;
}

// Collapses "." and ".." segments in a URL *path* using the standard
// "remove_dot_segments" algorithm from RFC 3986 §5.2.4. This is deliberately
// the named RFC routine rather than std::filesystem::path::lexically_normal(),
// which is unsafe for URLs: on Windows it uses '\' as the preferred separator,
// collapses "//" (corrupting the "scheme://" prefix), and can misread a
// segment such as "C:" as a drive root. Operates purely on the path portion;
// the caller keeps scheme, authority (host), query and fragment intact.
static std::string idtx_remove_dot_segments(const std::string &path) {
    std::string in = path;
    std::string out;
    out.reserve(in.size());
    while (!in.empty()) {
        // A. Strip leading "../" or "./".
        if (in.compare(0, 3, "../") == 0) { in.erase(0, 3); continue; }
        if (in.compare(0, 2, "./") == 0)  { in.erase(0, 2); continue; }
        // B. "/./" → "/", and a trailing "/." → "/".
        if (in.compare(0, 3, "/./") == 0) { in.replace(0, 3, "/"); continue; }
        if (in == "/.")                   { in = "/"; continue; }
        // C. "/../" (or trailing "/..") → "/", popping the last output segment.
        if (in.compare(0, 4, "/../") == 0 || in == "/..") {
            in = (in.size() == 3) ? std::string("/") : in.substr(3);
            auto slash = out.rfind('/');
            out.erase(slash == std::string::npos ? 0 : slash);
            continue;
        }
        // D. A bare "." or ".." with no slash is dropped entirely.
        if (in == "." || in == "..") { in.clear(); continue; }
        // E. Move the first path segment (leading '/' up to the next '/').
        std::size_t start = (in[0] == '/') ? 1 : 0;
        std::size_t next  = in.find('/', start);
        if (next == std::string::npos) {
            out += in;
            in.clear();
        } else {
            out += in.substr(0, next);
            in.erase(0, next);
        }
    }
    return out;
}

// Canonicalises an http/https URL so that identifiers differing only by "." /
// ".." path segments (e.g. ".../Racks/Instances/../../Servers/x.usdz" reached
// via a relative reference vs ".../Servers/x.usdz" opened directly) collapse to
// a single string — and therefore a single cache-folder hash and a single USD
// layer. Only the path is normalised; scheme, authority, query and fragment are
// preserved verbatim.
static std::string idtx_normalize_http_url(const std::string &url) {
    if (!idtx_is_http_uri(url)) return url;
    auto scheme_end = url.find("//");
    if (scheme_end == std::string::npos) return url;
    // The authority ends at the first '/', '?' or '#' after "scheme://".
    auto path_start = url.find_first_of("/?#", scheme_end + 2);
    if (path_start == std::string::npos || url[path_start] != '/')
        return url;                                    // no path to normalise
    std::string prefix = url.substr(0, path_start);    // "scheme://host"
    std::string rest   = url.substr(path_start);       // path[?query][#frag]
    std::string suffix;
    auto qf = rest.find_first_of("?#");
    if (qf != std::string::npos) {
        suffix = rest.substr(qf);
        rest   = rest.substr(0, qf);
    }
    return prefix + idtx_remove_dot_segments(rest) + suffix;
}

// ---------------------------------------------------------------------------
// Resolver class — must live inside the PXR namespace for AR_DEFINE_RESOLVER
// ---------------------------------------------------------------------------

PXR_NAMESPACE_OPEN_SCOPE

class IdtxHttpArResolver : public ArResolver {
public:
    IdtxHttpArResolver()  = default;
    ~IdtxHttpArResolver() = default;

protected:
    std::string _CreateIdentifier(
            const std::string    &assetPath,
            const ArResolvedPath &anchorPath) const override {
        if (idtx_is_http_uri(assetPath))
            return idtx_normalize_http_url(assetPath);
        if (!anchorPath.IsEmpty()) {
            const std::string &anchor = anchorPath.GetPathString();
            if (idtx_is_http_uri(anchor))
                return idtx_normalize_http_url(
                    idtx_resolve_relative(anchor, assetPath));
        }
        return assetPath;
    }

    std::string _CreateIdentifierForNewAsset(
            const std::string    &assetPath,
            const ArResolvedPath &anchorPath) const override {
        return _CreateIdentifier(assetPath, anchorPath);
    }

    ArResolvedPath _Resolve(const std::string &assetPath) const override {
        if (!idtx_is_http_uri(assetPath)) return {};
        // During a cook/package, keep remote refs external: report the URL as
        // unresolvable so the USDZ localizer leaves it authored rather than
        // fetching + embedding it. See idtx_http_resolver_set_packaging_mode.
        if (g_resolver_state.packaging_mode.load(std::memory_order_acquire))
            return {};
        return ArResolvedPath{assetPath};
    }

    ArResolvedPath _ResolveForNewAsset(
            const std::string &) const override {
        return {};
    }

    std::shared_ptr<ArAsset> _OpenAsset(
            const ArResolvedPath &resolvedPath) const override {
        const std::string &url = resolvedPath.GetPathString();
        if (!idtx_is_http_uri(url)) return nullptr;

        IdtxFetchFn fn = g_resolver_state.fetch_fn.load(std::memory_order_acquire);
        if (!fn) return nullptr;

        std::string cache_dir;
        {
            std::lock_guard<std::mutex> lk(g_resolver_state.cache_dir_mu);
            cache_dir = g_resolver_state.cache_dir;
        }
        if (cache_dir.empty()) return nullptr;

        std::filesystem::path local =
            std::filesystem::path(cache_dir)
            / idtx_fnv1a_hex(url)
            / idtx_url_filename(url);

        std::error_code ec;
        std::filesystem::create_directories(local.parent_path(), ec);

        // Fetch when the cache is empty, or when a Redownload has armed
        // force_refresh (in which case rust_fetch overwrites the cached file
        // in place via a temp-write + atomic rename).
        const bool force = g_resolver_state.force_refresh.load(std::memory_order_acquire);
        if (force || !std::filesystem::exists(local, ec)) {
            if (!fn(url.c_str(), local.string().c_str())) return nullptr;
        }

        return ArFilesystemAsset::Open(ArResolvedPath{local.string()});
    }

    std::shared_ptr<ArWritableAsset> _OpenAssetForWrite(
            const ArResolvedPath &,
            ArResolver::WriteMode) const override {
        return nullptr;
    }

    bool _IsContextDependentPath(
            const std::string &) const override {
        return false;
    }

    ArTimestamp _GetModificationTimestamp(
            const std::string &,
            const ArResolvedPath &) const override {
        return {};
    }
};

AR_DEFINE_RESOLVER(IdtxHttpArResolver, ArResolver);

PXR_NAMESPACE_CLOSE_SCOPE

// ---------------------------------------------------------------------------
// Install API — called from Rust before the first remote stage open
// ---------------------------------------------------------------------------

// On Windows, symbols are hidden by default in a DLL; __declspec(dllexport) is
// required so link.exe creates the import library (.lib) that Rust links against.
#ifdef _WIN32
#  define IDTX_EXPORT __declspec(dllexport)
#else
#  define IDTX_EXPORT
#endif

extern "C" IDTX_EXPORT void idtx_http_resolver_install(IdtxFetchFn fn,
                                                        const char  *cache_dir,
                                                        const char  *plugin_dir) {
    {
        std::lock_guard<std::mutex> lk(g_resolver_state.cache_dir_mu);
        g_resolver_state.cache_dir = cache_dir ? cache_dir : "";
    }
    g_resolver_state.fetch_fn.store(fn, std::memory_order_release);

    // Register the plugInfo.json that declares IdtxHttpArResolver's URI schemes.
    //
    // Setting PXR_PLUGINPATH_NAME from Rust before this call is NOT sufficient:
    // Plug reads that env var in an ARCH_CONSTRUCTOR (plug/initConfig.cpp) that
    // runs at library-load time, before main() — so any runtime mutation of the
    // env var is ignored. We must instead register the plugin directory
    // explicitly via the runtime API, which also declares the type against this
    // plugin (populating PlugRegistry's class map) so Ar can find the plugin for
    // the type when it enumerates URI resolvers. This must happen before the
    // first ArGetResolver() call (i.e. before any stage is opened).
    if (plugin_dir && *plugin_dir) {
        PXR_NS::PlugRegistry::GetInstance().RegisterPlugins(
            std::string(plugin_dir));
    }
}

// Arms or disarms force-refresh. While armed, `_OpenAsset` re-fetches every
// asset it opens even if cached, overwriting the cache in place. The shell
// arms this on the loader thread for the duration of a "Redownload" stage open
// and disarms it immediately after, so normal opens keep serving the cache.
extern "C" IDTX_EXPORT void idtx_http_resolver_set_force_refresh(bool on) {
    g_resolver_state.force_refresh.store(on, std::memory_order_release);
}

// Arms or disarms packaging mode. While armed, `_Resolve` reports every http(s)
// URI as unresolvable, so OpenUSD's dependency localization leaves remote
// references authored (external) in a produced usdz instead of fetching and
// embedding them. The cook arms this for the duration of the package call (see
// idtx_usd::package_stage_keeping_remote) and disarms it immediately after, so
// normal remote stage opens keep resolving + fetching.
extern "C" IDTX_EXPORT void idtx_http_resolver_set_packaging_mode(bool on) {
    g_resolver_state.packaging_mode.store(on, std::memory_order_release);
}
