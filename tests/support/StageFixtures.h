/**
 * @file StageFixtures.h
 * @brief Helpers for locating and loading the on-disk USD test fixtures and
 *        for managing throw-away output directories used by the idtx-forge
 *        unit tests.
 *
 * The test USD stages live as plain `.usda` files under `tests/data/`. The
 * build copies that folder next to the test executable (see tests/SConscript),
 * so at run time they are found relative to the current working directory
 * (`./data`) or via the `IDTX_FORGE_TEST_DATA_DIR` environment variable, which
 * the build also sets to the source `tests/data` folder as a convenience for
 * running the tests straight from the build tree.
 *
 * Commands operate on files and write their results into an output directory;
 * the @ref TempDir helper provides a unique, self-cleaning scratch directory
 * for those outputs.
 */
#pragma once

#include <atomic>
#include <cstdlib>
#include <filesystem>
#include <string>
#include <thread>
#include <vector>

#include <pxr/usd/usd/stage.h>

namespace idtx::tests {

namespace fs = std::filesystem;

// ---------------------------------------------------------------------------
// Test data directory resolution
// ---------------------------------------------------------------------------

/**
 * @brief Locate the directory that holds the `.usda` test fixtures.
 *
 * Resolution order:
 *   1. The `IDTX_FORGE_TEST_DATA_DIR` environment variable (set by the build).
 *   2. `<cwd>/data` (the folder copied next to the installed test binary).
 *   3. `<cwd>/tests/data` (running from the project root).
 *
 * The first existing candidate wins. The result is cached on first use.
 */
inline const fs::path& DataDir()
{
    static const fs::path dir = [] {
        std::vector<fs::path> candidates;
        if (const char* env = std::getenv("IDTX_FORGE_TEST_DATA_DIR"))
            candidates.emplace_back(env);
        candidates.emplace_back(fs::current_path() / "data");
        candidates.emplace_back(fs::current_path() / "tests" / "data");

        for (const auto& c : candidates)
        {
            std::error_code ec;
            if (fs::exists(c, ec) && fs::is_directory(c, ec))
                return c;
        }
        // Fall back to the first candidate so the failure message is useful.
        return candidates.front();
    }();
    return dir;
}

/// Absolute path to a named fixture file inside the test data directory.
inline std::string DataFile(const std::string& name)
{
    return (DataDir() / name).string();
}

/// Open a fixture stage by file name (relative to the test data directory).
inline pxr::UsdStageRefPtr OpenDataStage(const std::string& name)
{
    return pxr::UsdStage::Open(DataFile(name));
}

// ---------------------------------------------------------------------------
// TempDir - a unique temporary directory that removes itself on destruction.
// ---------------------------------------------------------------------------

/**
 * @brief A unique temporary directory that removes itself on destruction.
 *
 * Every instance creates a fresh directory under the system temp path so tests
 * running in parallel (or repeatedly) never collide. The directory and all its
 * contents are recursively deleted when the object goes out of scope.
 */
class TempDir
{
public:
    TempDir()
    {
        static std::atomic<unsigned> counter{0};
        const auto tid = static_cast<unsigned long long>(
            std::hash<std::thread::id>{}(std::this_thread::get_id()));
        const unsigned n = counter.fetch_add(1);
        path_ = fs::temp_directory_path() /
                ("idtx_forge_tests_" + std::to_string(tid) + "_" +
                 std::to_string(n));
        std::error_code ec;
        fs::create_directories(path_, ec);
    }

    ~TempDir()
    {
        std::error_code ec;
        fs::remove_all(path_, ec);
    }

    TempDir(const TempDir&)            = delete;
    TempDir& operator=(const TempDir&) = delete;

    /// The absolute path to the temporary directory.
    const fs::path& path() const { return path_; }

    /// Convenience: a child path inside the temporary directory as a string.
    std::string file(const std::string& name) const
    {
        return (path_ / name).string();
    }

    /// Copy a fixture file from the data dir into this temp dir and return the
    /// destination path. Useful when a command must be pointed at a writable
    /// copy or when a stable input path inside the scratch area is desired.
    std::string copyFixture(const std::string& name) const
    {
        const fs::path dst = path_ / name;
        std::error_code ec;
        fs::copy_file(DataFile(name), dst,
                      fs::copy_options::overwrite_existing, ec);
        return dst.string();
    }

private:
    fs::path path_;
};

} // namespace idtx::tests