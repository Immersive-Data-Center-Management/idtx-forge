/**
 * @file updateversion.cpp
 * @brief Implementation of the "updateversion" subcommand.
 *
 * See updateversion.h for the full behavioural contract. The command discovers
 * every binary crate layer reachable from each input stage (optionally
 * recursing through reference/payload/sublayer arcs and into `.usdz` packages),
 * reports which ones carry a deprecated crate version, and - unless running in
 * dry-run mode - re-serializes each outdated layer in place at the crate version
 * written by the currently linked OpenUSD software.
 *
 * All heavy lifting lives in the reusable utilities:
 *   - idtx::utils::InspectCrateVersion / UpdateCrateFileInPlace (CrateUpdate.h)
 *   - idtx::utils::UpdateUsdzCratesInPlace (UsdzRepackage.h)
 * so this file is only responsible for layer discovery, de-duplication and
 * reporting.
 **/

#include "commands/updateversion.h"

#include <string>
#include <vector>

#include <pxr/pxr.h>
#include <pxr/usd/ar/resolverContext.h>

#include <idtx/utils/Logger.h>

#include "utils/CrateUpdate.h"
#include "utils/LayerDiscovery.h"
#include "utils/UsdzRepackage.h"

PXR_NAMESPACE_USING_DIRECTIVE

IDTX_LOG_CATEGORY("UpdateVersion")

namespace idtx::commands {

namespace {

using idtx::utils::DiscoveredLayers;
// ---------------------------------------------------------------------------
// Running tallies for the final summary.
// ---------------------------------------------------------------------------
struct Tally
{
    int deprecated = 0; ///< layers found carrying a deprecated crate version
    int updated    = 0; ///< layers actually re-serialized (0 in dry-run)
    int upToDate   = 0; ///< crate layers already at the current version
    int nonCrate   = 0; ///< layers/entries that are not crate files
    int failures   = 0; ///< layers/packages that failed to update
};

// Process the de-duplicated plain on-disk crate files.
void ProcessPlainFiles(const DiscoveredLayers& targets, bool dryRun, Tally& tally)
{
    for (const std::string& path : targets.plainFiles)
    {
        const idtx::utils::CrateVersionInfo info =
            idtx::utils::InspectCrateVersion(path);

        if (!info.isCrate)
        {
            IDTX_LOG(IDTX_DEBUG,
                "  '{}' is not a binary crate file - skipping", path);
            ++tally.nonCrate;
            continue;
        }

        if (!info.isOutdated)
        {
            IDTX_LOG(IDTX_INFO, "  '{}' crate version {} is current",
                path, info.fileVersion);
            ++tally.upToDate;
            continue;
        }

        ++tally.deprecated;
        IDTX_LOG(IDTX_WARN,
            "  '{}' has deprecated crate version {} (below current {})",
            path, info.fileVersion, idtx::utils::CrateDeprecationThreshold());

        if (dryRun)
        {
            IDTX_LOG(IDTX_INFO, "    [dry-run] would update '{}'", path);
            continue;
        }

        if (idtx::utils::UpdateCrateFileInPlace(path))
        {
            const idtx::utils::CrateVersionInfo now =
                idtx::utils::InspectCrateVersion(path);
            IDTX_LOG(IDTX_INFO, "    Updated '{}' to crate version {}",
                path, now.fileVersion);
            ++tally.updated;
        }
        else
        {
            ++tally.failures;
        }
    }
}

// Process the de-duplicated `.usdz` packages.
void ProcessPackages(const DiscoveredLayers& targets, bool dryRun, Tally& tally)
{
    for (const std::string& pkg : targets.packages)
    {
        IDTX_LOG(IDTX_INFO, "  Inspecting package '{}'", pkg);

        const idtx::utils::UsdzCrateUpdateResult res =
            idtx::utils::UpdateUsdzCratesInPlace(pkg, dryRun);

        if (!res.ok)
        {
            IDTX_LOG(IDTX_ERROR, "  Failed to process package '{}'", pkg);
            ++tally.failures;
            continue;
        }

        tally.upToDate  += res.upToDate;
        tally.nonCrate  += res.nonCrate;
        tally.deprecated += res.updated; // res.updated == deprecated-entry count

        if (dryRun)
        {
            if (res.updated > 0)
                IDTX_LOG(IDTX_INFO,
                    "    [dry-run] {} entry/entries in '{}' would be updated",
                    res.updated, pkg);
        }
        else
        {
            tally.updated += res.updated;
        }
    }
}

} // anonymous namespace

// ===========================================================================
// Public API
// ===========================================================================

CLI::App* RegisterUpdateVersionCommand(CLI::App& app, UpdateVersionOptions& opts)
{
    auto* sub = app.add_subcommand("updateversion",
        "Update deprecated OpenUSD binary (crate) files to the version written\n"
        "by the current OpenUSD software, in place. Recursively covers every\n"
        "referenced/payloaded/sublayered layer and layers packed inside .usdz\n"
        "archives. Honours --dry-run (report only); ignores --output-dir.");

    // A lone negated flag name ("!--no-recursive") toggles the bound bool to
    // false when present, leaving the default (true) otherwise.
    sub->add_flag("!--no-recursive", opts.recursive,
        "Only update the explicitly named input stage(s); do not follow "
        "reference/payload/sublayer arcs (default: recurse)");

    return sub;
}

int RunUpdateVersionCommand(
    const std::vector<std::string>& inputFiles,
    const std::string&              /*outputDir*/,
    const UpdateVersionOptions&     opts,
    bool                            dryRun)
{
    IDTX_LOG(IDTX_INFO, "updateversion  ({} input(s), recursive={}{})",
        inputFiles.size(), opts.recursive ? "yes" : "no",
        dryRun ? ", dry-run" : "");

    // 1) Discover the de-duplicated set of layers/packages across all inputs.
    DiscoveredLayers targets;
    ArResolverContext context; // unused here; crate update needs no resolution
    for (const std::string& inputPath : inputFiles)
        idtx::utils::CollectLayerTargets(inputPath, opts.recursive, targets, context);

    IDTX_LOG(IDTX_INFO, "  Discovered {} layer(s) and {} package(s) to inspect",
        targets.plainFiles.size(), targets.packages.size());

    // 2) Inspect, report and (unless dry-run) update.
    Tally tally;
    ProcessPlainFiles(targets, dryRun, tally);
    ProcessPackages(targets, dryRun, tally);

    const int inspected = tally.upToDate + tally.nonCrate + tally.deprecated;
    IDTX_LOG(IDTX_INFO,
        "updateversion summary: {} inspected, {} deprecated, {} {}, "
        "{} already current, {} non-crate, {} failure(s)",
        inspected, tally.deprecated,
        tally.updated, dryRun ? "would be updated" : "updated",
        tally.upToDate, tally.nonCrate, tally.failures);

    return tally.failures == 0 ? 0 : 1;
}

} // namespace idtx::commands
