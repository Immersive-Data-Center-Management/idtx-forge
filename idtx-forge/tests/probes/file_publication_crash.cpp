// Standalone subprocess: no USD handles/threads or test-runner state are inherited.
#include "utils/FilePublication.h"
#include <cstdlib>
#include <fstream>

namespace fs = std::filesystem;
using idtx::utils::detail::FilePublicationOps;

struct CrashOps : FilePublicationOps
{
    std::string checkpoint;
    std::error_code replace(const fs::path& replacement, const fs::path& target, const fs::path& backup) const override
    {
        if (checkpoint == "before-replace") std::_Exit(86);
        if (checkpoint == "after-replace")
        {
            const auto error = FilePublicationOps::replace(replacement, target, backup);
            if (error) return error;
            std::_Exit(86);
        }
        fs::rename(target, backup); // Model ReplaceFileW's documented partial failure.
        if (checkpoint == "after-backup") std::_Exit(86);
        return std::make_error_code(std::errc::permission_denied);
    }
    std::error_code restore(const fs::path& backup, const fs::path& target) const override
    {
        const auto error = FilePublicationOps::restore(backup, target);
        if (error) return error;
        std::_Exit(86);
    }
};

int main(int argc, char** argv)
{
    if (argc != 3) return 2;
    const std::string target(argv[1]);
    idtx::utils::FilePublication publication(fs::path(std::u8string(target.begin(), target.end())));
    if (!publication.valid()) return 3;
    std::ofstream file(publication.replacement(), std::ios::binary);
    file << std::string(96000, 'b');
    file.close();
    if (!file.good()) return 4;
    CrashOps operations;
    operations.checkpoint = argv[2];
    publication.publish(operations);
    return 5; // Every requested checkpoint must terminate without running cleanup.
}
