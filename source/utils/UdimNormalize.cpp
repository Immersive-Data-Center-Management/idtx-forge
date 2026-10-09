/**
 * @file UdimNormalize.cpp
 * @brief Implementation of the UDIM-token normalization helpers.
 *
 * See UdimNormalize.h for the behavioural contract.
 **/

#include "utils/UdimNormalize.h"

#include <filesystem>
#include <regex>
#include <string>
#include <system_error>
#include <vector>

#include <pxr/pxr.h>
#include <pxr/base/vt/array.h>
#include <pxr/base/vt/value.h>
#include <pxr/usd/sdf/assetPath.h>
#include <pxr/usd/sdf/attributeSpec.h>
#include <pxr/usd/sdf/path.h>
#include <pxr/usd/sdf/schema.h>
#include <pxr/usd/sdf/valueTypeName.h>

#include <idtx/utils/Logger.h>

namespace fs = std::filesystem;
PXR_NAMESPACE_USING_DIRECTIVE

IDTX_LOG_CATEGORY("UdimNormalize")

namespace idtx::utils {

namespace {

// Normalize a single VtValue that may hold an SdfAssetPath or a
// VtArray<SdfAssetPath>. For every element whose asset path carries a
// non-standard UDIM token: when `edits` is non-null, append a UdimRewrite
// describing it (keyed by `specPath`); when `rewritten` is non-null, store the
// normalized VtValue there. Returns true iff at least one element changed.
bool NormalizeAssetValue(const VtValue&            in,
                         const SdfPath&            specPath,
                         std::vector<UdimRewrite>* edits,
                         VtValue*                  rewritten)
{
    if (in.IsHolding<SdfAssetPath>())
    {
        const SdfAssetPath asset = in.UncheckedGet<SdfAssetPath>();
        std::string normalized;
        if (!NormalizeUdimString(asset.GetAssetPath(), normalized))
            return false;

        if (edits)
            edits->push_back(UdimRewrite{specPath, asset.GetAssetPath(), normalized});
        if (rewritten)
            *rewritten = VtValue(SdfAssetPath(normalized));
        return true;
    }

    if (in.IsHolding<VtArray<SdfAssetPath>>())
    {
        VtArray<SdfAssetPath> assets = in.UncheckedGet<VtArray<SdfAssetPath>>();
        bool changed = false;
        for (SdfAssetPath& asset : assets)
        {
            std::string normalized;
            if (!NormalizeUdimString(asset.GetAssetPath(), normalized))
                continue;
            if (edits)
                edits->push_back(
                    UdimRewrite{specPath, asset.GetAssetPath(), normalized});
            asset = SdfAssetPath(normalized);
            changed = true;
        }
        if (changed && rewritten)
            *rewritten = VtValue(assets);
        return changed;
    }

    return false;
}

// Visit every asset-valued attribute spec in `layer`. For each authored value
// (the default value and every time sample) invoke
// `onValue(attr, isDefault, time, value)`. `time` is meaningful only when
// `isDefault` is false. The callback is free to mutate the layer.
template <class Fn>
void ForEachAssetValue(const SdfLayerHandle& layer, Fn&& onValue)
{
    layer->Traverse(SdfPath::AbsoluteRootPath(),
        [&](const SdfPath& specPath)
        {
            const SdfAttributeSpecHandle attr =
                layer->GetAttributeAtPath(specPath);
            if (!attr)
                return;

            // Only asset / asset[] attributes can carry UDIM tokens.
            const SdfValueTypeName tn = attr->GetTypeName();
            if (tn != SdfValueTypeNames->Asset &&
                tn != SdfValueTypeNames->AssetArray)
                return;

            // Default value (if authored).
            if (attr->HasDefaultValue())
                onValue(attr, /*isDefault=*/true, 0.0, attr->GetDefaultValue());

            // Time samples (rare for texture paths, but handled for completeness).
            for (double t : layer->ListTimeSamplesForPath(specPath))
            {
                VtValue v;
                if (layer->QueryTimeSample(specPath, t, &v))
                    onValue(attr, /*isDefault=*/false, t, v);
            }
        });
}

} // anonymous namespace

// ---------------------------------------------------------------------------
// NormalizeUdimString
// ---------------------------------------------------------------------------
bool NormalizeUdimString(const std::string& in, std::string& out)
{
    // 1) printf / Python-dict form: %(UDIM)d, with optional flags/width such as
    //    %(UDIM)04d. 2) an existing <udim> token in any case -> canonical <UDIM>.
    static const std::regex kPercentUdim(
        R"(%\(UDIM\)[-+ 0#]*[0-9]*d)",
        std::regex::icase | std::regex::optimize);
    static const std::regex kAngleUdim(
        R"(<udim>)",
        std::regex::icase | std::regex::optimize);

    std::string s = std::regex_replace(in, kPercentUdim, "<UDIM>");
    s = std::regex_replace(s, kAngleUdim, "<UDIM>");

    out = s;
    return out != in;
}

// ---------------------------------------------------------------------------
// FindUdimRewrites
// ---------------------------------------------------------------------------
std::vector<UdimRewrite> FindUdimRewrites(const SdfLayerHandle& layer)
{
    std::vector<UdimRewrite> result;
    if (!layer)
        return result;

    ForEachAssetValue(layer,
        [&](const SdfAttributeSpecHandle& attr, bool /*isDefault*/,
            double /*time*/, const VtValue& value)
        {
            NormalizeAssetValue(value, attr->GetPath(), &result,
                                /*rewritten=*/nullptr);
        });

    return result;
}

// ---------------------------------------------------------------------------
// ApplyUdimRewrites
// ---------------------------------------------------------------------------
int ApplyUdimRewrites(const SdfLayerHandle& layer)
{
    if (!layer)
        return 0;

    int rewritten = 0;
    ForEachAssetValue(layer,
        [&](const SdfAttributeSpecHandle& attr, bool isDefault, double time,
            const VtValue& value)
        {
            std::vector<UdimRewrite> edits; // per-value, to count changed strings
            VtValue newValue;
            if (!NormalizeAssetValue(value, attr->GetPath(), &edits, &newValue))
                return;

            if (isDefault)
                attr->SetDefaultValue(newValue);
            else
                layer->SetTimeSample(attr->GetPath(), time, newValue);

            rewritten += static_cast<int>(edits.size());
        });

    return rewritten;
}

// ---------------------------------------------------------------------------
// NormalizeUdimLayerFileInPlace
// ---------------------------------------------------------------------------
UdimResult NormalizeUdimLayerFileInPlace(const std::string& path, bool dryRun)
{
    UdimResult result;

    const fs::path target(path);
    const fs::path tmp =
        target.string() + ".idtxudim" + target.extension().string();
    std::error_code ec;

    {
        SdfLayerRefPtr layer = SdfLayer::FindOrOpen(path);
        if (!layer)
        {
            IDTX_LOG(IDTX_ERROR,
                "  Could not open layer '{}' for inspection", path);
            return result; // ok == false
        }

        result.rewrites = FindUdimRewrites(layer);
        result.found    = static_cast<int>(result.rewrites.size());

        if (dryRun || result.found == 0)
        {
            // Nothing to write (either reporting only, or already canonical).
            result.ok = true;
            return result;
        }

        result.rewritten = ApplyUdimRewrites(layer);
        if (result.rewritten == 0)
        {
            result.ok = true;
            return result;
        }

        fs::remove(tmp, ec); // best-effort cleanup of any stale temp

        // Export (not Save) the edited layer to a sibling temp, keeping the
        // extension so the original on-disk encoding is preserved. The layer
        // handle (and its mapping of the original file) is released at the end
        // of this scope, before we replace the original below.
        if (!layer->Export(tmp.string()))
        {
            IDTX_LOG(IDTX_ERROR, "  Failed to export normalized layer '{}'", path);
            fs::remove(tmp, ec);
            return result; // ok == false
        }
    }

    // Atomically replace the original: remove it, then move the temp into
    // place, falling back to copy+remove if the move fails cross-device.
    fs::remove(target, ec);
    std::error_code moveEc;
    fs::rename(tmp, target, moveEc);
    if (moveEc)
    {
        std::error_code copyEc;
        fs::copy_file(tmp, target, fs::copy_options::overwrite_existing, copyEc);
        fs::remove(tmp, ec);
        if (copyEc)
        {
            IDTX_LOG(IDTX_ERROR, "  Could not replace original '{}': {}",
                path, copyEc.message());
            return result; // ok == false
        }
    }

    result.ok = true;
    return result;
}

} // namespace idtx::utils
