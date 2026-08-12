/**
 * @file triangulate.cpp
 * @brief Implementation of the "triangulate" CLI subcommand.
 *
 * Triangulates every UsdGeomMesh prim in the given USD file(s).
 *
 * Algorithms
 * ----------
 * fan
 *   For each N-gon [v0, v1, ..., vN-1] emit triangles:
 *     (v0, v1, v2), (v0, v2, v3), ..., (v0, vN-2, vN-1)
 *   O(N) per polygon. Works for convex polygons. No vertex positions needed.
 *
 * beauty
 *   Greedy ear-clipping: at each step remove the ear whose triangle has the
 *   highest minimum interior angle. Falls back to fan when vertex positions
 *   are unavailable or a vertex index is out of range.
 *
 **/

#include "commands/triangulate.h"

#include <algorithm>
#include <cmath>
#include <cstdio>
#include <filesystem>
#include <string>
#include <vector>

#include <pxr/usd/sdf/assetPath.h>
#include <pxr/usd/sdf/layer.h>
#include <pxr/usd/usd/stage.h>
#include <pxr/usd/usd/primRange.h>
#include <pxr/usd/usdGeom/mesh.h>
#include <pxr/usd/usdGeom/primvarsAPI.h>
#include <pxr/usd/usdGeom/subset.h>
#include <pxr/base/vt/array.h>
#include <pxr/base/gf/vec3f.h>

#include <idtx/utils/Logger.h>

#include "utils/StageExport.h"
#include "utils/stageutils.h"
#include "utils/UsdzRepackage.h"

namespace fs = std::filesystem;
PXR_NAMESPACE_USING_DIRECTIVE

IDTX_LOG_CATEGORY("Triangulate")

namespace idtx::commands {

namespace {

// ---------------------------------------------------------------------------
// Fan triangulation
// ---------------------------------------------------------------------------
void FanTriangulateFace(
    int               faceIdx,
    int               faceStart,
    int               faceSize,
    const VtIntArray& srcIndices,
    VtIntArray&       dstIndices,
    VtIntArray&       dstCounts,
    std::vector<int>& faceMapping,
    std::vector<int>& faceVaryingIndex,
    bool              isLeftHanded = false)
{
    const int v0 = srcIndices[faceStart];
    for (int k = 1; k < faceSize - 1; ++k)
    {
        if (isLeftHanded)
        {
            // adjust index order to rightHanded winding
            dstIndices.push_back(v0);
            dstIndices.push_back(srcIndices[faceStart + k + 1]);
            dstIndices.push_back(srcIndices[faceStart + k]);
            // store the original poly index used for this face
            faceVaryingIndex.push_back(0);
            faceVaryingIndex.push_back(k + 1);
            faceVaryingIndex.push_back(k);
        } else
        {
            dstIndices.push_back(v0);
            dstIndices.push_back(srcIndices[faceStart + k]);
            dstIndices.push_back(srcIndices[faceStart + k + 1]);
            // store the original poly index used for this face
            faceVaryingIndex.push_back(0);
            faceVaryingIndex.push_back(k);
            faceVaryingIndex.push_back(k + 1);
        }
        dstCounts.push_back(3);
        // all faces created here are derived from the original faceIndex
        faceMapping.push_back(faceIdx);
    }
}

float MinTriangleAngle(const GfVec3f& p0, const GfVec3f& p1, const GfVec3f& p2)
{
    const GfVec3f e01 = p1 - p0;
    const GfVec3f e12 = p2 - p1;
    const GfVec3f e20 = p0 - p2;

    const float len01 = e01.GetLength();
    const float len12 = e12.GetLength();
    const float len20 = e20.GetLength();

    if (len01 < 1e-9f || len12 < 1e-9f || len20 < 1e-9f)
        return 0.f;

    const GfVec3f n01 = e01 / len01;
    const GfVec3f n12 = e12 / len12;
    const GfVec3f n20 = e20 / len20;

    const float a0 = std::acos(std::clamp(GfDot(-n20,  n01), -1.f, 1.f));
    const float a1 = std::acos(std::clamp(GfDot(-n01,  n12), -1.f, 1.f));
    const float a2 = std::acos(std::clamp(GfDot(-n12,  n20), -1.f, 1.f));

    return std::min({a0, a1, a2});
}
    
// ---------------------------------------------------------------------------
// Beauty (angle-optimised ear-clipping) triangulation
// ---------------------------------------------------------------------------
void BeautyTriangulateFace(
    int                  faceIdx,
    int                  faceStart,
    int                  faceSize,
    const VtIntArray&    srcIndices,
    const VtVec3fArray&  points,
    VtIntArray&          dstIndices,
    VtIntArray&          dstCounts,
    std::vector<int>&    faceMapping,
    std::vector<int>&    faceVaryingIndex,
    bool                 isLeftHanded = false)
{
    // if only faces and no points are provided, fall back to fan-triangulation, even though a mesh with faces,
    // but no points shall be treated as error (checked by the caller already). But we keep it here to be save.
    if (points.empty() || faceSize < 3)
    {
        FanTriangulateFace(faceIdx, faceStart, faceSize, srcIndices, dstIndices, dstCounts, faceMapping, faceVaryingIndex);
        return;
    }

    // store the indices of the current face as polygon
    std::vector<int> poly(faceSize);
    for (int i = 0; i < faceSize; ++i)
        poly[i] = srcIndices[faceStart + i];

    // sanity check of the polygon to ensure the points it addresses are valid
    // otherwise fall back to fan-triangulation
    const int ptCount = static_cast<int>(points.size());
    for (const int idx : poly)
    {
        if (idx < 0 || idx >= ptCount)
        {
            IDTX_LOG(IDTX_WARN,
                "  [beauty] vertex index {} out of range (points.size()={}) "
                "- falling back to fan", idx, ptCount);
            FanTriangulateFace(faceIdx, faceStart, faceSize, srcIndices, dstIndices, dstCounts, faceMapping, faceVaryingIndex);
            return;
        }
    }
    
    // origIdx[i] tracks the stable within-face offset of poly[i] in the original face.
    // As ears are clipped and poly[] shrinks, origIdx[] shrinks in sync so we always
    // know which original face-varying slot each surviving vertex maps to.
    std::vector<int> origIdx(faceSize);
    for (int i = 0; i < faceSize; ++i)
        origIdx[i] = i;

    while (static_cast<int>(poly.size()) > 3)
    {
        const int n         = static_cast<int>(poly.size());
        float     bestAngle = -1.f;
        int       bestEar   = 0;
        
        for (int i = 0; i < n; ++i)
        {
            const int iPrev = (i + n - 1) % n;
            const int iNext = (i + 1)     % n;

            const float minA = MinTriangleAngle(
                points[poly[iPrev]],
                points[poly[i]],
                points[poly[iNext]]);

            if (minA > bestAngle)
            {
                bestAngle = minA;
                bestEar   = i;
            }
        }

        const int earPrev = (bestEar + n - 1) % n;
        const int earNext = (bestEar + 1)     % n;

        if (isLeftHanded)
        {
            dstIndices.push_back(poly[earNext]);
            dstIndices.push_back(poly[bestEar]);
            dstIndices.push_back(poly[earPrev]);  
        } else
        {
            dstIndices.push_back(poly[earPrev]);
            dstIndices.push_back(poly[bestEar]);
            dstIndices.push_back(poly[earNext]);    
        }
        dstCounts.push_back(3);
        // all faces created here are derived from the original faceIndex
        faceMapping.push_back(faceIdx);
        // use origIdx to get the stable original within-face offset for each vertex,
        // NOT the current position in the shrinking poly[] array
        if (isLeftHanded)
        {
            faceVaryingIndex.push_back(origIdx[earNext]);
            faceVaryingIndex.push_back(origIdx[bestEar]);
            faceVaryingIndex.push_back(origIdx[earPrev]);   
        } else
        {
            faceVaryingIndex.push_back(origIdx[earPrev]);
            faceVaryingIndex.push_back(origIdx[bestEar]);
            faceVaryingIndex.push_back(origIdx[earNext]);    
        }
        // remove the clipped ear from both arrays so they stay in sync
        poly.erase(poly.begin() + bestEar);
        origIdx.erase(origIdx.begin() + bestEar);
    }

    if (isLeftHanded)
    {
        dstIndices.push_back(poly[2]);
        dstIndices.push_back(poly[1]);
        dstIndices.push_back(poly[0]);
    } else
    {
        dstIndices.push_back(poly[0]);
        dstIndices.push_back(poly[1]);
        dstIndices.push_back(poly[2]);    
    }
    dstCounts.push_back(3);
    // all faces created here are derived from the original faceIndex
    faceMapping.push_back(faceIdx);
    // use origIdx for the final triangle — poly[0..2] are the three remnant vertices
    // whose original face offsets are tracked in origIdx[0..2]
    if (isLeftHanded)
    {
        faceVaryingIndex.push_back(origIdx[2]);
        faceVaryingIndex.push_back(origIdx[1]);
        faceVaryingIndex.push_back(origIdx[0]);
    } else {
        faceVaryingIndex.push_back(origIdx[0]);
        faceVaryingIndex.push_back(origIdx[1]);
        faceVaryingIndex.push_back(origIdx[2]);
    }
}
    
template <typename ArrayType>
void RemapFaceVaryingData(
    UsdGeomPrimvar& primvar,
    const std::vector<int>& originalFaceVaryingOffsets,
    const std::vector<int>& faceMapping,
    const std::vector<int>& faceVaryingIndex,
    const UsdTimeCode& timeCode
)
{
    // Use ComputeFlattened() rather than Get() so that indexed primvars are
    // expanded to one value per face-vertex corner before we remap them.
    ArrayType flatData;
    if (!primvar.ComputeFlattened(&flatData, timeCode))
        return;

    ArrayType targetData;
    targetData.reserve(faceMapping.size() * 3);

    for (size_t triIdx = 0; triIdx < faceMapping.size(); ++triIdx)
    {
        // get the original face from the new one
        int faceIndex  = faceMapping[triIdx];
        int faceOffset = originalFaceVaryingOffsets[faceIndex];

        targetData.push_back(flatData[faceOffset + faceVaryingIndex[triIdx * 3]]);
        targetData.push_back(flatData[faceOffset + faceVaryingIndex[triIdx * 3 + 1]]);
        targetData.push_back(flatData[faceOffset + faceVaryingIndex[triIdx * 3 + 2]]);
    }

    // Write the remapped flat values back.
    primvar.Set(targetData, timeCode);

    // If the primvar had an indices array, clear it: the remapped data is now
    // flat (one value per face-vertex) and the old indices no longer apply.
    UsdAttribute indicesAttr = primvar.GetIndicesAttr();
    if (indicesAttr && indicesAttr.HasAuthoredValue())
        indicesAttr.Clear();
}

void RemapFaceVaryingPrimvars(
    const UsdGeomMesh& mesh,
    const VtIntArray& originalFaceVertexCounts,
    const std::vector<int>& faceMapping,
    const std::vector<int>& faceVaryingIndex)
{
    const UsdTimeCode time = UsdTimeCode::Default();
    
    UsdGeomPrimvarsAPI primvarsAPI(mesh.GetPrim());
    std::vector<UsdGeomPrimvar> primvars = primvarsAPI.GetPrimvars();
    
    // Build the old face-varying offset table (one entry per original face,
    // value = sum of all face-vertex counts before that face).
    std::vector<int> faceVaryingOffsets;
    faceVaryingOffsets.reserve(originalFaceVertexCounts.size());
    int offset = 0;
    for (int count : originalFaceVertexCounts) {
        faceVaryingOffsets.push_back(offset);
        offset += count;
    }

    for (auto& primvar : primvars)
    {
        if (primvar.GetInterpolation() != UsdGeomTokens->faceVarying)
            continue;

        // Probe the type by computing a flattened sample; skip if unavailable.
        // We check the authored type token to dispatch to the right ArrayType.
        const SdfValueTypeName typeName = primvar.GetTypeName();

        if (typeName == SdfValueTypeNames->FloatArray ||
            typeName == SdfValueTypeNames->Float) {
            RemapFaceVaryingData<VtFloatArray>(
                primvar, faceVaryingOffsets, faceMapping, faceVaryingIndex, time);
        } else if (typeName == SdfValueTypeNames->TexCoord2fArray ||
                   typeName == SdfValueTypeNames->Float2Array) {
            RemapFaceVaryingData<VtVec2fArray>(
                primvar, faceVaryingOffsets, faceMapping, faceVaryingIndex, time);
        } else if (typeName == SdfValueTypeNames->Normal3fArray ||
                   typeName == SdfValueTypeNames->Point3fArray  ||
                   typeName == SdfValueTypeNames->Vector3fArray ||
                   typeName == SdfValueTypeNames->Float3Array   ||
                   typeName == SdfValueTypeNames->Color3fArray) {
            RemapFaceVaryingData<VtVec3fArray>(
                primvar, faceVaryingOffsets, faceMapping, faceVaryingIndex, time);
        } else {
            // Fallback: attempt to read via VtValue and dispatch on held type.
            VtValue value;
            primvar.Get(&value, time);
            if (value.IsEmpty()) continue;

            if (value.IsHolding<VtFloatArray>()) {
                RemapFaceVaryingData<VtFloatArray>(
                    primvar, faceVaryingOffsets, faceMapping, faceVaryingIndex, time);
            } else if (value.IsHolding<VtVec2fArray>()) {
                RemapFaceVaryingData<VtVec2fArray>(
                    primvar, faceVaryingOffsets, faceMapping, faceVaryingIndex, time);
            } else if (value.IsHolding<VtVec3fArray>()) {
                RemapFaceVaryingData<VtVec3fArray>(
                    primvar, faceVaryingOffsets, faceMapping, faceVaryingIndex, time);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Per-mesh triangulation: fan / beauty
// ---------------------------------------------------------------------------
bool TriangulateMesh(UsdGeomMesh& mesh, const std::string& algorithm)
{
    const UsdTimeCode time = UsdTimeCode::Default();

    VtIntArray faceVertexCounts, faceVertexIndices;
    if (!mesh.GetFaceVertexCountsAttr().Get(&faceVertexCounts,  time) ||
        !mesh.GetFaceVertexIndicesAttr().Get(&faceVertexIndices, time))
    {
        IDTX_LOG(IDTX_WARN,
            "Cannot read face topology from '{}' - skipping",
            mesh.GetPath().GetString());
        return false;
    }

    VtVec3fArray points;
    if (!mesh.GetPointsAttr().Get(&points, time))
    {
        IDTX_LOG(IDTX_ERROR, "Cannot read points from '{}' - skipping",
            mesh.GetPath().GetString());
        return false;
    }
    
    bool leftHanded = utils::IsLeftHanded(mesh);

    VtIntArray newCounts, newIndices;
    newCounts .reserve(faceVertexCounts.size() * 2);
    newIndices.reserve(faceVertexIndices.size());
    
    // Store original face counts for primvar adjustment
    VtIntArray originalFaceVertexCounts = faceVertexCounts;
    // Store mapping from new face index to original one
    std::vector<int> faceMapping;
    std::vector<int> faceVaryingIndex;

    int faceStart = 0;
    for (size_t faceIdx = 0; faceIdx < faceVertexCounts.size(); ++faceIdx)
    {
        int faceSize = faceVertexCounts[faceIdx];
        if (faceSize < 3)
        {
            // we will ignore faces with less than 3 points
            faceStart += faceSize;
            continue;
        }

        if (faceSize == 3)
        {
            // already a triangle face, just take over as is (but turn winding order from leftHanded to rightHanded
            if (leftHanded)
            {
                newIndices.push_back(faceVertexIndices[faceStart + 2]);
                newIndices.push_back(faceVertexIndices[faceStart + 1]);
                newIndices.push_back(faceVertexIndices[faceStart]);
                
                // store the original poly index used for this face
                faceVaryingIndex.push_back(2);
                faceVaryingIndex.push_back(1);
                faceVaryingIndex.push_back(0);
            } else
            {
                newIndices.push_back(faceVertexIndices[faceStart]);
                newIndices.push_back(faceVertexIndices[faceStart + 1]);
                newIndices.push_back(faceVertexIndices[faceStart + 2]);
                
                // store the original poly index used for this face
                faceVaryingIndex.push_back(0);
                faceVaryingIndex.push_back(1);
                faceVaryingIndex.push_back(2);
            }
            
            newCounts.push_back(faceSize);
            // store this face index as is
            faceMapping.push_back(faceIdx);
            
        }
        // all faces with more then 3 points are triangulated based on the algorythm
        else if (algorithm == "beauty")
        {
            BeautyTriangulateFace(faceIdx, faceStart, faceSize, faceVertexIndices, points,
                                  newIndices, newCounts, faceMapping, faceVaryingIndex,
                                  leftHanded);
        }
        else // "fan" (default)
        {
            FanTriangulateFace(faceIdx, faceStart, faceSize, faceVertexIndices, 
                               newIndices, newCounts, faceMapping, faceVaryingIndex,
                               leftHanded);
        }

        faceStart += faceSize;
    }

    // update the attributes in the prim
    mesh.GetFaceVertexCountsAttr().Set(newCounts,  time);
    mesh.GetFaceVertexIndicesAttr().Set(newIndices,  time);
    mesh.GetOrientationAttr().Set(UsdGeomTokens->rightHanded, time);
    // remove the subdivision attribute to ensure, renderer does not try to subdivide the faces as well
    mesh.GetSubdivisionSchemeAttr().Set(UsdGeomTokens->none, time);
    
    // ensure we adjust the primvars that are using face-varying interpolation after the triangulation has been applied
    RemapFaceVaryingPrimvars(mesh, originalFaceVertexCounts, faceMapping, faceVaryingIndex);
    
    // if the GeomMesh contains GeomSubsets we need to adjust them as well if they used face indices
    for (const UsdGeomSubset& subset : UsdGeomSubset::GetAllGeomSubsets(mesh))
    {
        class TfToken subsetType;
        // if the subset does not define it's type (face, edge, point) we can't handle it
        if (!subset.GetElementTypeAttr().Get(&subsetType)) continue;

        // we only need to adjust subsets of type "Face".
        if (subsetType != UsdGeomTokens->face) continue;
        VtArray<int> subsetFaces;
        // if the subset does not define any faces we can't handle it
        if (!subset.GetIndicesAttr().Get(&subsetFaces)) continue;
        VtIntArray newSubsetFaces;
        // reserve an estimated size of new subset faces to reduce the memory re-allocations
        newSubsetFaces.reserve(subsetFaces.size() * 3);
        // get the new face-indices for the actually stored ones and create a new face index list for this subset
        for (int faceIdx: subsetFaces)
        {
            auto it = faceMapping.cbegin();
            while ((it = std::find(it, faceMapping.cend(), faceIdx)) != faceMapping.cend())
            {
                newSubsetFaces.push_back(
                    static_cast<int>(std::distance(faceMapping.cbegin(), it))
                );
                ++it;
            }
        }
        // author the new face index list into the subset
        subset.GetIndicesAttr().Set(newSubsetFaces, time);
    }
    
    return true;
}

// ---------------------------------------------------------------------------
// Process a single USD stage file
// ---------------------------------------------------------------------------

bool ProcessStage(
    const std::string&        inputPath,
    const std::string&        outputDir,
    const TriangulateOptions& opts,
    bool                      dryRun)
{
    IDTX_LOG(IDTX_INFO, "Processing '{}'", inputPath);

    // when opening the stage, ensure we never load payload arcs, as we do not want to follow them anyway.
    UsdStageRefPtr stage = UsdStage::Open(inputPath, UsdStage::LoadNone);
    if (!stage)
    {
        IDTX_LOG(IDTX_ERROR, "  Failed to open USD stage '{}'", inputPath);
        return false;
    }

    // Instances (native or pseudo) reference a prototype/"template" prim
    // (usually authored as `over`) for their mesh data. The shared traversal
    // helper skips the instance prims themselves and triangulates each unique
    // prototype mesh exactly once afterwards so that downstream commands (e.g.
    // reduce) always see triangles.
    idtx::utils::TraversalOptions travOpts;  // defaults = SkipAndCollectPrototypes
    // By default only prims authored on the root layer are modified; the
    // caller can opt into editing referenced/payloaded prims explicitly.
    if (opts.includeReferenced)
        travOpts.referencedPolicy =
            idtx::utils::ReferencedPrimPolicy::IncludeReferencedAndPayloaded;
    const idtx::utils::TraversalResult res =
        idtx::utils::TraverseMeshLike<UsdGeomMesh>(
            stage, travOpts,
            [&](const UsdPrim& prim, const idtx::utils::PrimVisitContext& /*ctx*/) {
                UsdGeomMesh mesh(prim);
                IDTX_LOG(IDTX_DEBUG, "  Triangulating mesh prim '{}'",
                    prim.GetPath().GetString());
                return TriangulateMesh(mesh, opts.algorithm);
            });

    const int meshCount    = res.processed + res.failures;
    const int failureCount = res.failures;

    if (meshCount == 0)
    {
        IDTX_LOG(IDTX_WARN, "  No UsdGeomMesh prims found in '{}'", inputPath);
        return true;
    }

    IDTX_LOG(IDTX_INFO, "  Triangulated {}/{} mesh(es)",
        meshCount - failureCount, meshCount);

    if (dryRun)
    {
        IDTX_LOG(IDTX_INFO, "  [dry-run] Skipping export");
        return failureCount == 0;
    }

    // Output path: <outputDir>/<stem>_triangulated.<ext>
    bool exported = ExportStage(stage, inputPath, outputDir, "_triangulated");
    
    return failureCount == 0 && exported;
}

} // anonymous namespace

// ===========================================================================
// Public API
// ===========================================================================

CLI::App* RegisterTriangulateCommand(CLI::App& app, TriangulateOptions& opts)
{
    auto* sub = app.add_subcommand("triangulate",
        "Triangulate mesh geometry\n"
        "  fan    - simple fan triangulation from the first vertex (default)\n"
        "  beauty - angle-optimised ear-clipping for improved mesh quality\n");

    sub->add_option("--algorithm", opts.algorithm,
            "Triangulation algorithm to use")
        ->required()
        ->type_name("fan|beauty")
        ->check(CLI::IsMember({"fan", "beauty"}));

    sub->add_flag("--include-referenced", opts.includeReferenced,
        "Also process prims that are brought into the stage via "
        "reference/payload arcs (default: only prims authored on the root "
        "layer are modified)");

    return sub;
}

int RunTriangulateCommand(
    const std::vector<std::string>& inputFiles,
    const std::string&              outputDir,
    const TriangulateOptions&       opts,
    bool                            dryRun)
{
    IDTX_LOG(IDTX_INFO, "triangulate  algorithm={}", opts.algorithm);

    int failures = 0;
    for (const auto& inputPath : inputFiles)
    {
        if (!ProcessStage(inputPath, outputDir, opts, dryRun))
            ++failures;
    }

    if (failures > 0)
        IDTX_LOG(IDTX_ERROR, "{} file(s) failed to triangulate", failures);

    return failures == 0 ? 0 : 1;
}

} // namespace idtx::commands
