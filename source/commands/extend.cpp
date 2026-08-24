/**
 * @file extend.cpp
 * @brief Implementation of the "extend" CLI subcommand.
 *
 * Traverses every prim in the given USD file(s) and, for each prim that is a
 * UsdGeomBoundable, (re)computes its extent using USD's built-in helper
 * UsdGeomBoundable::ComputeExtentFromPlugins().
 *
 * Behaviours
 * ----------
 * preserve
 *   Existing authored extents are left untouched. Only prims without a
 *   previously authored extent get one authored.
 *
 * overwrite
 *   The extent is always recomputed. If a previously authored extent exists
 *   and differs from the newly computed one, the new value is authored and a
 *   warning is logged showing the old and the new value. If the values are
 *   identical the prim is skipped. Prims without a previously authored extent
 *   simply get the computed extent authored.
 **/

#include "commands/extend.h"

#include <filesystem>
#include <string>
#include <vector>

#include <pxr/usd/sdf/layer.h>
#include <pxr/usd/usd/stage.h>
#include <pxr/usd/usd/primRange.h>
#include <pxr/usd/usdGeom/boundable.h>
#include <pxr/base/vt/array.h>
#include <pxr/base/gf/vec3f.h>

#include <idtx/utils/Logger.h>

#include "utils/StageExport.h"
#include "utils/stageutils.h"

namespace fs = std::filesystem;
PXR_NAMESPACE_USING_DIRECTIVE

IDTX_LOG_CATEGORY("Extend")

namespace idtx::commands {

namespace {

// ---------------------------------------------------------------------------
// Format a VtVec3fArray extent as a human readable string for logging.
// ---------------------------------------------------------------------------
std::string ExtentToString(const VtVec3fArray& extent)
{
    if (extent.size() < 2)
        return "[<empty>]";

    const GfVec3f& mn = extent[0];
    const GfVec3f& mx = extent[1];

    char buf[160];
    std::snprintf(buf, sizeof(buf),
        "[(%g, %g, %g) (%g, %g, %g)]",
        mn[0], mn[1], mn[2],
        mx[0], mx[1], mx[2]);
    return std::string(buf);
}

// ---------------------------------------------------------------------------
// Per-boundable extent (re)computation.
//
// Returns true when the prim was processed successfully (including the case
// where nothing needed to change), false when the extent could not be
// computed.
// ---------------------------------------------------------------------------
bool ProcessBoundable(
    const UsdGeomBoundable& boundable,
    const ExtendOptions&    opts)
{
    const UsdTimeCode time = UsdTimeCode::Default();
    const std::string primPath = boundable.GetPath().GetString();

    UsdAttribute extentAttr = boundable.GetExtentAttr();
    const bool hasAuthored  = extentAttr && extentAttr.HasAuthoredValue();

    // preserve: never touch an already authored extent.
    if (opts.behavior == "preserve" && hasAuthored)
    {
        IDTX_LOG(IDTX_DEBUG,
            "  Extent already authored on '{}' - preserving", primPath);
        return true;
    }

    // Compute the new extent using the USD built-in plugin machinery.
    VtVec3fArray newExtent;
    if (!UsdGeomBoundable::ComputeExtentFromPlugins(boundable, time, &newExtent))
    {
        IDTX_LOG(IDTX_WARN,
            "  Failed to compute extent for '{}' - skipping", primPath);
        return false;
    }

    if (newExtent.empty())
    {
        IDTX_LOG(IDTX_WARN,
            "  Computed empty extent for '{}' - skipping", primPath);
        return false;
    }

    // No previously authored extent: just author the freshly computed one.
    if (!hasAuthored)
    {
        boundable.GetExtentAttr().Set(newExtent, time);
        IDTX_LOG(IDTX_DEBUG,
            "  Authored extent {} on '{}'",
            ExtentToString(newExtent), primPath);
        return true;
    }

    // At this point we are in "overwrite" mode with an already authored extent.
    VtVec3fArray oldExtent;
    extentAttr.Get(&oldExtent, time);

    if (oldExtent == newExtent)
    {
        IDTX_LOG(IDTX_DEBUG,
            "  Extent unchanged on '{}' - continuing", primPath);
        return true;
    }

    // Values differ: author the new value and warn showing old vs. new.
    IDTX_LOG(IDTX_WARN,
        "  Extent changed on '{}': old {} -> new {}",
        primPath,
        ExtentToString(oldExtent),
        ExtentToString(newExtent));

    boundable.GetExtentAttr().Set(newExtent, time);
    return true;
}

// ---------------------------------------------------------------------------
// Process a single USD stage file
// ---------------------------------------------------------------------------
bool ProcessStage(
    const std::string&   inputPath,
    const std::string&   outputDir,
    const ExtendOptions& opts,
    bool                 dryRun)
{
    IDTX_LOG(IDTX_INFO, "Processing '{}'", inputPath);

    // when opening the stage, ensure we never load payload arcs, as we do not want to follow them anyway.
    UsdStageRefPtr stage = UsdStage::Open(inputPath, UsdStage::LoadNone);
    if (!stage)
    {
        IDTX_LOG(IDTX_ERROR, "  Failed to open USD stage '{}'", inputPath);
        return false;
    }

    // Apply the shared, consistent prim filter (defined, non-abstract,
    // non-native-instance). Instances (native or pseudo) are treated like any
    // other prim (Ignore) - their extent is authored on the instance prim
    // itself.
    idtx::utils::TraversalOptions travOpts;
    travOpts.instancePolicy = idtx::utils::InstancePolicy::Ignore;
    // By default only prims authored on the root layer are modified; the caller
    // can opt into authoring extents on referenced/payloaded prims explicitly.
    if (opts.includeReferenced)
        travOpts.referencedPolicy =
            idtx::utils::ReferencedPrimPolicy::IncludeReferencedAndPayloaded;

    const idtx::utils::TraversalResult res =
        idtx::utils::TraverseMeshLike<UsdGeomBoundable>(
            stage, travOpts,
            [&](const UsdPrim& prim, const idtx::utils::PrimVisitContext& /*ctx*/) {
                UsdGeomBoundable boundable(prim);
                IDTX_LOG(IDTX_DEBUG, "  Checking boundable prim '{}'",
                    prim.GetPath().GetString());
                return ProcessBoundable(boundable, opts);
            });

    const int boundableCount = res.processed + res.failures;
    const int failureCount   = res.failures;

    if (boundableCount == 0)
    {
        IDTX_LOG(IDTX_WARN, "  No UsdGeomBoundable prims found in '{}'", inputPath);
        return true;
    }

    IDTX_LOG(IDTX_INFO, "  Processed {}/{} boundable prim(s)",
        boundableCount - failureCount, boundableCount);

    if (dryRun)
    {
        IDTX_LOG(IDTX_INFO, "  [dry-run] Skipping export");
        return failureCount == 0;
    }

    // Output path: <outputDir>/<stem>_extend.<ext>
    const bool exported = ExportStage(stage, inputPath, outputDir, "_extend");
   
    return failureCount == 0 && exported;
}

} // anonymous namespace

// ===========================================================================
// Public API
// ===========================================================================

CLI::App* RegisterExtendCommand(CLI::App& app, ExtendOptions& opts)
{
    auto* sub = app.add_subcommand("extend",
        "Compute and author geometry extents for boundable prims\n"
        "  preserve  - keep existing extents, only author missing ones (default)\n"
        "  overwrite - recompute and replace extents, warn on changed values\n");

    sub->add_option("--behavior", opts.behavior,
            "How to handle existing extents (default: preserve)")
        ->type_name("preserve|overwrite")
        ->check(CLI::IsMember({"preserve", "overwrite"}));

    sub->add_flag("--include-referenced", opts.includeReferenced,
        "Also process prims that are brought into the stage via "
        "reference/payload arcs (default: only prims authored on the root "
        "layer are modified)");

    return sub;
}

int RunExtendCommand(
    const std::vector<std::string>& inputFiles,
    const std::string&              outputDir,
    const ExtendOptions&            opts,
    bool                            dryRun)
{
    IDTX_LOG(IDTX_INFO, "extend  behavior={}", opts.behavior);

    int failures = 0;
    for (const auto& inputPath : inputFiles)
    {
        if (!ProcessStage(inputPath, outputDir, opts, dryRun))
            ++failures;
    }

    if (failures > 0)
        IDTX_LOG(IDTX_ERROR, "{} file(s) failed to process extents", failures);

    return failures == 0 ? 0 : 1;
}

} // namespace idtx::commands