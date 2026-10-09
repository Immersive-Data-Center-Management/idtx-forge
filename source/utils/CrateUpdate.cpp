/**
 * @file CrateUpdate.cpp
 * @brief Implementation of the crate-version inspection/update helpers.
 *
 * See CrateUpdate.h for the behavioural contract.
 **/

#include "utils/CrateUpdate.h"

#include <algorithm>
#include <array>
#include <cstdint>
#include <cstdio>
#include <filesystem>
#include <fstream>
#include <sstream>
#include <string>
#include <system_error>
#include <vector>

#include <pxr/pxr.h>
#include <pxr/usd/sdf/crateInfo.h>
#include <pxr/usd/sdf/layer.h>

#include <idtx/utils/Logger.h>

namespace fs = std::filesystem;
PXR_NAMESPACE_USING_DIRECTIVE

IDTX_LOG_CATEGORY("CrateUpdate")

namespace idtx::utils {

namespace {

/// Crate versions strictly older than this are deprecated. Mirrors OpenUSD's
/// OLDEST_CURRENT_VERSION / usdupdatecrate's default --version threshold.
constexpr const char* kDeprecationThreshold = "0.8.0";

/// The 8-byte identifier every binary crate file starts with.
constexpr char kCrateMagic[8] = {'P', 'X', 'R', '-', 'U', 'S', 'D', 'C'};

/// Read the crate version straight from the file header without going through
/// SdfCrateInfo. The header is an 8-byte magic followed by the version bytes
/// (major, minor, patch). Returns true and fills @p outVersion for a crate
/// file; returns false for any non-crate / unreadable file. This mirrors
/// usdupdatecrate's ReadCrateVersion and, crucially, never posts a USD runtime
/// error for non-crate inputs (e.g. `.usda` text layers), unlike
/// SdfCrateInfo::Open.
bool ReadCrateHeaderVersion(const std::string& path, std::string& outVersion)
{
    std::ifstream ifs(path, std::ios::binary);
    if (!ifs)
        return false;

    std::array<unsigned char, 11> header{};
    ifs.read(reinterpret_cast<char*>(header.data()),
             static_cast<std::streamsize>(header.size()));
    if (ifs.gcount() != static_cast<std::streamsize>(header.size()))
        return false;

    for (int i = 0; i < 8; ++i)
    {
        if (header[i] != static_cast<unsigned char>(kCrateMagic[i]))
            return false;
    }

    char buf[32];
    std::snprintf(buf, sizeof(buf), "%u.%u.%u",
                  static_cast<unsigned>(header[8]),
                  static_cast<unsigned>(header[9]),
                  static_cast<unsigned>(header[10]));
    outVersion = buf;
    return true;
}

/// Parse a dotted version token ("0.8.0") into its numeric components.
/// Non-numeric components become 0 so a malformed token never throws.
std::vector<int> ParseVersion(const std::string& version)
{
    std::vector<int> parts;
    std::stringstream ss(version);
    std::string component;
    while (std::getline(ss, component, '.'))
    {
        int value = 0;
        try
        {
            value = std::stoi(component);
        }
        catch (const std::exception&)
        {
            value = 0;
        }
        parts.push_back(value);
    }
    return parts;
}

} // anonymous namespace

// ---------------------------------------------------------------------------
// CrateDeprecationThreshold
// ---------------------------------------------------------------------------
const char* CrateDeprecationThreshold()
{
    return kDeprecationThreshold;
}

// ---------------------------------------------------------------------------
// CompareCrateVersions
// ---------------------------------------------------------------------------
int CompareCrateVersions(const std::string& a, const std::string& b)
{
    std::vector<int> va = ParseVersion(a);
    std::vector<int> vb = ParseVersion(b);

    // Zero-extend the shorter one so "0.8" compares equal to "0.8.0".
    const size_t n = std::max(va.size(), vb.size());
    va.resize(n, 0);
    vb.resize(n, 0);

    for (size_t i = 0; i < n; ++i)
    {
        if (va[i] != vb[i])
            return va[i] < vb[i] ? -1 : 1;
    }
    return 0;
}

// ---------------------------------------------------------------------------
// InspectCrateVersion
// ---------------------------------------------------------------------------
CrateVersionInfo InspectCrateVersion(const std::string& path)
{
    CrateVersionInfo info;

    // Cheap, side-effect-free crate detection by header magic. Non-crate files
    // (text .usda layers, textures, ...) fall out here without any USD error
    // being posted.
    std::string fileVersion;
    if (!ReadCrateHeaderVersion(path, fileVersion))
        return info;

    info.isCrate     = true;
    info.fileVersion = fileVersion;
    info.isOutdated =
        CompareCrateVersions(info.fileVersion, kDeprecationThreshold) < 0;

    // The file is a confirmed crate, so SdfCrateInfo::Open succeeds without
    // posting an error; use it only to report the software version for context.
    // The handle/mapping it owns is released when it goes out of scope here.
    if (SdfCrateInfo crate = SdfCrateInfo::Open(path))
        info.softwareVersion = crate.GetSoftwareVersion().GetString();

    return info;
}

// ---------------------------------------------------------------------------
// UpdateCrateFileInPlace
// ---------------------------------------------------------------------------
bool UpdateCrateFileInPlace(const std::string& path)
{
    const fs::path target(path);
    // Keep the original extension so SdfLayer::Export infers the crate (usdc)
    // file format for the temp file.
    const fs::path tmp = target.string() + ".idtxupdate" + target.extension().string();

    std::error_code ec;
    fs::remove(tmp, ec); // best-effort cleanup of any stale temp

    {
        SdfLayerRefPtr layer = SdfLayer::FindOrOpen(path);
        if (!layer)
        {
            IDTX_LOG(IDTX_ERROR, "  Could not open layer '{}' for updating", path);
            return false;
        }

        // Re-export (NOT Save): Save preserves the file's existing crate
        // version, whereas Export writes a new file at the current crate
        // version, lifting a deprecated file forward.
        if (!layer->Export(tmp.string()))
        {
            IDTX_LOG(IDTX_ERROR, "  Failed to re-export crate layer '{}'", path);
            fs::remove(tmp, ec);
            return false;
        }
        // layer (and its mapping of the original file) is released at scope end
        // before we replace the original below.
    }

    // Verify the re-exported file is actually no longer deprecated.
    const CrateVersionInfo after = InspectCrateVersion(tmp.string());
    if (!after.isCrate || after.isOutdated)
    {
        IDTX_LOG(IDTX_ERROR,
            "  Re-export of '{}' did not produce a current crate file "
            "(version '{}') - leaving original untouched",
            path, after.fileVersion);
        fs::remove(tmp, ec);
        return false;
    }

    // Atomically replace the original: remove it, then move the temp into
    // place. A plain rename cannot overwrite an existing file on every
    // platform, so remove first and fall back to copy+remove if the move fails.
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
            return false;
        }
    }

    return true;
}

} // namespace idtx::utils
