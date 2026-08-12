/**
 * @file normals.cpp
 * @brief Implementation of the "normals" CLI subcommand.
 *
 * Calculates vertex normals for every UsdGeomMesh prim in the given USD file(s).
 *
 * Algorithms
 * ----------
 * faceweighted
 *   Accumulate each adjacent face's geometric normal weighted by the face
 *   area, then normalise. Larger faces contribute more strongly to the shared
 *   vertex normal. (The un-normalised cross product of two triangle edges has
 *   a magnitude proportional to twice the triangle area, so simply summing the
 *   raw cross products yields area weighting for free.)
 *
 * angleweighted
 *   Accumulate each adjacent face's unit geometric normal weighted by the
 *   interior angle at the shared vertex, then normalise. Produces results that
 *   are less sensitive to uneven tessellation.
 *
 * Behaviour
 * ---------
 * preserve
 *   Keep any existing authored normals and only calculate normals where they
 *   are missing.
 *
 * overwrite
 *   Recalculate and replace all normals regardless of existing values.
 *
 * Normals are authored as vertex-interpolated `normals` (one per mesh point).
 *
 * Only prototype/template meshes and ordinary (non-instanced) meshes are
 * processed - the shared traversal helper skips instance prims and processes
 * each unique prototype exactly once so normals are authored on the shared
 * geometry rather than on every instance.
 **/

#include "commands/normals.h"

#include <algorithm>
#include <cmath>
#include <string>
#include <vector>

#include <pxr/base/gf/vec3f.h>
#include <pxr/base/vt/array.h>
#include <pxr/usd/usd/stage.h>
#include <pxr/usd/usdGeom/mesh.h>
#include <pxr/usd/usdGeom/primvarsAPI.h>
#include <pxr/usd/usdGeom/tokens.h>

#include <idtx/utils/Logger.h>

#include "utils/StageExport.h"
#include "utils/stageutils.h"

PXR_NAMESPACE_USING_DIRECTIVE

IDTX_LOG_CATEGORY("Normals")

namespace idtx::commands {

namespace {

// Compute the interior angle (radians) at vertex `apex` in the triangle
// (apex, b, c). Returns 0 for degenerate edges.
float InteriorAngle(const GfVec3f& apex, const GfVec3f& b, const GfVec3f& c)
{
    GfVec3f e0 = b - apex;
    GfVec3f e1 = c - apex;
    const float l0 = e0.GetLength();
    const float l1 = e1.GetLength();
    if (l0 < 1e-9f || l1 < 1e-9f)
        return 0.0f;
    e0 /= l0;
    e1 /= l1;
    const float d = std::clamp(GfDot(e0, e1), -1.0f, 1.0f);
    return std::acos(d);
}

// ---------------------------------------------------------------------------
// Compute per-vertex (per-point) normals for a mesh and author them.
//
// Returns true on success (including "nothing to do" under preserve), false
// when required data could not be read.
// ---------------------------------------------------------------------------
bool ComputeNormalsForMesh(UsdGeomMesh& mesh, const NormalsOptions& opts)
{
    const UsdTimeCode time = UsdTimeCode::Default();
    const std::string primPath = mesh.GetPath().GetString();

    // preserve: leave an already authored normals attribute (or normals
    // primvar) untouched.
    UsdGeomPrimvarsAPI primvarsAPI(mesh.GetPrim());
    const bool hasSchemaNormals = mesh.GetNormalsAttr().HasAuthoredValue();
    UsdGeomPrimvar normalsPv = primvarsAPI.GetPrimvar(UsdGeomTokens->normals);
    const bool hasPrimvarNormals = normalsPv && normalsPv.HasAuthoredValue();

    if (opts.behaviour == "preserve" && (hasSchemaNormals || hasPrimvarNormals))
    {
        IDTX_LOG(IDTX_DEBUG,
            "  Normals already authored on '{}' - preserving", primPath);
        return true;
    }

    VtIntArray faceVertexCounts, faceVertexIndices;
    if (!mesh.GetFaceVertexCountsAttr().Get(&faceVertexCounts, time) ||
        !mesh.GetFaceVertexIndicesAttr().Get(&faceVertexIndices, time))
    {
        IDTX_LOG(IDTX_WARN,
            "  Cannot read face topology from '{}' - skipping", primPath);
        return false;
    }

    VtVec3fArray points;
    if (!mesh.GetPointsAttr().Get(&points, time) || points.empty())
    {
        IDTX_LOG(IDTX_WARN,
            "  Cannot read points from '{}' - skipping", primPath);
        return false;
    }
    
    bool leftHanded = utils::IsLeftHanded(mesh);

    const int pointCount = static_cast<int>(points.size());
    std::vector<GfVec3f> accum(pointCount, GfVec3f(0));

    // Accumulate contributions by fan-triangulating each face.
    int faceStart = 0;
    for (size_t f = 0; f < faceVertexCounts.size(); ++f)
    {
        const int faceSize = faceVertexCounts[f];
        if (faceSize < 3)
        {
            faceStart += faceSize;
            continue;
        }

        // Fan from the first vertex: triangles (0, k, k+1).
        const int baseIdx = faceVertexIndices[faceStart];
        for (int k = 1; k < faceSize - 1; ++k)
        {
            const int iA = baseIdx;
            const int iB = faceVertexIndices[faceStart + k];
            const int iC = faceVertexIndices[faceStart + k + 1];

            if (iA < 0 || iA >= pointCount ||
                iB < 0 || iB >= pointCount ||
                iC < 0 || iC >= pointCount)
                continue;

            const GfVec3f& a = points[iA];
            const GfVec3f& b = points[iB];
            const GfVec3f& c = points[iC];

            // Raw cross product: direction = face normal, magnitude = 2*area.
            const GfVec3f faceNormalAreaWeighted = GfCross(b - a, c - a);

            if (opts.algorithm == "angleweighted")
            {
                GfVec3f unitNormal = faceNormalAreaWeighted;
                if (unitNormal.GetLengthSq() > 1e-20f)
                    unitNormal.Normalize();

                // Weight each corner of this triangle by its interior angle.
                accum[iA] += unitNormal * InteriorAngle(a, b, c);
                accum[iB] += unitNormal * InteriorAngle(b, c, a);
                accum[iC] += unitNormal * InteriorAngle(c, a, b);
            }
            else  // "faceweighted" (default)
            {
                // Area weighting comes for free from the un-normalised cross.
                accum[iA] += faceNormalAreaWeighted;
                accum[iB] += faceNormalAreaWeighted;
                accum[iC] += faceNormalAreaWeighted;
            }
        }

        faceStart += faceSize;
    }

    // Normalise into the final per-point normals.
    VtVec3fArray normals(pointCount);
    for (int i = 0; i < pointCount; ++i)
    {
        GfVec3f n = accum[i];
        if (n.GetLengthSq() > 1e-20f)
            n.Normalize();
        else
            n = GfVec3f(0, 0, 1);  // isolated / degenerate point fallback
        
        // flip the normal for leftHanded winding order, as they would point inwards otherwise.
        if (leftHanded) n = n * -1.0f;
        normals[i] = n;
    }

    // Author as vertex-interpolated schema normals.
    mesh.CreateNormalsAttr().Set(normals, time);
    mesh.SetNormalsInterpolation(UsdGeomTokens->vertex);

    IDTX_LOG(IDTX_DEBUG, "  Authored {} vertex normals on '{}'",
        normals.size(), primPath);
    return true;
}

// ---------------------------------------------------------------------------
// Process a single USD stage file.
// ---------------------------------------------------------------------------
bool ProcessStage(
    const std::string&    inputPath,
    const std::string&    outputDir,
    const NormalsOptions& opts,
    bool                  dryRun)
{
    IDTX_LOG(IDTX_INFO, "Processing '{}'", inputPath);

    UsdStageRefPtr stage = UsdStage::Open(inputPath, UsdStage::LoadNone);
    if (!stage)
    {
        IDTX_LOG(IDTX_ERROR, "  Failed to open USD stage '{}'", inputPath);
        return false;
    }

    // Instances (native or pseudo) are skipped; their unique prototype/template
    // mesh is processed exactly once so normals are authored on the shared
    // geometry rather than on each instance.
    idtx::utils::TraversalOptions travOpts;  // defaults = SkipAndCollectPrototypes
    if (opts.includeReferenced)
        travOpts.referencedPolicy =
            idtx::utils::ReferencedPrimPolicy::IncludeReferencedAndPayloaded;

    const idtx::utils::TraversalResult res =
        idtx::utils::TraverseMeshLike<UsdGeomMesh>(
            stage, travOpts,
            [&](const UsdPrim& prim, const idtx::utils::PrimVisitContext& /*ctx*/) {
                UsdGeomMesh mesh(prim);
                IDTX_LOG(IDTX_DEBUG, "  Computing normals for mesh prim '{}'",
                    prim.GetPath().GetString());
                return ComputeNormalsForMesh(mesh, opts);
            });

    const int meshCount    = res.processed + res.failures;
    const int failureCount = res.failures;

    if (meshCount == 0)
    {
        IDTX_LOG(IDTX_WARN, "  No UsdGeomMesh prims found in '{}'", inputPath);
        return true;
    }

    IDTX_LOG(IDTX_INFO, "  Processed normals for {}/{} mesh(es)",
        meshCount - failureCount, meshCount);

    if (dryRun)
    {
        IDTX_LOG(IDTX_INFO, "  [dry-run] Skipping export");
        return failureCount == 0;
    }

    // Output path: <outputDir>/<stem>_normals.<ext>
    const bool exported = ExportStage(stage, inputPath, outputDir, "_normals");

    return failureCount == 0 && exported;
}

} // anonymous namespace

// ===========================================================================
// Public API
// ===========================================================================

CLI::App* RegisterNormalsCommand(CLI::App& app, NormalsOptions& opts)
{
    auto* sub = app.add_subcommand("normals",
        "Calculate mesh normals\n"
        "  faceweighted  - weight normals by polygon face area\n"
        "  angleweighted - weight normals by vertex interior angle");

    sub->add_option("--algorithm", opts.algorithm,
            "Normals calculation algorithm to use")
        ->required()
        ->type_name("faceweighted|angleweighted")
        ->check(CLI::IsMember({"faceweighted", "angleweighted"}));

    sub->add_option("--behaviour", opts.behaviour,
            "How to handle existing normals (default: preserve)\n"
            "  preserve  - keep existing, only fill missing\n"
            "  overwrite - recalculate and replace all normals")
        ->type_name("preserve|overwrite")
        ->check(CLI::IsMember({"preserve", "overwrite"}));

    sub->add_flag("--include-referenced", opts.includeReferenced,
        "Also process prims that are brought into the stage via "
        "reference/payload arcs (default: only prims authored on the root "
        "layer are modified)");

    return sub;
}

int RunNormalsCommand(
    const std::vector<std::string>& inputFiles,
    const std::string&              outputDir,
    const NormalsOptions&           opts,
    bool                            dryRun)
{
    IDTX_LOG(IDTX_INFO, "normals  algorithm={}  behaviour={}",
        opts.algorithm, opts.behaviour);

    int failures = 0;
    for (const auto& inputPath : inputFiles)
    {
        if (!ProcessStage(inputPath, outputDir, opts, dryRun))
            ++failures;
    }

    if (failures > 0)
        IDTX_LOG(IDTX_ERROR, "{} file(s) failed to compute normals", failures);

    return failures == 0 ? 0 : 1;
}

} // namespace idtx::commands
