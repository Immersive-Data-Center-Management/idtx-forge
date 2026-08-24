/**
 * @file tangents.cpp
 * @brief Implementation of the "tangents" CLI subcommand.
 *
 * Calculates tangent space vectors for every UsdGeomMesh prim in the given
 * USD file(s). Tangent space is required for correct normal mapping.
 *
 * Algorithms
 * ----------
 * mikktspace
 *   Industry-standard MikkTSpace algorithm (recommended). Produces tangents
 *   consistent with most DCC tools and game engines, avoiding seams and
 *   mirroring artefacts. Implemented via the vendored reference implementation
 *   under source/thirdparty/mikktspace.
 *
 * gramschmidt
 *   Gram-Schmidt orthogonalisation of the tangent basis derived from the UV
 *   gradients (Lengyel's method). Faster but less robust for complex meshes.
 *
 * Tangent calculation requires the mesh to have UV coordinates (texture
 * coordinates) and normals present.
 *
 * Output
 * ------
 * Tangents are authored as a face-varying `primvars:tangents` primvar of type
 * float4[]. The xyz components hold the unit tangent; the w component holds the
 * handedness sign (+1 or -1) so the bitangent can be reconstructed as
 * `w * cross(normal, tangent)`. This is the glTF / common game-engine
 * convention.
 *
 * Orientation
 * -----------
 * The mesh's `orientation` property ("rightHanded" or "leftHanded") affects
 * the winding order interpretation. Both MikkTSpace and Gram-Schmidt assume
 * rightHanded (counter-clockwise) winding. When a mesh is leftHanded, the
 * triangle corners are emitted in reversed order so the tangent algorithms
 * see consistent rightHanded triangles. The tangent output is then written
 * back in the reversed order matching the mesh's actual face-varying layout.
 *
 * Only prototype/template meshes and ordinary (non-instanced) meshes are
 * processed - the shared traversal helper skips instance prims and processes
 * each unique prototype exactly once so tangents are authored on the shared
 * geometry rather than on every instance.
 **/

#include "commands/tangents.h"

#include <cmath>
#include <string>
#include <vector>

#include <pxr/base/gf/vec2f.h>
#include <pxr/base/gf/vec3f.h>
#include <pxr/base/gf/vec4f.h>
#include <pxr/base/vt/array.h>
#include <pxr/usd/usd/stage.h>
#include <pxr/usd/usdGeom/mesh.h>
#include <pxr/usd/usdGeom/primvarsAPI.h>
#include <pxr/usd/usdGeom/tokens.h>

#include <idtx/utils/Logger.h>

#include "thirdparty/mikktspace/mikktspace.h"
#include "utils/StageExport.h"
#include "utils/stageutils.h"

PXR_NAMESPACE_USING_DIRECTIVE

IDTX_LOG_CATEGORY("Tangents")

namespace idtx::commands {

namespace {

// The token under which we author the tangents primvar.
const TfToken kTangentsPrimvarName("tangents");

// ---------------------------------------------------------------------------
// MeshData: flattened, per-triangle-corner view of a mesh.
//
// MikkTSpace and the gram-schmidt path both operate on triangle corners. We
// build one entry per corner (size == 3 * numTriangles) so face-varying
// tangents can be written back directly in the same order.
// ---------------------------------------------------------------------------
struct MeshData
{
    std::vector<GfVec3f> positions;   ///< corner position
    std::vector<GfVec3f> normals;     ///< corner normal (unit length)
    std::vector<GfVec2f> uvs;         ///< corner uv
    std::vector<GfVec4f> tangents;    ///< output: tangent xyz + handedness w

    /// When the source mesh is leftHanded, corners are emitted in reversed
    /// order so the tangent algorithms see rightHanded triangles. This flag
    /// signals that the output tangents need to be un-reversed before being
    /// written back as face-varying data matching the mesh's topology.
    bool isLeftHanded = false;

    [[nodiscard]] int numFaces() const
    {
        return static_cast<int>(positions.size() / 3);
    }
};

// ---------------------------------------------------------------------------
// Fetch helpers that resolve a value by USD interpolation. `fvIdx` is the
// running face-varying corner index (matches ComputeFlattened output order);
// `pointIdx` is the mesh point index for vertex/varying interpolation.
// ---------------------------------------------------------------------------
GfVec3f LookupVec3(const VtVec3fArray& arr, const TfToken& interp,
                   int pointIdx, int fvIdx)
{
    if (arr.empty())
        return GfVec3f(0);
    if (interp == UsdGeomTokens->faceVarying)
        return (fvIdx < static_cast<int>(arr.size())) ? arr[fvIdx] : GfVec3f(0);
    if (interp == UsdGeomTokens->constant || interp == UsdGeomTokens->uniform)
        return arr[0];
    // vertex / varying
    return (pointIdx < static_cast<int>(arr.size())) ? arr[pointIdx] : GfVec3f(0);
}

GfVec2f LookupVec2(const VtVec2fArray& arr, const TfToken& interp,
                   int pointIdx, int fvIdx)
{
    if (arr.empty())
        return GfVec2f(0);
    if (interp == UsdGeomTokens->faceVarying)
        return (fvIdx < static_cast<int>(arr.size())) ? arr[fvIdx] : GfVec2f(0);
    if (interp == UsdGeomTokens->constant || interp == UsdGeomTokens->uniform)
        return arr[0];
    return (pointIdx < static_cast<int>(arr.size())) ? arr[pointIdx] : GfVec2f(0);
}

// ---------------------------------------------------------------------------
// Read a mesh's topology + points + normals + uvs and flatten them into a
// per-corner (face-varying) triangle-corner layout.
//
// Returns false when a required attribute is missing/unreadable; the mesh is
// then skipped by the caller. Only triangles and quads are supported (quads
// are fan-split into two triangles); higher-order polygons cause the mesh to
// be skipped with a warning (run `triangulate` first).
//
// When the mesh's orientation is "leftHanded", triangle corners are emitted
// in reversed order (2,1,0 instead of 0,1,2) so that the tangent algorithms
// (MikkTSpace / Gram-Schmidt) see consistent rightHanded winding. The
// `isLeftHanded` flag on MeshData signals that the output needs to be
// un-reversed when authored back onto the mesh.
// ---------------------------------------------------------------------------
bool BuildMeshData(const UsdGeomMesh& mesh, MeshData* out)
{
    const UsdTimeCode time = UsdTimeCode::Default();
    const std::string primPath = mesh.GetPath().GetString();

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

    UsdGeomPrimvarsAPI primvarsAPI(mesh.GetPrim());

    // --- Normals: accept schema `normals` or `primvars:normals` ------------
    VtVec3fArray normals;
    TfToken      normalsInterp;
    {
        UsdGeomPrimvar normalsPv =
            primvarsAPI.GetPrimvar(UsdGeomTokens->normals);
        if (normalsPv && normalsPv.HasValue())
        {
            normalsPv.ComputeFlattened(&normals, time);
            normalsInterp = normalsPv.GetInterpolation();
        }
        else if (mesh.GetNormalsAttr().HasAuthoredValue())
        {
            mesh.GetNormalsAttr().Get(&normals, time);
            normalsInterp = mesh.GetNormalsInterpolation();
        }
    }
    if (normals.empty())
    {
        IDTX_LOG(IDTX_WARN,
            "  Mesh '{}' has no normals - run the 'normals' command first; "
            "skipping", primPath);
        return false;
    }

    // --- UVs: the common texture-coordinate primvar names ------------------
    VtVec2fArray uvs;
    TfToken      uvInterp;
    {
        static const TfToken candidates[] = {
            TfToken("st"), TfToken("st0"), TfToken("uv"), TfToken("map1")
        };
        for (const TfToken& name : candidates)
        {
            UsdGeomPrimvar uvPv = primvarsAPI.GetPrimvar(name);
            if (uvPv && uvPv.HasValue())
            {
                uvPv.ComputeFlattened(&uvs, time);
                uvInterp = uvPv.GetInterpolation();
                break;
            }
        }
    }
    if (uvs.empty())
    {
        IDTX_LOG(IDTX_WARN,
            "  Mesh '{}' has no UV/texture coordinates (st/uv) - required for "
            "tangents; skipping", primPath);
        return false;
    }

    // Determine orientation
    out->isLeftHanded = utils::IsLeftHanded(mesh);

    const int pointCount = static_cast<int>(points.size());

    // emitCorner appends one corner using the running face-varying index
    // (faceVertexOffset) to resolve face-varying attributes.
    auto emitCorner = [&](int faceVertexOffset)
    {
        const int pointIdx = faceVertexIndices[faceVertexOffset];
        if (pointIdx < 0 || pointIdx >= pointCount)
        {
            out->positions.emplace_back(0.0f, 0.0f, 0.0f);
            out->normals.emplace_back(0.0f, 0.0f, 1.0f);
            out->uvs.emplace_back(0.0f, 0.0f);
            return;
        }
        out->positions.push_back(points[pointIdx]);

        GfVec3f n = LookupVec3(normals, normalsInterp, pointIdx, faceVertexOffset);
        if (n.GetLengthSq() > 0.0f)
            n.Normalize();
        else
            n = GfVec3f(0, 0, 1);
        out->normals.push_back(n);

        out->uvs.push_back(LookupVec2(uvs, uvInterp, pointIdx, faceVertexOffset));
    };

    // Helper: emit a triangle's three corners, reversing the order when the
    // mesh is leftHanded so that the tangent algorithms see rightHanded winding.
    auto emitTriangle = [&](int c0, int c1, int c2)
    {
        if (out->isLeftHanded)
        {
            emitCorner(c2);
            emitCorner(c1);
            emitCorner(c0);
        }
        else
        {
            emitCorner(c0);
            emitCorner(c1);
            emitCorner(c2);
        }
    };

    int faceStart = 0;  // running offset into faceVertexIndices == face-varying index base
    for (size_t f = 0; f < faceVertexCounts.size(); ++f)
    {
        const int faceSize = faceVertexCounts[f];

        if (faceSize == 3)
        {
            emitTriangle(faceStart + 0, faceStart + 1, faceStart + 2);
        }
        else if (faceSize == 4)
        {
            // fan-split the quad into (0,1,2) and (0,2,3)
            emitTriangle(faceStart + 0, faceStart + 1, faceStart + 2);
            emitTriangle(faceStart + 0, faceStart + 2, faceStart + 3);
        }
        else if (faceSize < 3)
        {
            // Degenerate face: nothing to contribute; just advance.
        }
        else
        {
            IDTX_LOG(IDTX_WARN,
                "  Mesh '{}' contains a {}-gon; run 'triangulate' first - "
                "skipping", primPath, faceSize);
            return false;
        }

        faceStart += faceSize;
    }

    if (out->positions.empty() || (out->positions.size() % 3) != 0)
    {
        IDTX_LOG(IDTX_WARN,
            "  Mesh '{}' produced no triangles - skipping", primPath);
        return false;
    }

    out->tangents.assign(out->positions.size(), GfVec4f(1, 0, 0, 1));
    return true;
}

// ===========================================================================
// MikkTSpace adapter
// ===========================================================================
// The MikkTSpace callbacks read straight from a MeshData instance stored in the
// context's user-data pointer. Positions/normals/uvs are already flattened per
// triangle corner, so the face/vertex addressing is trivial.

int Mikk_getNumFaces(const SMikkTSpaceContext* ctx)
{
    const auto* m = static_cast<const MeshData*>(ctx->m_pUserData);
    return m->numFaces();
}

int Mikk_getNumVerticesOfFace(const SMikkTSpaceContext* /*ctx*/, int /*iFace*/)
{
    return 3;  // BuildMeshData only ever emits triangles
}

void Mikk_getPosition(const SMikkTSpaceContext* ctx, float out[], int iFace, int iVert)
{
    const auto* m = static_cast<const MeshData*>(ctx->m_pUserData);
    const GfVec3f& p = m->positions[iFace * 3 + iVert];
    out[0] = p[0]; out[1] = p[1]; out[2] = p[2];
}

void Mikk_getNormal(const SMikkTSpaceContext* ctx, float out[], int iFace, int iVert)
{
    const auto* m = static_cast<const MeshData*>(ctx->m_pUserData);
    const GfVec3f& n = m->normals[iFace * 3 + iVert];
    out[0] = n[0]; out[1] = n[1]; out[2] = n[2];
}

void Mikk_getTexCoord(const SMikkTSpaceContext* ctx, float out[], int iFace, int iVert)
{
    const auto* m = static_cast<const MeshData*>(ctx->m_pUserData);
    const GfVec2f& t = m->uvs[iFace * 3 + iVert];
    out[0] = t[0]; out[1] = t[1];
}

void Mikk_setTSpaceBasic(const SMikkTSpaceContext* ctx, const float tangent[],
                         float fSign, int iFace, int iVert)
{
    auto* m = static_cast<MeshData*>(ctx->m_pUserData);
    // MikkTSpace's fSign convention: bitangent = fSign * cross(N, T).
    m->tangents[iFace * 3 + iVert] =
        GfVec4f(tangent[0], tangent[1], tangent[2], fSign);
}

bool ComputeTangentsMikkTSpace(MeshData* mesh)
{
    SMikkTSpaceInterface iface{};
    iface.m_getNumFaces          = Mikk_getNumFaces;
    iface.m_getNumVerticesOfFace = Mikk_getNumVerticesOfFace;
    iface.m_getPosition          = Mikk_getPosition;
    iface.m_getNormal            = Mikk_getNormal;
    iface.m_getTexCoord          = Mikk_getTexCoord;
    iface.m_setTSpaceBasic       = Mikk_setTSpaceBasic;
    iface.m_setTSpace            = nullptr;

    SMikkTSpaceContext ctx{};
    ctx.m_pInterface = &iface;
    ctx.m_pUserData  = mesh;

    return genTangSpaceDefault(&ctx) != 0;
}

// ===========================================================================
// Gram-Schmidt (Lengyel's method)
// ===========================================================================
// Accumulate per-corner tangent/bitangent from UV gradients, then orthonormalise
// each corner tangent against its normal and derive the handedness sign.
bool ComputeTangentsGramSchmidt(MeshData* mesh)
{
    const size_t n = mesh->positions.size();
    const int    numTris = mesh->numFaces();

    std::vector<GfVec3f> tan(n, GfVec3f(0));
    std::vector<GfVec3f> bit(n, GfVec3f(0));

    for (int f = 0; f < numTris; ++f)
    {
        const int i0 = f * 3 + 0;
        const int i1 = f * 3 + 1;
        const int i2 = f * 3 + 2;

        const GfVec3f& p0 = mesh->positions[i0];
        const GfVec3f& p1 = mesh->positions[i1];
        const GfVec3f& p2 = mesh->positions[i2];

        const GfVec2f& w0 = mesh->uvs[i0];
        const GfVec2f& w1 = mesh->uvs[i1];
        const GfVec2f& w2 = mesh->uvs[i2];

        const GfVec3f e1 = p1 - p0;
        const GfVec3f e2 = p2 - p0;

        const float du1 = w1[0] - w0[0];
        const float dv1 = w1[1] - w0[1];
        const float du2 = w2[0] - w0[0];
        const float dv2 = w2[1] - w0[1];

        const float denom = du1 * dv2 - du2 * dv1;
        const float r = (std::fabs(denom) > 1e-12f) ? (1.0f / denom) : 0.0f;

        const GfVec3f sdir(
            (dv2 * e1[0] - dv1 * e2[0]) * r,
            (dv2 * e1[1] - dv1 * e2[1]) * r,
            (dv2 * e1[2] - dv1 * e2[2]) * r);
        const GfVec3f tdir(
            (du1 * e2[0] - du2 * e1[0]) * r,
            (du1 * e2[1] - du2 * e1[1]) * r,
            (du1 * e2[2] - du2 * e1[2]) * r);

        // Accumulate onto all three corners of this triangle.
        for (int c = 0; c < 3; ++c)
        {
            tan[f * 3 + c] += sdir;
            bit[f * 3 + c] += tdir;
        }
    }

    for (size_t i = 0; i < n; ++i)
    {
        const GfVec3f& nrm = mesh->normals[i];
        GfVec3f t = tan[i];

        // Gram-Schmidt orthogonalise t against n.
        t = t - nrm * GfDot(nrm, t);
        if (t.GetLengthSq() > 1e-20f)
            t.Normalize();
        else
            t = GfVec3f(1, 0, 0);  // fall back to an arbitrary tangent

        // Handedness: sign of dot(cross(n, t), bitangent).
        const float w = (GfDot(GfCross(nrm, t), bit[i]) < 0.0f) ? -1.0f : 1.0f;

        mesh->tangents[i] = GfVec4f(t[0], t[1], t[2], w);
    }

    return true;
}

// ---------------------------------------------------------------------------
// Author the computed tangents onto the mesh as a face-varying float4 primvar
// named `primvars:tangents`.
//
// When the mesh was leftHanded, corners were emitted in reversed order during
// BuildMeshData so the tangent algorithms saw rightHanded winding. The
// computed tangents are therefore in reversed-corner order per triangle; we
// un-reverse them here so the authored face-varying data aligns with the
// mesh's actual topology (which still uses its original winding).
// ---------------------------------------------------------------------------
void AuthorTangents(const UsdGeomMesh& mesh, const MeshData& data)
{
    const UsdTimeCode time = UsdTimeCode::Default();

    VtVec4fArray tangents;
    tangents.resize(data.tangents.size());

    if (data.isLeftHanded)
    {
        // Un-reverse: for each triangle, swap corners 0 and 2 back to the
        // original face-varying order expected by the mesh topology.
        const int numTris = data.numFaces();
        for (int f = 0; f < numTris; ++f)
        {
            tangents[f * 3 + 0] = data.tangents[f * 3 + 2];
            tangents[f * 3 + 1] = data.tangents[f * 3 + 1];
            tangents[f * 3 + 2] = data.tangents[f * 3 + 0];
        }
    }
    else
    {
        tangents.assign(data.tangents.begin(), data.tangents.end());
    }

    UsdGeomPrimvarsAPI primvarsAPI(mesh.GetPrim());
    UsdGeomPrimvar pv = primvarsAPI.CreatePrimvar(
        kTangentsPrimvarName,
        SdfValueTypeNames->Float4Array,
        UsdGeomTokens->faceVarying);
    pv.Set(tangents, time);

    // Clear any stale index array so the flat values apply directly.
    UsdAttribute indicesAttr = pv.GetIndicesAttr();
    if (indicesAttr && indicesAttr.HasAuthoredValue())
        indicesAttr.Clear();
}

// ---------------------------------------------------------------------------
// Per-mesh tangent computation. Returns true on success (tangents authored),
// false when the mesh was skipped or the algorithm failed.
// ---------------------------------------------------------------------------
bool ProcessMesh(const UsdGeomMesh& mesh, const TangentsOptions& opts)
{
    MeshData data;
    if (!BuildMeshData(mesh, &data))
        return false;

    bool ok = false;
    if (opts.algorithm == "gramschmidt")
        ok = ComputeTangentsGramSchmidt(&data);
    else  // "mikktspace" (default / recommended)
        ok = ComputeTangentsMikkTSpace(&data);

    if (!ok)
    {
        IDTX_LOG(IDTX_WARN,
            "  Tangent generation failed for '{}' - skipping",
            mesh.GetPath().GetString());
        return false;
    }

    AuthorTangents(mesh, data);
    IDTX_LOG(IDTX_DEBUG, "  Authored {} tangents on '{}'",
        data.tangents.size(), mesh.GetPath().GetString());
    return true;
}

// ---------------------------------------------------------------------------
// Process a single USD stage file.
// ---------------------------------------------------------------------------
bool ProcessStage(
    const std::string&     inputPath,
    const std::string&     outputDir,
    const TangentsOptions& opts,
    bool                   dryRun)
{
    IDTX_LOG(IDTX_INFO, "Processing '{}'", inputPath);

    UsdStageRefPtr stage = UsdStage::Open(inputPath, UsdStage::LoadNone);
    if (!stage)
    {
        IDTX_LOG(IDTX_ERROR, "  Failed to open USD stage '{}'", inputPath);
        return false;
    }

    // Instances (native or pseudo) are skipped; their unique prototype/template
    // mesh is processed exactly once so tangents are authored on the shared
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
                IDTX_LOG(IDTX_DEBUG, "  Computing tangents for mesh prim '{}'",
                    prim.GetPath().GetString());
                return ProcessMesh(mesh, opts);
            });

    const int meshCount    = res.processed + res.failures;
    const int failureCount = res.failures;

    if (meshCount == 0)
    {
        IDTX_LOG(IDTX_WARN, "  No UsdGeomMesh prims found in '{}'", inputPath);
        return true;
    }

    IDTX_LOG(IDTX_INFO, "  Computed tangents for {}/{} mesh(es)",
        meshCount - failureCount, meshCount);

    if (dryRun)
    {
        IDTX_LOG(IDTX_INFO, "  [dry-run] Skipping export");
        return failureCount == 0;
    }

    // Output path: <outputDir>/<stem>_tangents.<ext>
    const bool exported = ExportStage(stage, inputPath, outputDir, "_tangents");

    return failureCount == 0 && exported;
}

} // anonymous namespace

// ===========================================================================
// Public API
// ===========================================================================

CLI::App* RegisterTangentsCommand(CLI::App& app, TangentsOptions& opts)
{
    auto* sub = app.add_subcommand("tangents",
        "Calculate tangent space vectors for normal mapping\n"
        "  mikktspace  - industry-standard MikkTSpace algorithm (recommended)\n"
        "  gramschmidt - Gram-Schmidt orthogonalisation");

    sub->add_option("--algorithm", opts.algorithm,
            "Tangent space calculation algorithm to use")
        ->required()
        ->type_name("mikktspace|gramschmidt")
        ->check(CLI::IsMember({"mikktspace", "gramschmidt"}));

    sub->add_flag("--include-referenced", opts.includeReferenced,
        "Also process prims that are brought into the stage via "
        "reference/payload arcs (default: only prims authored on the root "
        "layer are modified)");

    return sub;
}

int RunTangentsCommand(
    const std::vector<std::string>& inputFiles,
    const std::string&              outputDir,
    const TangentsOptions&          opts,
    bool                            dryRun)
{
    IDTX_LOG(IDTX_INFO, "tangents  algorithm={}", opts.algorithm);

    int failures = 0;
    for (const auto& inputPath : inputFiles)
    {
        if (!ProcessStage(inputPath, outputDir, opts, dryRun))
            ++failures;
    }

    if (failures > 0)
        IDTX_LOG(IDTX_ERROR, "{} file(s) failed to compute tangents", failures);

    return failures == 0 ? 0 : 1;
}

} // namespace idtx::commands
