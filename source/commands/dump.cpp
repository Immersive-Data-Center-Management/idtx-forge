/**
 * @file dump.cpp
 * @brief Implementation of the "dump" subcommand.
 *
 * See dump.h for the full behavioural contract. This file opens each input USD
 * stage read-only and prints a human-readable metadata summary. It never
 * mutates or exports the stage.
 **/

#include "commands/dump.h"

#include <cstdio>
#include <filesystem>
#include <string>
#include <system_error>
#include <vector>

#include <pxr/pxr.h>
#include <pxr/base/tf/token.h>
#include <pxr/base/vt/array.h>
#include <pxr/usd/sdf/layer.h>
#include <pxr/usd/sdf/path.h>
#include <pxr/usd/usd/prim.h>
#include <pxr/usd/usd/primRange.h>
#include <pxr/usd/usd/stage.h>
#include <pxr/usd/usdGeom/metrics.h>
#include <pxr/usd/usdGeom/mesh.h>

#include <idtx/utils/Logger.h>

#include "utils/stageutils.h"

namespace fs = std::filesystem;
PXR_NAMESPACE_USING_DIRECTIVE

IDTX_LOG_CATEGORY("Dump")

namespace idtx::commands {

namespace {

// ---------------------------------------------------------------------------
// FormatFileSize - pretty-print a byte count.
// ---------------------------------------------------------------------------
std::string FormatFileSize(std::uintmax_t bytes)
{
    constexpr double kKiB = 1024.0;
    constexpr double kMiB = 1024.0 * 1024.0;
    constexpr double kGiB = 1024.0 * 1024.0 * 1024.0;

    char buf[64];
    const double b = static_cast<double>(bytes);
    if (b >= kGiB)
        std::snprintf(buf, sizeof(buf), "%.2f GiB (%llu bytes)", b / kGiB,
                      static_cast<unsigned long long>(bytes));
    else if (b >= kMiB)
        std::snprintf(buf, sizeof(buf), "%.2f MiB (%llu bytes)", b / kMiB,
                      static_cast<unsigned long long>(bytes));
    else if (b >= kKiB)
        std::snprintf(buf, sizeof(buf), "%.2f KiB (%llu bytes)", b / kKiB,
                      static_cast<unsigned long long>(bytes));
    else
        std::snprintf(buf, sizeof(buf), "%llu bytes",
                      static_cast<unsigned long long>(bytes));
    return std::string(buf);
}

// ---------------------------------------------------------------------------
// MeshFaceInfo - result of inspecting a single mesh's face-vertex counts.
// ---------------------------------------------------------------------------
struct MeshFaceInfo
{
    size_t pointCount    = 0;   ///< number of authored points
    size_t faceCount     = 0;   ///< number of faces (size of faceVertexCounts)
    size_t indexCount    = 0;   ///< number of face-vertex indices
    size_t triangleCount = 0;   ///< triangle count after fan-triangulating each face
    bool   allTriangles  = true; ///< every face has exactly 3 vertices
};

MeshFaceInfo InspectMesh(const UsdGeomMesh& mesh)
{
    MeshFaceInfo info;

    VtVec3fArray points;
    if (UsdAttribute a = mesh.GetPointsAttr(); a && a.HasAuthoredValue())
    {
        a.Get(&points, UsdTimeCode::Default());
        info.pointCount = points.size();
    }

    VtIntArray faceVertexIndices;
    if (UsdAttribute a = mesh.GetFaceVertexIndicesAttr(); a && a.HasAuthoredValue())
    {
        a.Get(&faceVertexIndices, UsdTimeCode::Default());
        info.indexCount = faceVertexIndices.size();
    }

    VtIntArray faceVertexCounts;
    if (UsdAttribute a = mesh.GetFaceVertexCountsAttr(); a && a.HasAuthoredValue())
    {
        a.Get(&faceVertexCounts, UsdTimeCode::Default());
        info.faceCount = faceVertexCounts.size();
        for (int count : faceVertexCounts)
        {
            if (count != 3)
                info.allTriangles = false;
            // A convex polygon with n vertices fan-triangulates into (n - 2)
            // triangles; guard against degenerate faces (< 3 vertices).
            if (count >= 3)
                info.triangleCount += static_cast<size_t>(count - 2);
        }
    }
    else
    {
        // No authored face-vertex counts: cannot be considered triangulated.
        info.allTriangles = false;
    }

    return info;
}

// ---------------------------------------------------------------------------
// StageStats - accumulator for all the metrics we report per file.
// ---------------------------------------------------------------------------
struct StageStats
{
    size_t primCount              = 0; ///< total prims (excluding pseudo-root)
    size_t meshPrimCount          = 0; ///< prims that are UsdGeomMesh
    size_t nonMeshPrimCount       = 0; ///< prims that are not UsdGeomMesh
    size_t meshesWithoutTriangles = 0; ///< mesh prims not fully triangulated
    size_t primsWithoutTriangles  = 0; ///< prims that carry no triangle faces at all
    size_t pseudoInstanceCount    = 0; ///< prims classified as pseudo-instances
    size_t nativeInstanceCount    = 0; ///< prims where IsInstance() is true
    size_t nativePrototypeCount   = 0; ///< number of composed native prototypes

    // Stage-wide geometry totals across all mesh prims.
    size_t totalPoints    = 0; ///< sum of authored points over all meshes
    size_t totalIndices   = 0; ///< sum of face-vertex indices over all meshes
    size_t totalTriangles = 0; ///< sum of (fan-)triangulated triangle counts

    // Largest mesh tracking.
    size_t      maxPoints        = 0;
    std::string maxPointsPrim;
    size_t      maxFaces         = 0;
    std::string maxFacesPrim;
};

StageStats CollectStageStats(const UsdStageRefPtr& stage, const DumpOptions& opts)
{
    StageStats stats;

    // Choose traversal breadth: TraverseAll() includes prims behind
    // reference/payload arcs and inactive/abstract prims; Traverse() visits
    // the default-loaded composed scene.
    const auto range = opts.includeReferenced ? stage->TraverseAll()
                                              : stage->Traverse();

    for (const UsdPrim& prim : range)
    {
        if (prim.IsPseudoRoot())
            continue;

        ++stats.primCount;

        if (prim.IsInstance())
            ++stats.nativeInstanceCount;

        // Pseudo-instance detection (same helper the instancing command uses).
        SdfPath prototypePath;
        if (idtx::utils::IsPseudoInstance(prim, &prototypePath))
            ++stats.pseudoInstanceCount;

        if (prim.IsA<UsdGeomMesh>())
        {
            ++stats.meshPrimCount;

            const MeshFaceInfo info = InspectMesh(UsdGeomMesh(prim));
            if (!info.allTriangles)
            {
                ++stats.meshesWithoutTriangles;
                ++stats.primsWithoutTriangles;
            }

            stats.totalPoints    += info.pointCount;
            stats.totalIndices   += info.indexCount;
            stats.totalTriangles += info.triangleCount;

            if (info.pointCount > stats.maxPoints)
            {
                stats.maxPoints     = info.pointCount;
                stats.maxPointsPrim = prim.GetPath().GetString();
            }
            if (info.faceCount > stats.maxFaces)
            {
                stats.maxFaces     = info.faceCount;
                stats.maxFacesPrim = prim.GetPath().GetString();
            }
        }
        else
        {
            // Non-mesh prims trivially have no triangle faces.
            ++stats.nonMeshPrimCount;
            ++stats.primsWithoutTriangles;
        }
    }

    // Native prototypes are the composition-generated prototype prims.
    stats.nativePrototypeCount = stage->GetPrototypes().size();

    return stats;
}

// ---------------------------------------------------------------------------
// DumpRootLayerMetadata - up-axis, meters-per-unit, default prim, animation.
// ---------------------------------------------------------------------------
void DumpRootLayerMetadata(const UsdStageRefPtr& stage)
{
    // Up-axis.
    const TfToken upAxis = UsdGeomGetStageUpAxis(stage);
    IDTX_LOG(IDTX_INFO, "  Up-axis           : {}",
             upAxis.IsEmpty() ? std::string("(unset)") : upAxis.GetString());

    // Meters per unit.
    const double mpu = UsdGeomGetStageMetersPerUnit(stage);
    IDTX_LOG(IDTX_INFO, "  Meters per unit   : {}", mpu);

    // Default prim.
    if (stage->HasDefaultPrim())
    {
        const UsdPrim defaultPrim = stage->GetDefaultPrim();
        IDTX_LOG(IDTX_INFO, "  Default prim      : {}",
                 defaultPrim ? defaultPrim.GetPath().GetString()
                             : std::string("(invalid)"));
    }
    else
    {
        IDTX_LOG(IDTX_INFO, "  Default prim      : (none)");
    }

    // Animation data: report the authored time-code range if present.
    if (stage->HasAuthoredTimeCodeRange())
    {
        const double start = stage->GetStartTimeCode();
        const double end   = stage->GetEndTimeCode();
        const double fps   = stage->GetTimeCodesPerSecond();
        IDTX_LOG(IDTX_INFO,
                 "  Animation         : yes (start={}, end={}, tcps={})",
                 start, end, fps);
    }
    else
    {
        IDTX_LOG(IDTX_INFO, "  Animation         : none (no authored time range)");
    }
}

// ---------------------------------------------------------------------------
// ProcessFile - open one file and print its metadata summary.
// ---------------------------------------------------------------------------
bool ProcessFile(const std::string& inputPath, const DumpOptions& opts)
{
    IDTX_LOG(IDTX_INFO, "==================================================");
    IDTX_LOG(IDTX_INFO, "Dump of '{}'", inputPath);
    IDTX_LOG(IDTX_INFO, "==================================================");

    // File size on disk (before opening the stage, so it works even if USD
    // fails to fully compose).
    std::error_code ec;
    const std::uintmax_t sizeBytes = fs::file_size(inputPath, ec);
    if (ec)
        IDTX_LOG(IDTX_WARN, "  File size         : (unavailable: {})", ec.message());
    else
        IDTX_LOG(IDTX_INFO, "  File size         : {}", FormatFileSize(sizeBytes));

    UsdStageRefPtr stage = UsdStage::Open(inputPath, UsdStage::LoadNone);
    if (!stage)
    {
        IDTX_LOG(IDTX_ERROR, "  Failed to open USD stage '{}'", inputPath);
        return false;
    }

    // --- Root layer metadata -------------------------------------------------
    IDTX_LOG(IDTX_INFO, "  -- Root layer metadata --");
    DumpRootLayerMetadata(stage);

    // --- Prim statistics -----------------------------------------------------
    const StageStats stats = CollectStageStats(stage, opts);

    IDTX_LOG(IDTX_INFO, "  -- Prim statistics --");
    IDTX_LOG(IDTX_INFO, "  Prim count        : {}", stats.primCount);
    IDTX_LOG(IDTX_INFO, "  Mesh prims        : {}", stats.meshPrimCount);
    IDTX_LOG(IDTX_INFO, "  Non-mesh prims    : {}", stats.nonMeshPrimCount);
    IDTX_LOG(IDTX_INFO, "  Prims w/o triangle faces : {} ({} non-triangulated mesh(es) + {} non-mesh prim(s))",
             stats.primsWithoutTriangles, stats.meshesWithoutTriangles, stats.nonMeshPrimCount);
    IDTX_LOG(IDTX_INFO, "  Pseudo-instances  : {}", stats.pseudoInstanceCount);
    IDTX_LOG(IDTX_INFO, "  Native prototypes : {}", stats.nativePrototypeCount);
    IDTX_LOG(IDTX_INFO, "  Native instances  : {}", stats.nativeInstanceCount);

    // --- Stage-wide geometry totals -----------------------------------------
    IDTX_LOG(IDTX_INFO, "  -- Geometry totals (all mesh prims) --");
    IDTX_LOG(IDTX_INFO, "  Total points      : {}", stats.totalPoints);
    IDTX_LOG(IDTX_INFO, "  Total indices     : {}", stats.totalIndices);
    IDTX_LOG(IDTX_INFO, "  Total triangles   : {}", stats.totalTriangles);

    // --- Largest mesh --------------------------------------------------------
    IDTX_LOG(IDTX_INFO, "  -- Largest mesh --");
    if (stats.meshPrimCount == 0)
    {
        IDTX_LOG(IDTX_INFO, "  (no mesh prims found)");
    }
    else
    {
        IDTX_LOG(IDTX_INFO, "  Most points       : {} points  ('{}')",
                 stats.maxPoints,
                 stats.maxPointsPrim.empty() ? std::string("(unknown)")
                                             : stats.maxPointsPrim);
        IDTX_LOG(IDTX_INFO, "  Most faces        : {} faces   ('{}')",
                 stats.maxFaces,
                 stats.maxFacesPrim.empty() ? std::string("(unknown)")
                                            : stats.maxFacesPrim);
    }

    return true;
}

} // anonymous namespace

// ===========================================================================
// Public API
// ===========================================================================

CLI::App* RegisterDumpCommand(CLI::App& app, DumpOptions& opts)
{
    auto* sub = app.add_subcommand("dump",
        "Print a metadata summary of the given USD input files (read-only).\n"
        "Reports root-layer metadata (up-axis, meters-per-unit, default prim,\n"
        "animation), prim/mesh counts, pseudo-instance and native-instance\n"
        "counts, file size and the largest mesh prim.");

    sub->add_flag("--include-referenced", opts.includeReferenced,
        "Also account for prims that are brought into the stage via "
        "reference/payload arcs (default: only the default-loaded composed "
        "scene is traversed)");

    return sub;
}

int RunDumpCommand(
    const std::vector<std::string>& inputFiles,
    const std::string&              /*outputDir*/,
    const DumpOptions&              opts,
    bool                            /*dryRun*/)
{
    IDTX_LOG(IDTX_INFO, "dump  ({} file(s))", inputFiles.size());

    int failures = 0;
    for (const auto& inputPath : inputFiles)
    {
        if (!ProcessFile(inputPath, opts))
            ++failures;
    }

    if (failures > 0)
        IDTX_LOG(IDTX_ERROR, "{} file(s) could not be inspected", failures);

    return failures == 0 ? 0 : 1;
}

} // namespace idtx::commands
