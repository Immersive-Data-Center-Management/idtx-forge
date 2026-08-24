/**
 * @file mpu.cpp
 * @brief Implementation of the "mpu" subcommand.
 *
 * See mpu.h for the full behavioural contract. This file opens each input USD
 * stage, adjusts its metersPerUnit metadata to the requested target and applies
 * a compensating uniform scale on the (possibly injected) root prim so the
 * rendered physical size of the model is preserved.
 **/

#include "commands/mpu.h"

#include <algorithm>
#include <string>
#include <vector>

#include <pxr/pxr.h>
#include <pxr/base/gf/matrix4d.h>
#include <pxr/base/gf/matrix4f.h>
#include <pxr/base/gf/vec3d.h>
#include <pxr/base/gf/vec3f.h>
#include <pxr/base/tf/token.h>
#include <pxr/usd/sdf/layer.h>
#include <pxr/usd/sdf/namespaceEdit.h>
#include <pxr/usd/sdf/path.h>
#include <pxr/usd/usd/prim.h>
#include <pxr/usd/usd/stage.h>
#include <pxr/usd/usdGeom/metrics.h>
#include <pxr/usd/usdGeom/xform.h>
#include <pxr/usd/usdGeom/xformOp.h>
#include <pxr/usd/usdGeom/xformable.h>
#include <pxr/usd/usdGeom/xformCommonAPI.h>

#include <idtx/utils/Logger.h>

#include "utils/StageExport.h"

PXR_NAMESPACE_USING_DIRECTIVE

IDTX_LOG_CATEGORY("Mpu")

namespace idtx::commands {

namespace {

// ---------------------------------------------------------------------------
// ResolveTargetMpu - translate a --target string into a meters-per-unit value.
//
// Accepts common real-world unit names (case-insensitive) or a raw numeric
// meters-per-unit value. Returns false if the string could not be interpreted
// or resolved to a strictly positive value.
// ---------------------------------------------------------------------------
bool ResolveTargetMpu(const std::string& target, double* outMpu)
{
    std::string key = target;
    std::transform(key.begin(), key.end(), key.begin(),
                   [](unsigned char c) { return static_cast<char>(std::tolower(c)); });

    // Common real-world unit names mapped to meters-per-unit.
    if (key == "meter" || key == "meters" || key == "metre" || key == "m")
    {
        *outMpu = 1.0;
        return true;
    }
    if (key == "cm" || key == "centimeter" || key == "centimeters" || key == "centimetre")
    {
        *outMpu = 0.01;
        return true;
    }
    if (key == "mm" || key == "millimeter" || key == "millimeters" || key == "millimetre")
    {
        *outMpu = 0.001;
        return true;
    }
    if (key == "feet" || key == "foot" || key == "ft")
    {
        *outMpu = 0.3048;
        return true;
    }
    if (key == "inch" || key == "inches" || key == "in")
    {
        *outMpu = 0.0254;
        return true;
    }

    // Fall back to interpreting the target as a raw meters-per-unit value.
    try
    {
        size_t consumed = 0;
        const double value = std::stod(target, &consumed);
        if (consumed != target.size())
            return false; // trailing garbage
        if (!(value > 0.0) || !std::isfinite(value))
            return false;
        *outMpu = value;
        return true;
    }
    catch (const std::exception&)
    {
        return false;
    }
}

// ---------------------------------------------------------------------------
// ApplyScaleViaCommonAPI - fast path for prims whose xformOp stack conforms to
// the UsdGeomXformCommonAPI schema (empty, or the exact
// translate / translate:pivot / rotate / scale / !invert!translate:pivot
// ordering).
//
// The compensating factor is folded into the scale component and the existing
// translation is scaled as well so that offsets authored directly on the root
// keep their physical position after the unit change. Rotation and pivot are
// preserved untouched.
// ---------------------------------------------------------------------------
bool ApplyScaleViaCommonAPI(const UsdGeomXformCommonAPI& common, double scale)
{
    GfVec3d translation(0.0);
    GfVec3f rotation(0.0f);
    GfVec3f scaleVec(1.0f);
    GfVec3f pivot(0.0f);
    UsdGeomXformCommonAPI::RotationOrder rotOrder =
        UsdGeomXformCommonAPI::RotationOrderXYZ;

    // Read any existing common transform components so we preserve them.
    common.GetXformVectors(&translation, &rotation, &scaleVec, &pivot, &rotOrder,
                            UsdTimeCode::Default());

    const double s = scale;
    scaleVec = GfVec3f(scaleVec[0] * static_cast<float>(s),
                       scaleVec[1] * static_cast<float>(s),
                       scaleVec[2] * static_cast<float>(s));
    translation = GfVec3d(translation[0] * s, translation[1] * s, translation[2] * s);

    common.SetScale(scaleVec, UsdTimeCode::Default());
    common.SetTranslate(translation, UsdTimeCode::Default());
    return true;
}

// ---------------------------------------------------------------------------
// ApplyScaleToSingleTransformOp - fold the compensating scale into a prim whose
// op stack is a single 4x4 matrix op ("xformOp:transform").
//
// We pre-multiply the authored matrix M by a uniform scale S so the result is
// (S * M). Because USD uses row-vector matrices applied as
//   v_world = v_local * M
// pre-multiplying by S scales both the linear part and the translation row
// uniformly - exactly matching the meters-per-unit compensation, so a point p
// ends up at S * (p * M). This preserves any authored rotation / shear /
// translation encoded in the matrix.
// ---------------------------------------------------------------------------
bool ApplyScaleToSingleTransformOp(const UsdGeomXformOp& op, double scale)
{
    // Read the current matrix value (defaults to identity if unauthored). The
    // op may store the value at either double or float precision.
    GfMatrix4d matrix(1.0);
    if (!op.GetAs<GfMatrix4d>(&matrix, UsdTimeCode::Default()))
    {
        GfMatrix4f matrixf(1.0f);
        if (op.GetAs<GfMatrix4f>(&matrixf, UsdTimeCode::Default()))
            matrix = GfMatrix4d(matrixf);
    }

    GfMatrix4d scaleMatrix(1.0);
    scaleMatrix.SetScale(GfVec3d(scale, scale, scale));

    // v_world = v_local * (scaleMatrix * matrix) == scale * (v_local * matrix)
    const GfMatrix4d scaled = scaleMatrix * matrix;

    // A matrix ("xformOp:transform") op is always authored as a double-precision
    // 'matrix4d' in USD's Sdf data model: there is no single-precision matrix
    // SdfValueTypeName (GfMatrix4f is NOT a valid attribute value type). So we
    // always write back a GfMatrix4d regardless of the op's reported precision.
    return op.Set(scaled, UsdTimeCode::Default());
}

// ---------------------------------------------------------------------------
// PrependScaleOp - general fallback for any arbitrary op stack.
//
// Rather than trying to interpret an unknown combination of xformOps, we author
// a fresh uniform "xformOp:scale:mpu" and place it at the *front* of the
// xformOpOrder. Because USD composes ops left-to-right on row vectors
//   v_world = v_local * op0 * op1 * ...
// a scale at the front is applied first, in local space, and cleanly composes
// with whatever follows - giving the desired uniform metersPerUnit
// compensation without needing to understand the remaining ops.
//
// A dedicated suffix ("mpu") makes the op idempotent-friendly (re-running folds
// into the same op) and avoids clashing with any existing xformOp:scale already
// present in the stack.
// ---------------------------------------------------------------------------
bool PrependScaleOp(const UsdGeomXformable& xformable, double scale)
{
    bool resetsXformStack = false;
    std::vector<UsdGeomXformOp> orderedOps =
        xformable.GetOrderedXformOps(&resetsXformStack);

    const TfToken mpuSuffix("mpu");

    // Reuse an existing mpu scale op if we already authored one previously so
    // repeated runs stay idempotent (fold the new factor into the old value).
    for (const UsdGeomXformOp& op : orderedOps)
    {
        if (op.GetOpType() == UsdGeomXformOp::TypeScale && op.HasSuffix(mpuSuffix))
        {
            GfVec3d current(1.0);
            if (op.GetPrecision() == UsdGeomXformOp::PrecisionFloat)
            {
                GfVec3f cf(1.0f);
                op.GetAs<GfVec3f>(&cf, UsdTimeCode::Default());
                current = GfVec3d(cf);
            }
            else
            {
                op.GetAs<GfVec3d>(&current, UsdTimeCode::Default());
            }
            const GfVec3d updated(current[0] * scale, current[1] * scale,
                                  current[2] * scale);
            if (op.GetPrecision() == UsdGeomXformOp::PrecisionFloat)
                return op.Set(GfVec3f(updated), UsdTimeCode::Default());
            return op.Set(updated, UsdTimeCode::Default());
        }
    }

    // Author a new double-precision uniform scale op carrying the compensation.
    UsdGeomXformOp scaleOp =
        xformable.AddScaleOp(UsdGeomXformOp::PrecisionDouble, mpuSuffix);
    if (!scaleOp)
    {
        IDTX_LOG(IDTX_ERROR,
            "  Failed to author compensating 'xformOp:scale:mpu' on '{}'",
            xformable.GetPath().GetString());
        return false;
    }
    scaleOp.Set(GfVec3d(scale, scale, scale), UsdTimeCode::Default());

    // AddScaleOp appended the op at the end of the order. Move it to the front
    // so it is applied first (local space), composing cleanly with the rest.
    std::vector<UsdGeomXformOp> newOrder;
    newOrder.reserve(orderedOps.size() + 1);
    newOrder.push_back(scaleOp);
    for (const UsdGeomXformOp& op : orderedOps)
        newOrder.push_back(op);

    if (!xformable.SetXformOpOrder(newOrder, resetsXformStack))
    {
        IDTX_LOG(IDTX_ERROR,
            "  Failed to reorder xformOps to prepend compensating scale on '{}'",
            xformable.GetPath().GetString());
        return false;
    }
    return true;
}

// ---------------------------------------------------------------------------
// ApplyScaleToPrim - fold a compensating uniform scale onto the given prim.
//
// This does NOT rely solely on UsdGeomXformCommonAPI. UsdGeomXformCommonAPI only
// accepts a very restricted xformOp configuration (empty, or the exact
// translate/pivot/rotate/scale ordering). Many authored/exported assets instead
// use a single "xformOp:transform" matrix or an arbitrary op stack, which the
// common API rejects (operator bool() is false). To robustly handle every way
// the ops may be authored we dispatch across three strategies:
//
//   1. Common-API compatible stack  -> fold scale into scale + translation.
//   2. A single xformOp:transform   -> pre-multiply the matrix by the scale.
//   3. Any other / arbitrary stack  -> prepend a dedicated xformOp:scale:mpu.
//
// The prim must be UsdGeomXformable (guaranteed by the caller).
// ---------------------------------------------------------------------------
bool ApplyScaleToPrim(const UsdPrim& prim, double scale)
{
    UsdGeomXformable xformable(prim);
    if (!xformable)
    {
        IDTX_LOG(IDTX_ERROR,
            "  Prim '{}' is not xformable; cannot author compensating scale",
            prim.GetPath().GetString());
        return false;
    }

    // Strategy 1: fast path via the common API when the stack conforms.
    UsdGeomXformCommonAPI common(prim);
    if (common)
    {
        IDTX_LOG(IDTX_DEBUG,
            "  Prim '{}' is UsdGeomXformCommonAPI-compatible; folding scale into "
            "scale/translation components",
            prim.GetPath().GetString());
        return ApplyScaleViaCommonAPI(common, scale);
    }

    // Inspect the actual op stack for the remaining strategies.
    bool resetsXformStack = false;
    const std::vector<UsdGeomXformOp> ops =
        xformable.GetOrderedXformOps(&resetsXformStack);

    // Strategy 2: a single 4x4 matrix op.
    if (ops.size() == 1 && ops[0].GetOpType() == UsdGeomXformOp::TypeTransform)
    {
        IDTX_LOG(IDTX_DEBUG,
            "  Prim '{}' uses a single 'xformOp:transform'; pre-multiplying the "
            "matrix by the compensating scale",
            prim.GetPath().GetString());
        return ApplyScaleToSingleTransformOp(ops[0], scale);
    }

    // Strategy 3: arbitrary / non-conforming op stack. Prepend a dedicated
    // uniform scale op that composes cleanly with whatever is already authored.
    IDTX_LOG(IDTX_INFO,
        "  Prim '{}' has a non-UsdGeomXformCommonAPI op stack ({} op(s)); "
        "prepending a dedicated 'xformOp:scale:mpu' compensating scale",
        prim.GetPath().GetString(), ops.size());
    return PrependScaleOp(xformable, scale);
}

// ---------------------------------------------------------------------------
// InjectScaledRootPrim - create a new Xform root prim, reparent every existing
// top-level prim beneath it, set it as the stage default prim and fold the
// compensating scale onto it.
//
// This is used when the stage has no default prim at all (or a default prim
// that is not xformable, e.g. a Scope, Material or untyped/over prim).
// Reparenting under a fresh Xform guarantees the compensating transform can be
// authored safely.
//
// Returns true on success.
// ---------------------------------------------------------------------------
bool InjectScaledRootPrim(const UsdStageRefPtr& stage, double scale)
{
    const UsdPrim pseudoRoot = stage->GetPseudoRoot();

    // Collect existing top-level prim names so we can pick a non-colliding root
    // name and know what needs to be reparented.
    std::vector<TfToken> topLevelNames;
    for (const UsdPrim& child : pseudoRoot.GetChildren())
        topLevelNames.push_back(child.GetName());

    // Choose a unique name for the injected root.
    std::string rootName = "Root";
    auto nameTaken = [&](const std::string& n) {
        return std::any_of(topLevelNames.begin(), topLevelNames.end(),
                           [&](const TfToken& t) { return t.GetString() == n; });
    };
    if (nameTaken(rootName))
    {
        int suffix = 1;
        while (nameTaken(rootName + std::to_string(suffix)))
            ++suffix;
        rootName += std::to_string(suffix);
    }

    const SdfPath rootPath = SdfPath::AbsoluteRootPath().AppendChild(TfToken(rootName));

    UsdGeomXform rootXform = UsdGeomXform::Define(stage, rootPath);
    if (!rootXform)
    {
        IDTX_LOG(IDTX_ERROR, "  Failed to create injected root prim '{}'",
                 rootPath.GetString());
        return false;
    }
    IDTX_LOG(IDTX_INFO, "  Injected new root prim '{}'", rootPath.GetString());

    // Reparent every existing top-level prim beneath the new root using the
    // Sdf layer batch namespace-edit facility on the root (edit target) layer.
    const SdfLayerHandle layer = stage->GetEditTarget().GetLayer();
    SdfBatchNamespaceEdit batchEdit;
    for (const TfToken& name : topLevelNames)
    {
        const SdfPath oldPath = SdfPath::AbsoluteRootPath().AppendChild(name);
        const SdfPath newPath = rootPath.AppendChild(name);
        batchEdit.Add(SdfNamespaceEdit::Reparent(oldPath, rootPath,
                                                  SdfNamespaceEdit::AtEnd));
        IDTX_LOG(IDTX_DEBUG, "  Reparenting '{}' -> '{}'",
                 oldPath.GetString(), newPath.GetString());
    }

    if (!topLevelNames.empty())
    {
        if (!layer->CanApply(batchEdit))
        {
            IDTX_LOG(IDTX_ERROR,
                "  Cannot reparent existing root-level prims under '{}'",
                rootPath.GetString());
            return false;
        }
        layer->Apply(batchEdit);
    }

    // Set the injected prim as the stage's default prim.
    stage->SetDefaultPrim(rootXform.GetPrim());

    IDTX_LOG(IDTX_INFO,
        "  Applying compensating scale {} to injected root prim '{}'",
        scale, rootPath.GetString());
    return ApplyScaleToPrim(rootXform.GetPrim(), scale);
}

// ---------------------------------------------------------------------------
// EnsureScaledRootPrim - obtain a root prim carrying the compensating scale.
//
// If the stage already has a default prim that is xformable, the scale is
// applied to it (via ApplyScaleToPrim, which handles every op-stack variant).
// Otherwise (no default prim, or a default prim that is not xformable), a new
// Xform prim "/Root" (with a unique name if needed) is created, every existing
// root-level prim is reparented beneath it, and it is set as the stage's
// default prim with the compensating scale.
//
// Returns true on success.
// ---------------------------------------------------------------------------
bool EnsureScaledRootPrim(const UsdStageRefPtr& stage, double scale)
{
    // Case 1: a default prim already exists.
    if (stage->HasDefaultPrim())
    {
        UsdPrim defaultPrim = stage->GetDefaultPrim();
        if (!defaultPrim)
        {
            IDTX_LOG(IDTX_ERROR,
                "  Stage declares a default prim but it could not be resolved");
            return false;
        }

        // Report the actual type of the default prim for transparency.
        const std::string typeName = defaultPrim.GetTypeName().GetString();
        IDTX_LOG(IDTX_INFO,
            "  Default prim '{}' has type '{}'",
            defaultPrim.GetPath().GetString(),
            typeName.empty() ? "<untyped>" : typeName);

        // The default prim must be xformable for us to fold the compensating
        // scale onto it. Note: being xformable does NOT imply
        // UsdGeomXformCommonAPI compatibility - ApplyScaleToPrim handles all the
        // ways the ops may be authored (common-API, single matrix, arbitrary).
        // Prim types such as Scope, Material or untyped/over prims are not
        // xformable; in those cases we fall back to injecting a dedicated Xform
        // root that carries the scale instead.
        if (defaultPrim.IsA<UsdGeomXformable>())
        {
            IDTX_LOG(IDTX_INFO,
                "  Applying compensating scale {} to existing default prim '{}'",
                scale, defaultPrim.GetPath().GetString());
            return ApplyScaleToPrim(defaultPrim, scale);
        }

        IDTX_LOG(IDTX_INFO,
            "  Default prim '{}' (type '{}') is not xformable - injecting a "
            "compensating Xform root prim instead",
            defaultPrim.GetPath().GetString(),
            typeName.empty() ? "<untyped>" : typeName);
        return InjectScaledRootPrim(stage, scale);
    }

    // Case 2: no default prim - inject a single Xform root prim and reparent
    // all existing root-level prims beneath it.
    IDTX_LOG(IDTX_INFO,
        "  Stage has no default prim - injecting a compensating Xform root prim");
    return InjectScaledRootPrim(stage, scale);
}

// ---------------------------------------------------------------------------
// ProcessStage - adjust the MPU of a single stage.
// ---------------------------------------------------------------------------
bool ProcessStage(
    const std::string& inputPath,
    const std::string& outputDir,
    double             targetMpu,
    bool               dryRun)
{
    IDTX_LOG(IDTX_INFO, "Processing '{}'", inputPath);

    UsdStageRefPtr stage = UsdStage::Open(inputPath, UsdStage::LoadNone);
    if (!stage)
    {
        IDTX_LOG(IDTX_ERROR, "  Failed to open USD stage '{}'", inputPath);
        return false;
    }

    const double currentMpu = UsdGeomGetStageMetersPerUnit(stage);
    IDTX_LOG(IDTX_INFO, "  Current MPU : {}", currentMpu);
    IDTX_LOG(IDTX_INFO, "  Target  MPU : {}", targetMpu);

    if (!(currentMpu > 0.0) || !std::isfinite(currentMpu))
    {
        IDTX_LOG(IDTX_ERROR,
            "  Current metersPerUnit ({}) is not a valid positive value",
            currentMpu);
        return false;
    }

    // Compensating scale that keeps the physical size constant:
    //   (p * scale) * targetMpu == p * currentMpu  =>  scale = currentMpu / targetMpu
    const double scale = currentMpu / targetMpu;
    IDTX_LOG(IDTX_INFO, "  Compensating scale : {}", scale);

    // If current and target already match there is nothing to do (scale ~= 1).
    constexpr double kEps = 1e-12;
    if (std::abs(currentMpu - targetMpu) <=
        kEps * std::max(1.0, std::abs(currentMpu)))
    {
        IDTX_LOG(IDTX_INFO,
            "  Current and target MPU already match - no scaling required");
        // Still (re)author the metadata explicitly to normalize the value.
        UsdGeomSetStageMetersPerUnit(stage, targetMpu);

        if (dryRun)
        {
            IDTX_LOG(IDTX_INFO, "  [dry-run] Skipping export");
            return true;
        }
        return ExportStage(stage, inputPath, outputDir, "_mpu");
    }

    // Apply the compensating scale on the (possibly injected) root prim.
    if (!EnsureScaledRootPrim(stage, scale))
    {
        IDTX_LOG(IDTX_ERROR, "  Failed to apply compensating root scale");
        return false;
    }

    // Author the new metersPerUnit metadata.
    UsdGeomSetStageMetersPerUnit(stage, targetMpu);
    IDTX_LOG(IDTX_INFO, "  Updated metersPerUnit to {}", targetMpu);

    if (dryRun)
    {
        IDTX_LOG(IDTX_INFO, "  [dry-run] Skipping export");
        return true;
    }

    if (!ExportStage(stage, inputPath, outputDir, "_mpu"))
    {
        IDTX_LOG(IDTX_ERROR, "  Failed to export stage");
        return false;
    }

    return true;
}

} // anonymous namespace

// ===========================================================================
// Public API
// ===========================================================================

CLI::App* RegisterMpuCommand(CLI::App& app, MpuOptions& opts)
{
    auto* sub = app.add_subcommand("mpu",
        "Adjust the stage's metersPerUnit (MPU) while preserving the rendered\n"
        "physical size of the model. A compensating uniform scale is folded onto\n"
        "the stage's default/root prim (a root Xform is injected if none exists).\n"
        "\n"
        "--target accepts a unit name (meter, m, cm, mm, feet, ft, inch, in)\n"
        "or a raw meters-per-unit value (e.g. 0.01).");

    sub->add_option("--target", opts.target,
            "Target unit or raw meters-per-unit value (default: meter)")
        ->type_name("meter|cm|mm|feet|inch|<value>");

    return sub;
}

int RunMpuCommand(
    const std::vector<std::string>& inputFiles,
    const std::string&              outputDir,
    const MpuOptions&               opts,
    bool                            dryRun)
{
    IDTX_LOG(IDTX_INFO, "mpu  target={}", opts.target);

    double targetMpu = 1.0;
    if (!ResolveTargetMpu(opts.target, &targetMpu))
    {
        IDTX_LOG(IDTX_ERROR,
            "Invalid --target '{}'. Use one of meter/m, cm, mm, feet/ft, "
            "inch/in or a positive meters-per-unit value.", opts.target);
        return 1;
    }

    IDTX_LOG(IDTX_INFO, "Resolved target metersPerUnit = {}", targetMpu);

    int failures = 0;
    for (const auto& inputPath : inputFiles)
    {
        if (!ProcessStage(inputPath, outputDir, targetMpu, dryRun))
            ++failures;
    }

    if (failures > 0)
        IDTX_LOG(IDTX_ERROR, "{} file(s) failed to process MPU", failures);

    return failures == 0 ? 0 : 1;
}

} // namespace idtx::commands
