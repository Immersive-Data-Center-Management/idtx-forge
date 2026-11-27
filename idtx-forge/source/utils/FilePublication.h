/**
 * @file FilePublication.h
 * @brief Recoverable publication of completed, same-volume file rewrites.
 */
#pragma once

#include <filesystem>
#include <string>
#include <system_error>

namespace idtx::utils
{

namespace detail
{
// Per-call operation seam for failure/crash tests; no process-global hooks.
struct FilePublicationOps
{
    virtual ~FilePublicationOps() = default;
    virtual std::error_code sync(const std::filesystem::path& replacement) const;
    virtual std::error_code replace(const std::filesystem::path& replacement, const std::filesystem::path& target,
                                    const std::filesystem::path& backup) const;
    virtual std::error_code restore(const std::filesystem::path& backup, const std::filesystem::path& target) const;
};
} // namespace detail

/**
 * Owns a uniquely reserved directory next to the target. Export/write to
 * replacement(), release all USD/mmap handles, then publish(). The filename's
 * extension is preserved, and relative asset strings are not rewritten.
 *
 * POSIX publication renames over the original, without unlink or copy fallback.
 * Windows uses ReplaceFileW with a backup; a partial failure attempts restoration
 * without overwriting an existing target. EVERY publication failure retains the
 * work directory and reports its recovery location. Later attempts use new paths.
 * Pre-publication export/validation failures clean up only this attempt's files.
 * This protects process-crash recovery, not arbitrary power-loss/filesystem faults.
 */
class FilePublication
{
  public:
    explicit FilePublication(const std::filesystem::path& target);
    ~FilePublication();
    FilePublication(const FilePublication&) = delete;
    FilePublication& operator=(const FilePublication&) = delete;

    bool valid() const
    {
        return error_.empty();
    }
    const std::string& error() const
    {
        return error_;
    }
    const std::filesystem::path& directory() const
    {
        return directory_;
    }
    const std::filesystem::path& replacement() const
    {
        return replacement_;
    }
    // A containing package rewrite must not erase a nested layer's recovery files.
    void retainForRecovery()
    {
        retain_ = true;
    }

    bool publish();
    bool publish(const detail::FilePublicationOps& operations);

  private:
    std::filesystem::path target_;
    std::filesystem::path directory_;
    std::filesystem::path replacement_;
    std::filesystem::path backup_;
    std::string error_;
    bool retain_ = false;
};

} // namespace idtx::utils
