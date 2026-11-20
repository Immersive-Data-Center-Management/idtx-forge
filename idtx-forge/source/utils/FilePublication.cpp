#include "utils/FilePublication.h"

#include <atomic>
#include <chrono>

#ifdef _WIN32
#ifndef NOMINMAX
#define NOMINMAX
#endif
#include <windows.h>
#else
#include <cerrno>
#include <fcntl.h>
#include <unistd.h>
#endif

namespace fs = std::filesystem;

namespace idtx::utils
{

std::error_code detail::FilePublicationOps::sync(const fs::path& replacement) const
{
#ifdef _WIN32
    const HANDLE file =
        CreateFileW(replacement.c_str(), GENERIC_WRITE, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                    nullptr, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, nullptr);
    if (file == INVALID_HANDLE_VALUE) return {static_cast<int>(GetLastError()), std::system_category()};
    const bool flushed = FlushFileBuffers(file) != 0;
    const DWORD error = flushed ? ERROR_SUCCESS : GetLastError();
    CloseHandle(file);
    return {static_cast<int>(error), std::system_category()};
#else
    const int file = ::open(replacement.c_str(), O_RDONLY);
    if (file < 0) return {errno, std::generic_category()};
    const int flushed = ::fsync(file);
    const int error = flushed == 0 ? 0 : errno;
    ::close(file);
    return {error, std::generic_category()};
#endif
}

std::error_code detail::FilePublicationOps::replace(const fs::path& replacement, const fs::path& target,
                                                  const fs::path& backup) const
{
#ifdef _WIN32
    // Do not ignore ACL/metadata merge errors or unlink the replaced file first.
    // ReplaceFile can fail after moving the original to backup; publish handles
    // that explicitly and never deletes either recoverable generation on failure.
    if (ReplaceFileW(target.c_str(), replacement.c_str(), backup.c_str(), 0, nullptr, nullptr)) return {};
    return {static_cast<int>(GetLastError()), std::system_category()};
#else
    (void)backup;
    std::error_code error;
    fs::rename(replacement, target, error);
    return error;
#endif
}

std::error_code detail::FilePublicationOps::restore(const fs::path& backup, const fs::path& target) const
{
#ifdef _WIN32
    // No REPLACE_EXISTING: never clobber a target that appeared during recovery.
    if (MoveFileExW(backup.c_str(), target.c_str(), MOVEFILE_WRITE_THROUGH)) return {};
    return {static_cast<int>(GetLastError()), std::system_category()};
#else
    // This seam also exercises Windows-style partial failures on POSIX hosts.
    // A hard link restores the name without overwriting a racing target. Keep
    // the backup link until the work directory is explicitly recovered.
    std::error_code error;
    fs::create_hard_link(backup, target, error);
    return error;
#endif
}

FilePublication::FilePublication(const fs::path& target)
{
    std::error_code error;
    target_ = fs::absolute(target, error);
    if (!error && !fs::is_regular_file(fs::symlink_status(target_, error)))
        error = error ? error : std::make_error_code(std::errc::invalid_argument);
    if (error)
    {
        error_ = "cannot prepare file rewrite for '" + target.string() + "': " + error.message();
        return;
    }

    static std::atomic<unsigned long long> next{0};
    const auto stamp = std::chrono::steady_clock::now().time_since_epoch().count();
    for (unsigned attempt = 0; attempt < 100; ++attempt)
    {
        const fs::path candidate = target_.parent_path() /
                                   (".idtx-rewrite-" + std::to_string(stamp) + "-" + std::to_string(next.fetch_add(1)));
        if (fs::create_directory(candidate, error))
        {
            directory_ = candidate;
#ifndef _WIN32
            fs::permissions(directory_, fs::perms::owner_all, error);
#endif
            if (!error)
            {
                replacement_ = directory_ / target_.filename();
                backup_ = directory_ / "original.backup";
                return;
            }
            break;
        }
        if (error && error != std::errc::file_exists) break;
    }
    error_ = "cannot reserve file rewrite for '" + target.string() + "': " +
             (error ? error.message() : "temporary name attempts exhausted");
}

FilePublication::~FilePublication()
{
    if (!retain_ && !directory_.empty())
    {
        std::error_code error;
        // Only the newly and exclusively reserved directory belongs to us.
        fs::remove_all(directory_, error);
    }
}

bool FilePublication::publish()
{
    const detail::FilePublicationOps operations;
    return publish(operations);
}

bool FilePublication::publish(const detail::FilePublicationOps& operations)
{
    if (!valid()) return false;
    // Set BEFORE any fallible publication step. Even a sync/permission failure
    // must not let destruction (or a subsequent attempt) discard the replacement.
    retain_ = true;
    std::error_code error;
    const auto original = fs::symlink_status(target_, error);
    if (!error && !fs::is_regular_file(original)) error = std::make_error_code(std::errc::invalid_argument);
    if (!error && !fs::is_regular_file(fs::symlink_status(replacement_, error)))
        error = error ? error : std::make_error_code(std::errc::invalid_argument);
#ifndef _WIN32
    if (!error) fs::permissions(replacement_, original.permissions(), error);
#endif
    if (!error) error = operations.sync(replacement_);
    if (!error) error = operations.replace(replacement_, target_, backup_);
    if (!error)
    {
        retain_ = false;
        return true;
    }

    error_ = "could not publish '" + target_.string() + "': " + error.message();
    std::error_code checkError;
    const bool missing = !fs::exists(target_, checkError) && !checkError;
    if (missing && fs::is_regular_file(backup_, checkError) && !checkError)
    {
        const auto restoreError = operations.restore(backup_, target_);
        if (restoreError) error_ += "; backup restoration failed: " + restoreError.message();
    }
    // Some ReplaceFileW errors can move either name. Report the directory that
    // retains BOTH candidate recovery paths, rather than claiming a moved file
    // still exists at its old name. The target, if present, is never removed.
    error_ += "; recovery files retained in '" + directory_.string() + "' (replacement '" + replacement_.string() +
              "', original backup '" + backup_.string() + "'); inspect target '" + target_.string() +
              "' before retrying";
    return false;
}

} // namespace idtx::utils
