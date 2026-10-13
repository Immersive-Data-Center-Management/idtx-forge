/**
 * @file cleanrefs.cpp
 * @brief Implementation of the "cleanrefs" subcommand.
 *
 * See cleanrefs.h for the full behavioural contract. The command discovers
 * every layer reachable from each input stage (optionally recursing through
 * reference/payload/sublayer arcs and into `.usdz` packages), reports which
 * ones carry dangling composition arcs and - unless running in dry-run mode -
 * strips those arcs and re-serializes the affected layers in place.
 *
 * All heavy lifting lives in the reusable utilities:
 *   - idtx::utils::CollectLayerTargets        (LayerDiscovery.h)
 *   - idtx::utils::CleanupLayerFileInPlace     (ReferenceCleanup.h)
 *   - idtx::utils::CleanupUsdzReferencesInPlace (UsdzRepackage.h)
 * so this file is only responsible for discovery orchestration and reporting.
 **/

#include "commands/cleanrefs.h"

#include <string>
#include <vector>

#include <pxr/pxr.h>
#include <pxr/usd/ar/resolverContext.h>

#include <idtx/utils/Logger.h>

#include "utils/LayerDiscovery.h"
#include "utils/ReferenceCleanup.h"
#include "utils/UdimNormalize.h"
#include "utils/UsdzRepackage.h"

PXR_NAMESPACE_USING_DIRECTIVE

IDTX_LOG_CATEGORY("CleanRefs")

namespace idtx::commands {

namespace {

// Running tallies for the final summary.
struct Tally
{
    int layers   = 0; ///< plain layers + package entries inspected
    int found    = 0; ///< dangling arcs detected
    int removed  = 0; ///< dangling arcs removed (0 in dry-run)
    int failures = 0; ///< layers/packages that failed to process
};

// Is this found arc removable under the current --anon-only filter?
bool IsRemovable(const idtx::utils::DanglingArc& arc, bool anonOnly)
{
    return !anonOnly ||
           arc.reason == idtx::utils::DanglingReason::AnonymousLayer;
}

// Report one dangling arc, phrased for dry-run vs. live and the filter.
void ReportArc(const std::string&              layerPath,
               const idtx::utils::DanglingArc& arc,
               bool                            anonOnly,
               bool                            dryRun)
{
    const char* kind   = idtx::utils::ToString(arc.kind);
    const char* reason = idtx::utils::ToString(arc.reason);

    if (!IsRemovable(arc, anonOnly))
    {
        IDTX_LOG(IDTX_WARN,
            "    dangling {} in '{}' <{}> -> @{}@ ({}) - kept (--anon-only)",
            kind, layerPath, arc.primPath.GetString(), arc.assetPath, reason);
        return;
    }

    if (dryRun)
        IDTX_LOG(IDTX_INFO,
            "    [dry-run] would remove {} in '{}' <{}> -> @{}@ ({})",
            kind, layerPath, arc.primPath.GetString(), arc.assetPath, reason);
    else
        IDTX_LOG(IDTX_INFO,
            "    removed {} in '{}' <{}> -> @{}@ ({})",
            kind, layerPath, arc.primPath.GetString(), arc.assetPath, reason);
}

// Process the de-duplicated plain on-disk layers.
void ProcessPlainFiles(const idtx::utils::DiscoveredLayers& targets,
                       const ArResolverContext&             context,
                       const CleanRefsOptions&              opts,
                       bool                                 dryRun,
                       Tally&                               tally)
{
    for (const std::string& path : targets.plainFiles)
    {
        ++tally.layers;
        const idtx::utils::CleanRefsResult res =
            idtx::utils::CleanupLayerFileInPlace(
                path, opts.anonOnly, dryRun, context, opts.textures);

        if (!res.ok)
        {
            IDTX_LOG(IDTX_ERROR, "  Failed to process layer '{}'", path);
            ++tally.failures;
            continue;
        }

        if (res.found == 0)
        {
            IDTX_LOG(IDTX_DEBUG, "  '{}' has no dangling arcs", path);
            continue;
        }

        IDTX_LOG(IDTX_WARN, "  '{}' has {} dangling arc(s)", path, res.found);
        tally.found += res.found;
        for (const idtx::utils::DanglingArc& arc : res.arcs)
            ReportArc(path, arc, opts.anonOnly, dryRun);

        if (dryRun)
        {
            for (const idtx::utils::DanglingArc& arc : res.arcs)
                if (IsRemovable(arc, opts.anonOnly))
                    ++tally.removed; // count what *would* be removed
        }
        else
        {
            tally.removed += res.removed;
        }
    }
}

// Process the de-duplicated `.usdz` packages.
void ProcessPackages(const idtx::utils::DiscoveredLayers& targets,
                     const CleanRefsOptions&              opts,
                     bool                                 dryRun,
                     Tally&                               tally)
{
    for (const std::string& pkg : targets.packages)
    {
        ++tally.layers;
        IDTX_LOG(IDTX_INFO, "  Inspecting package '{}'", pkg);

        const idtx::utils::UsdzCleanupResult res =
            idtx::utils::CleanupUsdzReferencesInPlace(
                pkg, opts.anonOnly, dryRun, opts.textures);

        if (!res.ok)
        {
            IDTX_LOG(IDTX_ERROR, "  Failed to process package '{}'", pkg);
            ++tally.failures;
            continue;
        }

        tally.found += res.found;
        if (dryRun)
        {
            // CleanupUsdzReferencesInPlace reports per-arc internally; here we
            // only need the counts. In dry-run res.removed is 0, so derive the
            // would-remove count from res.found when not filtering, otherwise
            // leave the per-arc detail (already logged) to speak for itself.
            if (res.found > 0 && !opts.anonOnly)
                tally.removed += res.found;
        }
        else
        {
            tally.removed += res.removed;
        }
    }
}

// ---------------------------------------------------------------------------
// UDIM normalization pass (opt-in via --udim).
// ---------------------------------------------------------------------------

// Running tallies for the UDIM pass summary.
struct UdimTally
{
    int found     = 0; ///< asset paths carrying a non-standard UDIM token
    int rewritten = 0; ///< asset paths rewritten to <UDIM> (0 in dry-run)
    int failures  = 0; ///< layers/packages that failed to process
};

// Normalize UDIM tokens in the de-duplicated plain on-disk layers.
void ProcessUdimPlainFiles(const idtx::utils::DiscoveredLayers& targets,
                           bool                                 dryRun,
                           UdimTally&                           tally)
{
    for (const std::string& path : targets.plainFiles)
    {
        const idtx::utils::UdimResult res =
            idtx::utils::NormalizeUdimLayerFileInPlace(path, dryRun);

        if (!res.ok)
        {
            IDTX_LOG(IDTX_ERROR, "  Failed to normalize UDIM in layer '{}'", path);
            ++tally.failures;
            continue;
        }

        if (res.found == 0)
        {
            IDTX_LOG(IDTX_DEBUG, "  '{}' has no non-standard UDIM tokens", path);
            continue;
        }

        IDTX_LOG(IDTX_WARN, "  '{}' has {} non-standard UDIM token(s)",
            path, res.found);
        tally.found += res.found;
        for (const idtx::utils::UdimRewrite& rw : res.rewrites)
        {
            if (dryRun)
                IDTX_LOG(IDTX_INFO,
                    "    [dry-run] would rewrite <{}> @{}@ -> @{}@",
                    rw.specPath.GetString(), rw.oldPath, rw.newPath);
            else
                IDTX_LOG(IDTX_INFO,
                    "    rewrote <{}> @{}@ -> @{}@",
                    rw.specPath.GetString(), rw.oldPath, rw.newPath);
        }

        if (dryRun)
            tally.rewritten += res.found; // count what *would* be rewritten
        else
            tally.rewritten += res.rewritten;
    }
}

// Normalize UDIM tokens in the de-duplicated `.usdz` packages.
void ProcessUdimPackages(const idtx::utils::DiscoveredLayers& targets,
                         bool                                 dryRun,
                         UdimTally&                           tally)
{
    for (const std::string& pkg : targets.packages)
    {
        IDTX_LOG(IDTX_INFO, "  Inspecting package '{}' for UDIM tokens", pkg);

        const idtx::utils::UsdzUdimResult res =
            idtx::utils::NormalizeUdimUsdzInPlace(pkg, dryRun);

        if (!res.ok)
        {
            IDTX_LOG(IDTX_ERROR,
                "  Failed to normalize UDIM in package '{}'", pkg);
            ++tally.failures;
            continue;
        }

        tally.found += res.found;
        if (dryRun)
            tally.rewritten += res.found; // dry-run reports would-rewrite count
        else
            tally.rewritten += res.rewritten;
    }
}

} // anonymous namespace

// ===========================================================================
// Public API
// ===========================================================================

CLI::App* RegisterCleanRefsCommand(CLI::App& app, CleanRefsOptions& opts)
{
    auto* sub = app.add_subcommand("cleanrefs",
        "Detect and remove dangling composition arcs (references, payloads and\n"
        "sublayers whose target can never be opened - e.g. the stray\n"
        "'anon:0x...' identifiers Houdini bakes into exports), in place.\n"
        "Recursively covers every referenced/payloaded/sublayered layer and\n"
        "layers packed inside .usdz archives. With --udim, also normalizes\n"
        "non-standard UDIM tokens (%(UDIM)d, <udim>) to the canonical <UDIM>.\n"
        "With --textures, also removes asset-valued attributes (e.g. texture\n"
        "'inputs:file') whose path cannot be resolved.\n"
        "Honours --dry-run (list only); ignores --output-dir.");

    // A lone negated flag name ("!--no-recursive") toggles the bound bool to
    // false when present, leaving the default (true) otherwise.
    sub->add_flag("!--no-recursive", opts.recursive,
        "Only clean the explicitly named input stage(s); do not follow "
        "reference/payload/sublayer arcs (default: recurse)");

    sub->add_flag("--anon-only", opts.anonOnly,
        "Only remove arcs pointing at anonymous layer identifiers (the "
        "always-safe case); report but keep unresolvable on-disk arcs");

    sub->add_flag("--udim", opts.udim,
        "Additionally rewrite non-standard UDIM tokens in asset-valued "
        "attributes ('%(UDIM)d' and width variants, or a lower/mixed-case "
        "'<udim>') to the canonical OpenUSD '<UDIM>' token");

    sub->add_flag("--textures", opts.textures,
        "Additionally remove asset-valued attributes (e.g. a texture "
        "'inputs:file') whose path cannot be resolved; '<UDIM>'/'%(...)' "
        "template paths are never touched (combine with --udim to normalize "
        "those first)");

    return sub;
}

int RunCleanRefsCommand(
    const std::vector<std::string>& inputFiles,
    const std::string&              /*outputDir*/,
    const CleanRefsOptions&         opts,
    bool                            dryRun)
{
    IDTX_LOG(IDTX_INFO, "cleanrefs  ({} input(s), recursive={}, anon-only={}, udim={}, textures={}{})",
        inputFiles.size(), opts.recursive ? "yes" : "no",
        opts.anonOnly ? "yes" : "no", opts.udim ? "yes" : "no",
        opts.textures ? "yes" : "no", dryRun ? ", dry-run" : "");

    // 1) Discover the de-duplicated set of layers/packages across all inputs,
    //    capturing a representative resolver context for the unresolvable check.
    idtx::utils::DiscoveredLayers targets;
    ArResolverContext context;
    for (const std::string& inputPath : inputFiles)
    {
        ArResolverContext ctx;
        idtx::utils::CollectLayerTargets(inputPath, opts.recursive, targets, ctx);
        if (!ctx.IsEmpty())
            context = ctx;
    }

    IDTX_LOG(IDTX_INFO, "  Discovered {} layer(s) and {} package(s) to inspect",
        targets.plainFiles.size(), targets.packages.size());

    // 2) Inspect, report and (unless dry-run) clean.
    Tally tally;
    ProcessPlainFiles(targets, context, opts, dryRun, tally);
    ProcessPackages(targets, opts, dryRun, tally);

    IDTX_LOG(IDTX_INFO,
        "cleanrefs summary: {} layer(s) inspected, {} dangling arc(s) found, "
        "{} {}, {} failure(s)",
        tally.layers, tally.found,
        tally.removed, dryRun ? "would be removed" : "removed",
        tally.failures);

    // 3) Optional UDIM-token normalization pass over the same discovered
    //    targets (opt-in via --udim). Runs in addition to the arc cleanup.
    UdimTally udimTally;
    if (opts.udim)
    {
        IDTX_LOG(IDTX_INFO, "  Normalizing UDIM tokens");
        ProcessUdimPlainFiles(targets, dryRun, udimTally);
        ProcessUdimPackages(targets, dryRun, udimTally);

        IDTX_LOG(IDTX_INFO,
            "cleanrefs udim summary: {} non-standard UDIM token(s) found, "
            "{} {}, {} failure(s)",
            udimTally.found,
            udimTally.rewritten, dryRun ? "would be rewritten" : "rewritten",
            udimTally.failures);
    }

    return (tally.failures + udimTally.failures) == 0 ? 0 : 1;
}

} // namespace idtx::commands
