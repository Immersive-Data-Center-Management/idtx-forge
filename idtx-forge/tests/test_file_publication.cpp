#include "thirdparty/doctest/doctest.h"
#include "utils/FilePublication.h"

#include <chrono>
#include <fstream>
#include <iterator>

#ifdef _WIN32
#include <windows.h>
#else
#include <unistd.h>
#endif

namespace fs = std::filesystem;
using idtx::utils::FilePublication;
using idtx::utils::detail::FilePublicationOps;

namespace
{
struct Scratch
{
    fs::path path =
        fs::temp_directory_path() /
        ("idtx-publication-tests-" + std::to_string(std::chrono::steady_clock::now().time_since_epoch().count()));
    Scratch() { REQUIRE(fs::create_directory(path)); }
    ~Scratch() { std::error_code error; fs::remove_all(path, error); }
};

void Write(const fs::path& path, const std::string& bytes)
{
    std::ofstream file(path, std::ios::binary);
    file << bytes;
    file.close();
    REQUIRE(file.good());
}

std::string Read(const fs::path& path)
{
    std::ifstream file(path, std::ios::binary);
    REQUIRE(file.good());
    return {std::istreambuf_iterator<char>(file), std::istreambuf_iterator<char>()};
}

struct FailReplace : FilePublicationOps
{
    std::errc failure = std::errc::permission_denied;
    mutable bool called = false;
    std::error_code replace(const fs::path&, const fs::path&, const fs::path&) const override
    {
        called = true;
        return std::make_error_code(failure);
    }
};

struct FailSync : FailReplace
{
    std::error_code sync(const fs::path&) const override { return std::make_error_code(std::errc::io_error); }
};

struct PartialReplace : FilePublicationOps
{
    bool failRestore = false;
    mutable fs::path backupPath;
    std::error_code replace(const fs::path&, const fs::path& target, const fs::path& backup) const override
    {
        backupPath = backup;
        fs::rename(target, backup);
        return std::make_error_code(std::errc::permission_denied);
    }
    std::error_code restore(const fs::path& backup, const fs::path& target) const override
    {
        if (failRestore) return std::make_error_code(std::errc::permission_denied);
        return FilePublicationOps::restore(backup, target);
    }
};
} // namespace

TEST_CASE("FilePublication replaces complete generations and cleans only its own workspace")
{
    Scratch scratch;
    for (const char* extension : {".usd", ".usdc", ".usdz"})
    {
        const fs::path target = scratch.path / (std::string("scene") + extension);
        Write(target, std::string(64000, 'a'));
        fs::path directory;
        {
            FilePublication publication(target);
            REQUIRE(publication.valid());
            directory = publication.directory();
            CHECK(publication.replacement().extension() == target.extension());
            CHECK(directory.parent_path() == target.parent_path());
            Write(publication.replacement(), std::string(96000, 'b'));
            REQUIRE(publication.publish());
            CHECK(Read(target) == std::string(96000, 'b'));
        }
        CHECK_FALSE(fs::exists(directory));
    }
}

TEST_CASE("FilePublication retains complete files after sync and rename failures and across retries")
{
    Scratch scratch;
    const fs::path target = scratch.path / "scene.usdc";
    Write(target, "original");
    FailReplace operations;
    SUBCASE("permission failure") { operations.failure = std::errc::permission_denied; }
    SUBCASE("cross-device failure has no destructive copy fallback")
    {
        operations.failure = std::errc::cross_device_link;
    }
    fs::path retained;
    {
        FilePublication publication(target);
        REQUIRE(publication.valid());
        retained = publication.replacement();
        Write(retained, "replacement");
        REQUIRE_FALSE(publication.publish(operations));
        CHECK(operations.called);
        CHECK(publication.error().find(retained.string()) != std::string::npos);
    }
    CHECK(Read(target) == "original");
    CHECK(Read(retained) == "replacement");
    {
        FilePublication retry(target);
        REQUIRE(retry.valid());
        CHECK(retry.replacement() != retained);
        Write(retry.replacement(), "retry");
        REQUIRE(retry.publish());
    }
    CHECK(Read(target) == "retry");
    CHECK(Read(retained) == "replacement");

    FailSync sync;
    fs::path unsynced;
    {
        FilePublication publication(target);
        unsynced = publication.replacement();
        Write(unsynced, "complete but unsynced");
        REQUIRE_FALSE(publication.publish(sync));
        CHECK_FALSE(sync.called);
    }
    CHECK(Read(target) == "retry");
    CHECK(Read(unsynced) == "complete but unsynced");
}

TEST_CASE("FilePublication recovers Windows-style partial failures without losing either generation")
{
    Scratch scratch;
    const fs::path target = scratch.path / "scene.usdz";
    Write(target, "original");
    PartialReplace operations;
    SUBCASE("restore succeeds") { operations.failRestore = false; }
    SUBCASE("restore fails but backup and replacement remain") { operations.failRestore = true; }
    fs::path retained;
    {
        FilePublication publication(target);
        retained = publication.replacement();
        Write(retained, "replacement");
        REQUIRE_FALSE(publication.publish(operations));
        CHECK(publication.error().find(operations.backupPath.string()) != std::string::npos);
    }
    CHECK(Read(retained) == "replacement");
    if (operations.failRestore)
    {
        CHECK_FALSE(fs::exists(target));
        CHECK(Read(operations.backupPath) == "original");
    }
    else CHECK(Read(target) == "original");
}

TEST_CASE("FilePublication cleans abandoned exports but can retain nested package recovery files")
{
    Scratch scratch;
    const fs::path target = scratch.path / "scene.usdz";
    Write(target, "original package");
    fs::path directory;
    {
        FilePublication abandoned(target);
        directory = abandoned.directory();
        Write(abandoned.replacement(), "partial export");
    }
    CHECK_FALSE(fs::exists(directory));
    CHECK(Read(target) == "original package");
    fs::path retained;
    {
        FilePublication package(target);
        const fs::path layer = package.directory() / "layer.usdc";
        Write(layer, "original layer");
        FilePublication nested(layer);
        retained = nested.replacement();
        Write(retained, "replacement layer");
        FailReplace failure;
        REQUIRE_FALSE(nested.publish(failure));
        package.retainForRecovery();
    }
    CHECK(Read(retained) == "replacement layer");
    CHECK(Read(target) == "original package");
}

TEST_CASE("FilePublication handles native publication permission failures")
{
    Scratch scratch;
    const fs::path target = scratch.path / "scene.usd";
    Write(target, "original");
    FilePublication publication(target);
    REQUIRE(publication.valid());
    Write(publication.replacement(), "replacement");
#ifdef _WIN32
    // Deny delete-sharing, as a retained USD/mmap handle would do.
    const HANDLE held = CreateFileW(target.c_str(), GENERIC_READ, FILE_SHARE_READ, nullptr, OPEN_EXISTING,
                                    FILE_ATTRIBUTE_NORMAL, nullptr);
    REQUIRE(held != INVALID_HANDLE_VALUE);
    const bool published = publication.publish();
    CloseHandle(held);
#else
    if (::geteuid() == 0) return; // Root bypasses the permission fixture.
    fs::permissions(scratch.path, fs::perms::owner_read | fs::perms::owner_exec);
    const bool published = publication.publish();
    fs::permissions(scratch.path, fs::perms::owner_all);
#endif
    REQUIRE_FALSE(published);
    CHECK(Read(target) == "original");
    CHECK(Read(publication.replacement()) == "replacement");
}

#ifndef _WIN32
TEST_CASE("FilePublication preserves original POSIX permissions")
{
    Scratch scratch;
    const fs::path target = scratch.path / "scene.usdc";
    Write(target, "original");
    fs::permissions(target, fs::perms::owner_read | fs::perms::owner_write);
    FilePublication publication(target);
    Write(publication.replacement(), "replacement");
    REQUIRE(publication.publish());
    CHECK(fs::status(target).permissions() == (fs::perms::owner_read | fs::perms::owner_write));
}
#endif
