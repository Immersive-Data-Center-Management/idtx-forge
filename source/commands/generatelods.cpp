/**
 * @file generatelods.cpp
 * @brief Implementation of the "reduce" CLI subcommand.
 *
 * QEM (Garland & Heckbert 1997) half-edge collapse simplifier using only
 * USD core / Gf types.  Two CLI algorithms: "qem" (full quadric cost,
 * optimal target position) and "edgecollapse" (shortest-edge cost,
 * midpoint).  Both share the same loop and the same primvar / subset
 * remap so face-varying data (UVs, corner normals, colors) stays valid.
 *
 * Input meshes must be triangulated; non-triangle meshes are skipped.
 **/

#include "commands/generatelods.h"
#include <algorithm>
#include <array>
#include <cmath>
#include <cstdint>
#include <queue>
#include <unordered_map>
#include <unordered_set>
#include <vector>
#include <pxr/base/gf/vec2f.h>
#include <pxr/base/gf/vec3f.h>
#include <pxr/base/vt/array.h>
#include <pxr/usd/sdf/layer.h>
#include <pxr/usd/sdf/path.h>
#include <pxr/usd/sdf/payload.h>
#include <pxr/usd/sdf/primSpec.h>
#include <pxr/usd/usd/editContext.h>
#include <pxr/usd/usd/editTarget.h>
#include <pxr/usd/usd/stage.h>
#include <pxr/usd/usd/payloads.h>
#include <pxr/usd/usd/primRange.h>
#include <pxr/usd/usd/variantSets.h>
#include <pxr/usd/usdGeom/mesh.h>
#include <pxr/usd/usdGeom/primvarsAPI.h>
#include <pxr/usd/usdGeom/subset.h>
#include <idtx/utils/Logger.h>
#include "utils/StageExport.h"
#include "utils/stageutils.h"
#include "utils/TextureResize.h"

namespace fs = std::filesystem;
PXR_NAMESPACE_USING_DIRECTIVE
IDTX_LOG_CATEGORY("Reduce")

namespace {

float getLodRatio(int lod)
{
    if (lod <= 0) return 1.0f;
    if (lod == 1) return 0.8f;
    if (lod == 2) return 0.6f;
    return 0.5f;
}

/// Hardcoded per-LOD texture downscale factor.
///
/// Textures typically account for ~90% of a usdz payload, so even a modest
/// mesh reduction on its own barely moves the needle. Halving texture side
/// lengths quarters the pixel count (and roughly the encoded size). This
/// table is deliberately a bit more aggressive than the mesh ratio table
/// above: the goal is to make LOD1+ meaningfully smaller on disk and in
/// VRAM.
///
/// Mirrors the style of getLodRatio() for now - exposed via CLI later if
/// needed.
float getTextureScale(int lod)
{
    if (lod <= 0) return 1.0f;  // LOD0 keeps full-resolution textures
    if (lod == 1) return 0.5f;  // half side -> 1/4 pixels
    if (lod == 2) return 0.25f; // quarter side -> 1/16 pixels
    return 0.125f;              // LOD3+ -> 1/64 pixels (clamped by minDim)
}

// Symmetric 4x4 quadric stored as 10 doubles (upper triangle row-major).
struct Quadric {
    double m[10] = {0,0,0,0,0,0,0,0,0,0};
    void AddPlane(double a,double b,double c,double d){
        m[0]+=a*a;m[1]+=a*b;m[2]+=a*c;m[3]+=a*d;
        m[4]+=b*b;m[5]+=b*c;m[6]+=b*d;
        m[7]+=c*c;m[8]+=c*d;m[9]+=d*d;
    }
    Quadric& operator+=(const Quadric& o){for(int i=0;i<10;++i)m[i]+=o.m[i];return *this;}
    double Eval(const GfVec3f& p) const {
        const double x=p[0],y=p[1],z=p[2];
        return m[0]*x*x+2*m[1]*x*y+2*m[2]*x*z+2*m[3]*x
              +m[4]*y*y+2*m[5]*y*z+2*m[6]*y
              +m[7]*z*z+2*m[8]*z+m[9];
    }
};

struct EdgeKey { int v0,v1; bool operator==(const EdgeKey& o)const{return v0==o.v0&&v1==o.v1;} };
struct EdgeKeyHash { std::size_t operator()(const EdgeKey& e)const noexcept{
    return std::hash<std::uint64_t>()(
        (std::uint64_t(std::uint32_t(e.v0))<<32) ^ std::uint64_t(std::uint32_t(e.v1)));
}};
inline EdgeKey MakeEdgeKey(int a,int b){return (a<b)?EdgeKey{a,b}:EdgeKey{b,a};}

struct EdgeCandidate {
    double cost; GfVec3f optPos; int v0,v1;
    std::uint32_t ver0,ver1;
    bool operator>(const EdgeCandidate& o)const{return cost>o.cost;}
};

void EvaluateCollapse(const Quadric& q,const GfVec3f& p0,const GfVec3f& p1,
                      bool useQEM,GfVec3f& outPos,double& outCost)
{
    if (useQEM) {
        const double a=q.m[0],b=q.m[1],c=q.m[2],d=q.m[4],e=q.m[5],f=q.m[7];
        const double det = a*(d*f-e*e) - b*(b*f-e*c) + c*(b*e-d*c);
        if (std::abs(det) > 1e-12) {
            const double rX=-q.m[3],rY=-q.m[6],rZ=-q.m[8];
            const double inv=1.0/det;
            const double i00=(d*f-e*e)*inv, i01=-(b*f-c*e)*inv, i02=(b*e-c*d)*inv;
            const double i11=(a*f-c*c)*inv, i12=-(a*e-b*c)*inv, i22=(a*d-b*b)*inv;
            const double x=i00*rX+i01*rY+i02*rZ;
            const double y=i01*rX+i11*rY+i12*rZ;
            const double z=i02*rX+i12*rY+i22*rZ;
            outPos = GfVec3f((float)x,(float)y,(float)z);
            outCost = q.Eval(outPos); return;
        }
        const GfVec3f mid=0.5f*(p0+p1);
        const double c0=q.Eval(p0), c1=q.Eval(p1), cm=q.Eval(mid);
        if (c0<=c1 && c0<=cm){outPos=p0;outCost=c0;}
        else if (c1<=cm){outPos=p1;outCost=c1;}
        else {outPos=mid;outCost=cm;}
    } else {
        outPos = 0.5f*(p0+p1);
        const GfVec3f dv=p1-p0;
        outCost = double(GfDot(dv,dv));
    }
}

bool WouldFlip(int v0,int v1,const GfVec3f& newPos,
               const std::vector<std::array<int,3>>& faces,
               const std::vector<bool>& faceAlive,
               const std::vector<GfVec3f>& positions,
               const std::vector<std::vector<int>>& vertexFaces)
{
    auto check=[&](int vMove,int vOther)->bool{
        for (int f : vertexFaces[vMove]) {
            if (!faceAlive[f]) continue;
            const auto& tri = faces[f];
            if (tri[0]==vOther||tri[1]==vOther||tri[2]==vOther) continue;
            GfVec3f a=positions[tri[0]], b=positions[tri[1]], c=positions[tri[2]];
            const GfVec3f nOld=GfCross(b-a,c-a);
            if (tri[0]==vMove) a=newPos; else if (tri[1]==vMove) b=newPos; else c=newPos;
            const GfVec3f nNew=GfCross(b-a,c-a);
            if (nNew.GetLengthSq()<1e-20f) return true;
            if (GfDot(nOld,nNew)<=0.0f)    return true;
        }
        return false;
    };
    return check(v0,v1)||check(v1,v0);
}

// Face-varying primvar buffer.  For each FV primvar we flatten once and
// then track, per (live) triangle corner, a slot index into that buffer.
// During a collapse we simply rewrite corner slots; no per-corner data is
// copied around in the hot loop.  At the end we compact into a fresh
// array sized 3 * liveTriangleCount.  This is the same flatten-and-remap
// approach already used by triangulate.cpp::RemapFaceVaryingData.
enum class FvType { Float, Vec2f, Vec3f, Unsupported };

struct FvPrimvar {
    UsdGeomPrimvar primvar;
    FvType type = FvType::Unsupported;
    VtFloatArray fData;
    VtVec2fArray v2Data;
    VtVec3fArray v3Data;
    std::vector<int> cornerSlot;   // size = faces.size()*3
};

void CollectFvPrimvars(const UsdGeomMesh& mesh, std::vector<FvPrimvar>& out)
{
    const UsdTimeCode time = UsdTimeCode::Default();
    UsdGeomPrimvarsAPI api(mesh.GetPrim());
    for (const UsdGeomPrimvar& pv : api.GetPrimvars()) {
        if (pv.GetInterpolation() != UsdGeomTokens->faceVarying) continue;
        FvPrimvar st; st.primvar = pv;
        const SdfValueTypeName t = pv.GetTypeName();
        if (t==SdfValueTypeNames->TexCoord2fArray || t==SdfValueTypeNames->Float2Array) {
            if (pv.ComputeFlattened(&st.v2Data, time)) st.type=FvType::Vec2f;
        } else if (t==SdfValueTypeNames->Normal3fArray || t==SdfValueTypeNames->Point3fArray ||
                   t==SdfValueTypeNames->Vector3fArray || t==SdfValueTypeNames->Float3Array  ||
                   t==SdfValueTypeNames->Color3fArray) {
            if (pv.ComputeFlattened(&st.v3Data, time)) st.type=FvType::Vec3f;
        } else if (t==SdfValueTypeNames->FloatArray) {
            if (pv.ComputeFlattened(&st.fData, time)) st.type=FvType::Float;
        } else {
            VtValue v; pv.Get(&v, time);
            if      (v.IsHolding<VtVec2fArray>()) { st.v2Data=v.UncheckedGet<VtVec2fArray>(); st.type=FvType::Vec2f; }
            else if (v.IsHolding<VtVec3fArray>()) { st.v3Data=v.UncheckedGet<VtVec3fArray>(); st.type=FvType::Vec3f; }
            else if (v.IsHolding<VtFloatArray>()) { st.fData =v.UncheckedGet<VtFloatArray>(); st.type=FvType::Float; }
        }
        if (st.type != FvType::Unsupported) out.push_back(std::move(st));
    }
}

bool GenerateMeshLod(UsdGeomMesh& mesh, const std::string& algorithm, int lod)
{
    const UsdTimeCode time = UsdTimeCode::Default();
    const float lodRatio = getLodRatio(lod);

    VtIntArray faceVertexCounts, faceVertexIndices;
    if (!mesh.GetFaceVertexCountsAttr().Get(&faceVertexCounts, time) ||
        !mesh.GetFaceVertexIndicesAttr().Get(&faceVertexIndices, time))
    {
        IDTX_LOG(IDTX_WARN, "Cannot read face topology from '{}' - skipping",
            mesh.GetPath().GetString());
        return false;
    }
    VtVec3fArray pointsArr;
    if (!mesh.GetPointsAttr().Get(&pointsArr, time)) {
        IDTX_LOG(IDTX_ERROR, "Cannot read points from '{}' - skipping",
            mesh.GetPath().GetString());
        return false;
    }

    // Require pre-triangulated input.
    for (int c : faceVertexCounts) {
        if (c != 3) {
            IDTX_LOG(IDTX_WARN,
                "Mesh '{}' contains non-triangle faces (count={}). LOD generation "
                "expects a triangulated mesh - skipping. Run 'triangulate' first.",
                mesh.GetPath().GetString(), c);
            return false;
        }
    }

    const int origVertexCount  = static_cast<int>(pointsArr.size());
    const int origFaceCount    = static_cast<int>(faceVertexCounts.size());
    if (origFaceCount < 2 || origVertexCount < 4) {
        IDTX_LOG(IDTX_INFO, "Mesh '{}' too small to simplify - skipping",
            mesh.GetPath().GetString());
        return false;
    }

    // ---- Build working arrays --------------------------------------------
    std::vector<GfVec3f>           positions(pointsArr.begin(), pointsArr.end());
    std::vector<std::array<int,3>> faces(origFaceCount);
    std::vector<bool>              faceAlive(origFaceCount, true);
    std::vector<bool>              vertAlive(origVertexCount, true);
    std::vector<std::uint32_t>     vertVersion(origVertexCount, 0);
    std::vector<std::vector<int>>  vertexFaces(origVertexCount);
    std::vector<Quadric>           quadrics(origVertexCount);

    for (int f = 0; f < origFaceCount; ++f) {
        const int i0 = faceVertexIndices[3*f + 0];
        const int i1 = faceVertexIndices[3*f + 1];
        const int i2 = faceVertexIndices[3*f + 2];
        if (i0 < 0 || i1 < 0 || i2 < 0 ||
            i0 >= origVertexCount || i1 >= origVertexCount || i2 >= origVertexCount ||
            i0 == i1 || i1 == i2 || i0 == i2) {
            faceAlive[f] = false;
            continue;
        }
        faces[f] = {i0, i1, i2};
        vertexFaces[i0].push_back(f);
        vertexFaces[i1].push_back(f);
        vertexFaces[i2].push_back(f);

        // Plane equation (a,b,c,d) with unit normal accumulated into the
        // three corner-vertex quadrics.
        const GfVec3f& p0 = positions[i0];
        const GfVec3f& p1 = positions[i1];
        const GfVec3f& p2 = positions[i2];
        GfVec3f n = GfCross(p1 - p0, p2 - p0);
        const float nl = n.GetLength();
        if (nl < 1e-20f) continue;
        n /= nl;
        const double a = n[0], b = n[1], c = n[2];
        const double d = -(a*p0[0] + b*p0[1] + c*p0[2]);
        quadrics[i0].AddPlane(a,b,c,d);
        quadrics[i1].AddPlane(a,b,c,d);
        quadrics[i2].AddPlane(a,b,c,d);
    }

    // Face-varying primvars (UVs, corner normals, colors, ...).
    std::vector<FvPrimvar> fvPrimvars;
    CollectFvPrimvars(mesh, fvPrimvars);
    for (auto& pv : fvPrimvars) {
        pv.cornerSlot.resize(static_cast<std::size_t>(origFaceCount) * 3);
        for (int f = 0; f < origFaceCount; ++f) {
            pv.cornerSlot[3*f + 0] = 3*f + 0;
            pv.cornerSlot[3*f + 1] = 3*f + 1;
            pv.cornerSlot[3*f + 2] = 3*f + 2;
        }
    }

    const bool useQEM = (algorithm != "edgecollapse");

    // ---- Seed the edge priority queue ------------------------------------
    std::priority_queue<EdgeCandidate, std::vector<EdgeCandidate>,
                        std::greater<EdgeCandidate>> queue;
    std::unordered_map<EdgeKey, char, EdgeKeyHash> edgeSeen;
    edgeSeen.reserve(origFaceCount * 3);

    auto pushEdge = [&](int a, int b) {
        if (a == b) return;
        const EdgeKey k = MakeEdgeKey(a, b);
        Quadric q = quadrics[k.v0]; q += quadrics[k.v1];
        EdgeCandidate cand;
        cand.v0 = k.v0; cand.v1 = k.v1;
        cand.ver0 = vertVersion[k.v0];
        cand.ver1 = vertVersion[k.v1];
        EvaluateCollapse(q, positions[k.v0], positions[k.v1],
                         useQEM, cand.optPos, cand.cost);
        queue.push(cand);
    };

    for (int f = 0; f < origFaceCount; ++f) {
        if (!faceAlive[f]) continue;
        const auto& tri = faces[f];
        for (int k = 0; k < 3; ++k) {
            const int a = tri[k], b = tri[(k+1) % 3];
            const EdgeKey ek = MakeEdgeKey(a, b);
            if (edgeSeen.emplace(ek, char(1)).second) pushEdge(a, b);
        }
    }

    // ---- Collapse loop ---------------------------------------------------
    const int targetTri = std::max(4, static_cast<int>(std::round(origFaceCount * lodRatio)));
    int aliveTri = origFaceCount;

    while (aliveTri > targetTri && !queue.empty()) {
        EdgeCandidate cand = queue.top(); queue.pop();
        const int v0 = cand.v0, v1 = cand.v1;
        if (!vertAlive[v0] || !vertAlive[v1]) continue;
        if (cand.ver0 != vertVersion[v0] || cand.ver1 != vertVersion[v1]) continue;
        if (WouldFlip(v0, v1, cand.optPos, faces, faceAlive, positions, vertexFaces)) continue;

        // Move v0, kill v1.
        positions[v0] = cand.optPos;
        quadrics[v0] += quadrics[v1];

        // Rewrite faces incident to v1: faces containing both v0 and v1 die,
        // the rest have v1 replaced by v0 and migrate to vertexFaces[v0].
        for (int f : vertexFaces[v1]) {
            if (!faceAlive[f]) continue;
            auto& tri = faces[f];
            const bool hasV0 = (tri[0]==v0||tri[1]==v0||tri[2]==v0);
            if (hasV0) {
                faceAlive[f] = false;
                --aliveTri;
                continue;
            }
            // Replace v1 with v0 in this triangle and inherit its FV corner.
            for (int k = 0; k < 3; ++k) {
                if (tri[k] == v1) {
                    tri[k] = v0;
                    // Corner slot stays the same - the surviving vertex
                    // simply inherits v1's corner data for this face, which
                    // is the natural choice for half-edge collapse: the
                    // texture coordinate at v1 in face f is the one that
                    // best matches the geometry on that side of the seam.
                    break;
                }
            }
            vertexFaces[v0].push_back(f);
        }

        // For faces that already referenced v0 (not v1), if the collapse
        // moved v0 the cornerSlot stays valid (still the corner slot of v0
        // in that triangle - UVs at v0 are kept).
        vertexFaces[v1].clear();
        vertAlive[v1] = false;
        ++vertVersion[v0];

        // Re-queue every edge incident to v0.
        std::unordered_map<EdgeKey, char, EdgeKeyHash> localSeen;
        for (int f : vertexFaces[v0]) {
            if (!faceAlive[f]) continue;
            const auto& tri = faces[f];
            for (int k = 0; k < 3; ++k) {
                const int a = tri[k], b = tri[(k+1) % 3];
                if (a != v0 && b != v0) continue;
                const EdgeKey ek = MakeEdgeKey(a, b);
                if (localSeen.emplace(ek, char(1)).second) pushEdge(a, b);
            }
        }
    }

    // ---- Compact & write back --------------------------------------------
    // Build oldVertex -> newVertex map.
    std::vector<int> vMap(origVertexCount, -1);
    VtVec3fArray newPoints;
    newPoints.reserve(origVertexCount);
    for (int v = 0; v < origVertexCount; ++v) {
        if (!vertAlive[v]) continue;
        vMap[v] = static_cast<int>(newPoints.size());
        newPoints.push_back(positions[v]);
    }

    // Build new topology and oldFace -> newFace map.
    VtIntArray newCounts, newIndices;
    newCounts.reserve(aliveTri);
    newIndices.reserve(aliveTri * 3);
    std::vector<int> fMap(origFaceCount, -1);

    // Compacted face-varying output buffers (one per FV primvar).
    std::vector<VtFloatArray> outF (fvPrimvars.size());
    std::vector<VtVec2fArray> outV2(fvPrimvars.size());
    std::vector<VtVec3fArray> outV3(fvPrimvars.size());
    for (std::size_t i = 0; i < fvPrimvars.size(); ++i) {
        if      (fvPrimvars[i].type == FvType::Float) outF [i].reserve(aliveTri * 3);
        else if (fvPrimvars[i].type == FvType::Vec2f) outV2[i].reserve(aliveTri * 3);
        else if (fvPrimvars[i].type == FvType::Vec3f) outV3[i].reserve(aliveTri * 3);
    }

    for (int f = 0; f < origFaceCount; ++f) {
        if (!faceAlive[f]) continue;
        const auto& tri = faces[f];
        const int n0 = vMap[tri[0]], n1 = vMap[tri[1]], n2 = vMap[tri[2]];
        if (n0 < 0 || n1 < 0 || n2 < 0 || n0 == n1 || n1 == n2 || n0 == n2)
            continue;
        fMap[f] = static_cast<int>(newCounts.size());
        newIndices.push_back(n0);
        newIndices.push_back(n1);
        newIndices.push_back(n2);
        newCounts.push_back(3);

        // Append FV corner data using the slot lookup we maintained.
        for (std::size_t i = 0; i < fvPrimvars.size(); ++i) {
            const auto& pv = fvPrimvars[i];
            const int s0 = pv.cornerSlot[3*f + 0];
            const int s1 = pv.cornerSlot[3*f + 1];
            const int s2 = pv.cornerSlot[3*f + 2];
            if (pv.type == FvType::Vec2f) {
                if (s0 < (int)pv.v2Data.size() && s1 < (int)pv.v2Data.size() &&
                    s2 < (int)pv.v2Data.size()) {
                    outV2[i].push_back(pv.v2Data[s0]);
                    outV2[i].push_back(pv.v2Data[s1]);
                    outV2[i].push_back(pv.v2Data[s2]);
                }
            } else if (pv.type == FvType::Vec3f) {
                if (s0 < (int)pv.v3Data.size() && s1 < (int)pv.v3Data.size() &&
                    s2 < (int)pv.v3Data.size()) {
                    outV3[i].push_back(pv.v3Data[s0]);
                    outV3[i].push_back(pv.v3Data[s1]);
                    outV3[i].push_back(pv.v3Data[s2]);
                }
            } else if (pv.type == FvType::Float) {
                if (s0 < (int)pv.fData.size() && s1 < (int)pv.fData.size() &&
                    s2 < (int)pv.fData.size()) {
                    outF[i].push_back(pv.fData[s0]);
                    outF[i].push_back(pv.fData[s1]);
                    outF[i].push_back(pv.fData[s2]);
                }
            }
        }
    }

    // ---- Author back to the prim ----------------------------------------
    mesh.GetPointsAttr()           .Set(newPoints,  time);
    mesh.GetFaceVertexCountsAttr() .Set(newCounts,  time);
    mesh.GetFaceVertexIndicesAttr().Set(newIndices, time);

    // Recompute extent on the simplified point set.
    if (!newPoints.empty()) {
        GfVec3f mn = newPoints[0], mx = newPoints[0];
        for (const GfVec3f& p : newPoints) {
            for (int i = 0; i < 3; ++i) {
                if (p[i] < mn[i]) mn[i] = p[i];
                if (p[i] > mx[i]) mx[i] = p[i];
            }
        }
        VtVec3fArray extent(2);
        extent[0] = mn; extent[1] = mx;
        mesh.GetExtentAttr().Set(extent, time);
    }

    // Vertex normals: if authored with 'vertex' or 'varying' interpolation,
    // remap; if face-varying it is already handled via the primvar loop
    // above (UsdGeomPrimvarsAPI exposes 'primvars:normals').  For the
    // legacy schema attribute 'normals' on the mesh we recompute from the
    // simplified geometry to ensure shading stays correct.
    {
        VtVec3fArray normals;
        if (mesh.GetNormalsAttr().Get(&normals, time) && !normals.empty()) {
            // Recompute per-vertex normals via area-weighted face normals.
            VtVec3fArray newNormals(newPoints.size(), GfVec3f(0));
            for (std::size_t ti = 0; ti < newCounts.size(); ++ti) {
                const int a = newIndices[3*ti + 0];
                const int b = newIndices[3*ti + 1];
                const int c = newIndices[3*ti + 2];
                const GfVec3f n = GfCross(newPoints[b] - newPoints[a],
                                          newPoints[c] - newPoints[a]);
                newNormals[a] += n;
                newNormals[b] += n;
                newNormals[c] += n;
            }
            for (GfVec3f& n : newNormals) {
                const float l = n.GetLength();
                if (l > 1e-20f) n /= l;
            }
            mesh.GetNormalsAttr().Set(newNormals, time);
            mesh.SetNormalsInterpolation(UsdGeomTokens->vertex);
        }
    }

    // Vertex / varying / uniform primvars + write out the compacted
    // face-varying buffers we built above.
    UsdGeomPrimvarsAPI api(mesh.GetPrim());
    for (UsdGeomPrimvar& pv : api.GetPrimvars()) {
        const TfToken interp = pv.GetInterpolation();
        if (interp == UsdGeomTokens->constant) continue;

        if (interp == UsdGeomTokens->uniform) {
            // One value per face.  Remap via fMap.
            VtValue v; pv.Get(&v, time);
            if (v.IsHolding<VtFloatArray>()) {
                const auto& src = v.UncheckedGet<VtFloatArray>();
                VtFloatArray dst; dst.reserve(newCounts.size());
                for (int f = 0; f < origFaceCount; ++f)
                    if (fMap[f] >= 0 && f < (int)src.size())
                        dst.push_back(src[f]);
                pv.Set(dst, time);
            } else if (v.IsHolding<VtVec3fArray>()) {
                const auto& src = v.UncheckedGet<VtVec3fArray>();
                VtVec3fArray dst; dst.reserve(newCounts.size());
                for (int f = 0; f < origFaceCount; ++f)
                    if (fMap[f] >= 0 && f < (int)src.size())
                        dst.push_back(src[f]);
                pv.Set(dst, time);
            } else if (v.IsHolding<VtVec2fArray>()) {
                const auto& src = v.UncheckedGet<VtVec2fArray>();
                VtVec2fArray dst; dst.reserve(newCounts.size());
                for (int f = 0; f < origFaceCount; ++f)
                    if (fMap[f] >= 0 && f < (int)src.size())
                        dst.push_back(src[f]);
                pv.Set(dst, time);
            }
            UsdAttribute idx = pv.GetIndicesAttr();
            if (idx && idx.HasAuthoredValue()) idx.Clear();
            continue;
        }

        if (interp == UsdGeomTokens->vertex || interp == UsdGeomTokens->varying) {
            // One value (or `elementSize` consecutive values) per point.
            // Multi-element-per-vertex primvars include UsdSkel's
            // 'primvars:skel:jointIndices' and 'primvars:skel:jointWeights',
            // where elementSize equals the joints-per-vertex influence count.
            const int elemSize = std::max(1, pv.GetElementSize());
            VtValue v; pv.Get(&v, time);

            auto remapTyped = [&](auto& src, auto& dst) {
                dst.reserve(static_cast<std::size_t>(newPoints.size()) * elemSize);
                const std::size_t srcSize = src.size();
                for (int i = 0; i < origVertexCount; ++i) {
                    if (vMap[i] < 0) continue;
                    const std::size_t base = static_cast<std::size_t>(i) * elemSize;
                    if (base + static_cast<std::size_t>(elemSize) > srcSize) continue;
                    for (int k = 0; k < elemSize; ++k)
                        dst.push_back(src[base + k]);
                }
            };

            if (v.IsHolding<VtFloatArray>()) {
                const auto& src = v.UncheckedGet<VtFloatArray>();
                VtFloatArray dst; remapTyped(src, dst);
                pv.Set(dst, time);
            } else if (v.IsHolding<VtVec3fArray>()) {
                const auto& src = v.UncheckedGet<VtVec3fArray>();
                VtVec3fArray dst; remapTyped(src, dst);
                pv.Set(dst, time);
            } else if (v.IsHolding<VtVec2fArray>()) {
                const auto& src = v.UncheckedGet<VtVec2fArray>();
                VtVec2fArray dst; remapTyped(src, dst);
                pv.Set(dst, time);
            } else if (v.IsHolding<VtIntArray>()) {
                // Integer-typed per-vertex primvars (e.g.
                // 'primvars:skel:jointIndices').
                const auto& src = v.UncheckedGet<VtIntArray>();
                VtIntArray dst; remapTyped(src, dst);
                pv.Set(dst, time);
            } else {
                IDTX_LOG(IDTX_WARN,
                    "  Skipping vertex/varying primvar '{}' on '{}': unsupported value type",
                    pv.GetPrimvarName().GetString(),
                    mesh.GetPath().GetString());
            }
            UsdAttribute idx = pv.GetIndicesAttr();
            if (idx && idx.HasAuthoredValue()) idx.Clear();
            continue;
        }

        if (interp == UsdGeomTokens->faceVarying) {
            // Look up our compacted output buffer.
            std::size_t bufIdx = SIZE_MAX;
            for (std::size_t i = 0; i < fvPrimvars.size(); ++i) {
                if (fvPrimvars[i].primvar.GetAttr().GetPath() == pv.GetAttr().GetPath()) {
                    bufIdx = i;
                    break;
                }
            }
            if (bufIdx == SIZE_MAX) continue;
            const FvPrimvar& src = fvPrimvars[bufIdx];
            if      (src.type == FvType::Vec2f) pv.Set(outV2[bufIdx], time);
            else if (src.type == FvType::Vec3f) pv.Set(outV3[bufIdx], time);
            else if (src.type == FvType::Float) pv.Set(outF [bufIdx], time);
            UsdAttribute idx = pv.GetIndicesAttr();
            if (idx && idx.HasAuthoredValue()) idx.Clear();
        }
    }

    // GeomSubsets of element type 'face': remap face indices, drop dead.
    for (const UsdGeomSubset& subset : UsdGeomSubset::GetAllGeomSubsets(mesh)) {
        TfToken et;
        if (!subset.GetElementTypeAttr().Get(&et)) continue;
        if (et != UsdGeomTokens->face) continue;
        VtIntArray idx;
        if (!subset.GetIndicesAttr().Get(&idx)) continue;
        VtIntArray newIdx;
        newIdx.reserve(idx.size());
        for (int fi : idx) {
            if (fi >= 0 && fi < origFaceCount && fMap[fi] >= 0)
                newIdx.push_back(fMap[fi]);
        }
        subset.GetIndicesAttr().Set(newIdx, time);
    }

    IDTX_LOG(IDTX_INFO,
        "  '{}': simplified {} -> {} triangles (target {}, ratio {:.2f})",
        mesh.GetPath().GetString(), origFaceCount,
        static_cast<int>(newCounts.size()), targetTri, lodRatio);

    return true;
}

// ---------------------------------------------------------------------------
// Compute the output filename (basename only, no directory) that ExportStage
// produces for a given input + suffix. Mirrors the naming logic in
// StageExport.cpp so the "_reduces.usda" stage can reference each LOD package
// via a relative payload asset path.
// ---------------------------------------------------------------------------
std::string ComputeLodFileName(const std::string& inputPath,
                               const std::string& suffix)
{
    const fs::path inPath(inputPath);
    const std::string stem = inPath.stem().string() + suffix;
    std::string ext = inPath.extension().string();
    std::string lowerExt = ext;
    std::transform(lowerExt.begin(), lowerExt.end(), lowerExt.begin(), ::tolower);

    // .usdz inputs are re-packaged as .usdz (see ExportStage). All other
    // inputs keep their original extension.
    if (lowerExt == ".usdz")
        return stem + ".usdz";
    return stem + ext;
}

// ---------------------------------------------------------------------------
// Copy the root-layer metadata of the source stage onto the freshly created
// reduces stage.
//
// We copy the pseudo-root spec's info fields (which is where stage/root-layer
// metadata such as `metersPerUnit`, `upAxis`, `defaultPrim`, `kilogramsPerUnit`,
// `timeCodesPerSecond`, custom `customLayerData`, ... live) 1:1. `defaultPrim`
// is deliberately skipped because the reduces stage defines its own default
// prim ("/LOD_<stem>") which differs from the LOD0 file's default prim.
// ---------------------------------------------------------------------------
void CopyRootLayerMetadata(const SdfLayerHandle& srcLayer,
                           const SdfLayerHandle& dstLayer)
{
    if (!srcLayer || !dstLayer) return;

    const SdfPrimSpecHandle srcRoot = srcLayer->GetPseudoRoot();
    const SdfPrimSpecHandle dstRoot = dstLayer->GetPseudoRoot();
    if (!srcRoot || !dstRoot) return;

    for (const TfToken& field : srcRoot->ListInfoKeys()) {
        // Keep the reduces stage's own default prim and prim children/order.
        if (field == SdfFieldKeys->DefaultPrim ||
            field == SdfChildrenKeys->PrimChildren ||
            field == SdfFieldKeys->PrimOrder)
            continue;
        dstRoot->SetInfo(field, srcRoot->GetInfo(field));
    }
}

// ---------------------------------------------------------------------------
// Write the "<stem>_reduces.usda" stage.
//
// The stage carries a "/World" default prim and a single child prim that
// exposes every generated LOD as a variant on a "LOD" variant set. Each
// variant authors a payload arc onto the matching LOD asset file so that
// selecting a variant loads the corresponding geometry package.
//
// The root-layer metadata (MPU, upAxis, ...) is copied 1:1 from the source
// stage's root layer so the LOD variant stage matches the LOD0 file.
// ---------------------------------------------------------------------------
bool WriteReducesStage(
    const std::string&              inputPath,
    const std::string&              outputDir,
    const std::vector<std::string>& lodFileNames,
    const UsdStageRefPtr&           sourceStage)
{
    if (lodFileNames.empty()) return false;

    const fs::path inPath(inputPath);
    const std::string stem = inPath.stem().string();
    const fs::path outPath = fs::path(outputDir) / (stem + "_reduces.usda");

    UsdStageRefPtr stage = UsdStage::CreateNew(outPath.string());
    if (!stage) {
        IDTX_LOG(IDTX_ERROR, "  Failed to create reduces stage '{}'",
            outPath.string());
        return false;
    }

    // Author the same root-layer metadata (metersPerUnit, upAxis, ...) as the
    // LOD0 file by copying the source root layer's metadata 1:1.
    if (sourceStage)
        CopyRootLayerMetadata(sourceStage->GetRootLayer(), stage->GetRootLayer());

    // Child prim that holds the LOD variant set.
    const SdfPath modelPath("/LOD_" + stem);
    UsdPrim model = stage->DefinePrim(modelPath, TfToken("Xform"));
    if (!model) {
        IDTX_LOG(IDTX_ERROR, "  Failed to define model prim in reduces stage with path {}", modelPath.GetString());
        return false;
    }
    
    stage->SetDefaultPrim(model);

    // Author one variant per generated LOD, each adding a payload arc onto
    // the corresponding LOD asset file (relative to this stage).
    UsdVariantSet lodVset = model.GetVariantSets().AddVariantSet("LOD");
    for (std::size_t i = 0; i < lodFileNames.size(); ++i) {
        const std::string variantName = "LOD" + std::to_string(i);
        lodVset.AddVariant(variantName);
        lodVset.SetVariantSelection(variantName);
        {
            UsdEditContext ctx(lodVset.GetVariantEditContext());
            model.GetPayloads().AddPayload(
                SdfPayload("./" + lodFileNames[i]));
        }
    }

    // Default the selection to the highest-fidelity LOD (LOD0).
    lodVset.SetVariantSelection("LOD0");

    if (!stage->GetRootLayer()->Save()) {
        IDTX_LOG(IDTX_ERROR, "  Failed to save reduces stage '{}'",
            outPath.string());
        return false;
    }

    IDTX_LOG(IDTX_INFO, "  Exported LOD variant stage to '{}'", outPath.string());
    return true;
}

// ---------------------------------------------------------------------------
// Process a single USD stage file
// ---------------------------------------------------------------------------

bool ProcessStage(
    const std::string&        inputPath,
    const std::string&        outputDir,
    const idtx::commands::LodOptions& opts,
    bool                      dryRun)
{
    IDTX_LOG(IDTX_INFO, "Processing '{}'", inputPath);

    // when opening the stage, ensure we never load payload arcs, as we do not want to follow them anyway.
    UsdStageRefPtr stage = UsdStage::Open(inputPath, UsdStage::LoadNone);
    if (!stage) {
        IDTX_LOG(IDTX_ERROR, "  Failed to open USD stage '{}'", inputPath);
        return false;
    }

    // Track the basename of every exported LOD package so we can wire them
    // up as payload variants in the "_reduces.usda" stage afterwards.
    std::vector<std::string> lodFileNames;

    // LOD0 is the unmodified original - keep textures untouched.
    if (!ExportStage(stage, inputPath, outputDir, "_LOD0")) return false;
    if (!dryRun)
        lodFileNames.push_back(ComputeLodFileName(inputPath, "_LOD0"));

    for (int currentLod = 1; currentLod < opts.lodNum; ++currentLod) {
        // Build the texture resize options for this LOD. If the user has
        // explicitly opted out via --no-texture-reduction we leave the
        // default (scale == 1.0), which RepackageUsdz treats as a no-op.
        idtx::utils::TextureResizeOptions texOpts;
        if (!opts.noTextureReduction)
            texOpts.scale = getTextureScale(currentLod);

        // Re-open a fresh copy of the stage for each LOD so that the
        // progressive decimation always starts from the original mesh.
        UsdStageRefPtr lodStage = UsdStage::Open(inputPath);
        if (!lodStage) {
            IDTX_LOG(IDTX_ERROR, "  Failed to re-open stage for LOD {}", currentLod);
            continue;
        }

        // Instances (native or pseudo) reference a prototype/"template" prim
        // (usually authored as `over`) for their mesh data instead of authoring
        // their own. The shared traversal helper skips the instance prims
        // themselves and processes each unique prototype mesh exactly once
        // afterwards.
        idtx::utils::TraversalOptions opts2;  // defaults = SkipAndCollectPrototypes
        // By default only prims authored on the root layer are modified; the
        // caller can opt into decimating referenced/payloaded prims explicitly.
        if (opts.includeReferenced)
            opts2.referencedPolicy =
                idtx::utils::ReferencedPrimPolicy::IncludeReferencedAndPayloaded;
        const idtx::utils::TraversalResult res =
            idtx::utils::TraverseMeshLike<UsdGeomMesh>(
                lodStage, opts2,
                [&](const UsdPrim& prim, const idtx::utils::PrimVisitContext& /*ctx*/) {
                    UsdGeomMesh mesh(prim);
                    IDTX_LOG(IDTX_DEBUG, "  Create LOD {} for mesh prim '{}'",
                        currentLod, prim.GetPath().GetString());
                    return GenerateMeshLod(mesh, opts.algorithm, currentLod);
                });

        const int failureCount = res.failures;
        const int processed    = res.processed;

        if (dryRun) {
            IDTX_LOG(IDTX_INFO, "  [dry-run] Skipping LOD {} export ({} meshes, {} failures)",
                currentLod, processed, failureCount);
            continue;
        }

        const std::string suffix = "_LOD" + std::to_string(currentLod);
        if (!ExportStage(lodStage, inputPath, outputDir, suffix, texOpts)) {
            IDTX_LOG(IDTX_ERROR, "  Failed to export LOD {} for '{}'",
                currentLod, inputPath);
            return false;
        }
        lodFileNames.push_back(ComputeLodFileName(inputPath, suffix));
    }

    // Emit the companion "_reduces.usda" stage that ties all generated LOD
    // packages together as payload-backed variants of a single "LOD" variant
    // set. Skipped in dry-run mode (nothing was written to disk).
    if (!dryRun && !WriteReducesStage(inputPath, outputDir, lodFileNames, stage)) {
        IDTX_LOG(IDTX_ERROR, "  Failed to write reduces stage for '{}'", inputPath);
        return false;
    }

    return true;
}
} // anonymous namespace

namespace idtx::commands
{
// ===========================================================================
// Public API
// ===========================================================================

CLI::App* RegisterGenerateLodsCommand(CLI::App& app, LodOptions& opts)
{
    auto* subCommand = app.add_subcommand("reduce",
        "Reduce vertex/face count and generate LOD files\n"
        "  qem          - Quadric Error Metrics reduction\n"
        "  edgecollapse - Half-edge collapse reduction");

    subCommand->add_option("--algorithm", opts.algorithm, "Reduction algorithm to use")
        ->required()
        ->type_name("qem|edgecollapse")
        ->check(CLI::IsMember({"qem", "edgecollapse"}));

    subCommand->add_option("--lods", opts.lodNum,
        "Number of LOD levels to generate (default: 3 -> generates LOD0, LOD1, LOD2)")
        ->type_name("N")
        ->check(CLI::PositiveNumber);

    subCommand->add_flag("--no-texture-reduction", opts.noTextureReduction,
        "Disable per-LOD texture downsampling (mesh-only LODs). By default "
        "textures inside .usdz inputs are halved in size for each successive "
        "LOD level; pass this flag to keep textures at their original "
        "resolution in every LOD package");

    subCommand->add_flag("--include-referenced", opts.includeReferenced,
        "Also process prims that are brought into the stage via "
        "reference/payload arcs (default: only prims authored on the root "
        "layer are modified)");

    return subCommand;
}

int RunGenerateLodsCommand(
    const std::vector<std::string>& inputFiles,
    const std::string&              outputDir,
    const LodOptions&               opts,
    bool                            dryRun)
{
    IDTX_LOG(IDTX_INFO, "LOD generation algorithm={}, number of LODs={}",
        opts.algorithm, opts.lodNum);
    if (opts.lodNum < 1) {
        IDTX_LOG(IDTX_ERROR, "LOD number must be greater than 0");
        return -1;
    }

    int failures = 0;
    for (const auto& inputPath : inputFiles) {
        if (!ProcessStage(inputPath, outputDir, opts, dryRun))
            ++failures;
    }

    if (failures > 0)
        IDTX_LOG(IDTX_ERROR, "{} file(s) failed to generate LOD's for", failures);

    return failures == 0 ? 0 : 1;
}

} // namespace idtx::commands
