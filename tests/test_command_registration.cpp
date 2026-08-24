/**
 * @file test_command_registration.cpp
 * @brief Tests the Register*Command() entry points of every subcommand.
 *
 * These tests build a CLI::App, register each subcommand and then parse a
 * representative argv to verify the options land in the right option struct.
 * They deliberately do not run any USD processing - option wiring is a common
 * source of regressions after refactorings, so it gets its own focused suite.
 */

#include "thirdparty/doctest/doctest.h"

#include <string>
#include <vector>

#include <thirdparty/cli11/CLI11.hpp>

#include "commands/collision.h"
#include "commands/dump.h"
#include "commands/extend.h"
#include "commands/generatelods.h"
#include "commands/instancing.h"
#include "commands/mpu.h"
#include "commands/normals.h"
#include "commands/tangents.h"
#include "commands/triangulate.h"

using namespace idtx::commands;

namespace {

// Parse a fixed argv (no exceptions escaping) and report success/failure.
// CLI11 parses argv[1..], so callers pass just the tokens after the program
// name; we prepend a dummy program name here.
bool ParseArgs(CLI::App& app, std::vector<std::string> tokens)
{
    std::vector<char*> argv;
    std::string prog = "idtx-forge";
    argv.push_back(prog.data());
    for (auto& t : tokens)
        argv.push_back(t.data());
    try
    {
        app.parse(static_cast<int>(argv.size()), argv.data());
        return true;
    }
    catch (const CLI::ParseError&)
    {
        return false;
    }
}

} // namespace

TEST_CASE("triangulate: registers subcommand and parses --algorithm")
{
    CLI::App app;
    app.require_subcommand(0);
    TriangulateOptions opts;
    CLI::App* sub = RegisterTriangulateCommand(app, opts);
    REQUIRE(sub != nullptr);
    CHECK(sub->get_name() == "triangulate");

    REQUIRE(ParseArgs(app, {"triangulate", "--algorithm", "beauty"}));
    CHECK(app.got_subcommand(sub));
    CHECK(opts.algorithm == "beauty");
}

TEST_CASE("instancing: default mode is Pseudo, --mode selects identical-mesh")
{
    SUBCASE("default")
    {
        CLI::App app;
        app.require_subcommand(0);
        InstancingOptions opts;
        CLI::App* sub = RegisterInstancingCommand(app, opts);
        REQUIRE(sub != nullptr);
        CHECK(sub->get_name() == "instancing");
        REQUIRE(ParseArgs(app, {"instancing"}));
        CHECK(opts.mode == InstancingMode::Pseudo);
    }

    SUBCASE("identical-mesh")
    {
        CLI::App app;
        app.require_subcommand(0);
        InstancingOptions opts;
        RegisterInstancingCommand(app, opts);
        REQUIRE(ParseArgs(app, {"instancing", "--mode", "identical-mesh"}));
        CHECK(opts.mode == InstancingMode::IdenticalMesh);
    }
}

TEST_CASE("extend: default behaviour is preserve, --behavior overwrite parses")
{
    CLI::App app;
    app.require_subcommand(0);
    ExtendOptions opts;
    CLI::App* sub = RegisterExtendCommand(app, opts);
    REQUIRE(sub != nullptr);
    CHECK(sub->get_name() == "extend");

    CHECK(opts.behavior == "preserve"); // default before parsing
    REQUIRE(ParseArgs(app, {"extend", "--behavior", "overwrite"}));
    CHECK(opts.behavior == "overwrite");
}

TEST_CASE("reduce: parses algorithm and lod count")
{
    CLI::App app;
    app.require_subcommand(0);
    LodOptions opts;
    CLI::App* sub = RegisterGenerateLodsCommand(app, opts);
    REQUIRE(sub != nullptr);

    REQUIRE(ParseArgs(app, {sub->get_name(), "--algorithm", "qem"}));
    CHECK(opts.algorithm == "qem");
    CHECK(app.got_subcommand(sub));
}

TEST_CASE("normals: parses algorithm and behaviour")
{
    CLI::App app;
    app.require_subcommand(0);
    NormalsOptions opts;
    CLI::App* sub = RegisterNormalsCommand(app, opts);
    REQUIRE(sub != nullptr);
    CHECK(sub->get_name() == "normals");

    REQUIRE(ParseArgs(app,
        {"normals", "--algorithm", "angleweighted", "--behaviour", "overwrite"}));
    CHECK(opts.algorithm == "angleweighted");
    CHECK(opts.behaviour == "overwrite");
}

TEST_CASE("tangents: parses algorithm")
{
    CLI::App app;
    app.require_subcommand(0);
    TangentsOptions opts;
    CLI::App* sub = RegisterTangentsCommand(app, opts);
    REQUIRE(sub != nullptr);
    CHECK(sub->get_name() == "tangents");

    REQUIRE(ParseArgs(app, {"tangents", "--algorithm", "mikktspace"}));
    CHECK(opts.algorithm == "mikktspace");
}

TEST_CASE("collision: parses shape and complexity with sensible defaults")
{
    CLI::App app;
    app.require_subcommand(0);
    CollisionOptions opts;
    CLI::App* sub = RegisterCollisionCommand(app, opts);
    REQUIRE(sub != nullptr);
    CHECK(sub->get_name() == "collision");

    // defaults
    CHECK(opts.shape == "box");
    CHECK(opts.complexity == "medium");

    // --algorithm is required by the collision subcommand.
    REQUIRE(ParseArgs(app,
        {"collision", "--algorithm", "primitive",
         "--shape", "sphere", "--complexity", "high"}));
    CHECK(opts.algorithm == "primitive");
    CHECK(opts.shape == "sphere");
    CHECK(opts.complexity == "high");
}

TEST_CASE("collision: missing required --algorithm is rejected")
{
    CLI::App app;
    app.require_subcommand(0);
    CollisionOptions opts;
    RegisterCollisionCommand(app, opts);
    // Without the required --algorithm the parse must fail.
    CHECK_FALSE(ParseArgs(app, {"collision", "--shape", "box"}));
}

TEST_CASE("dump: registers subcommand")
{
    CLI::App app;
    app.require_subcommand(0);
    DumpOptions opts;
    CLI::App* sub = RegisterDumpCommand(app, opts);
    REQUIRE(sub != nullptr);
    CHECK(sub->get_name() == "dump");

    REQUIRE(ParseArgs(app, {"dump"}));
    CHECK(app.got_subcommand(sub));
}

TEST_CASE("mpu: default target is meter, --target parses named + raw values")
{
    SUBCASE("default")
    {
        CLI::App app;
        app.require_subcommand(0);
        MpuOptions opts;
        CLI::App* sub = RegisterMpuCommand(app, opts);
        REQUIRE(sub != nullptr);
        CHECK(sub->get_name() == "mpu");
        CHECK(opts.target == "meter");
    }

    SUBCASE("named unit")
    {
        CLI::App app;
        app.require_subcommand(0);
        MpuOptions opts;
        RegisterMpuCommand(app, opts);
        REQUIRE(ParseArgs(app, {"mpu", "--target", "cm"}));
        CHECK(opts.target == "cm");
    }

    SUBCASE("raw value")
    {
        CLI::App app;
        app.require_subcommand(0);
        MpuOptions opts;
        RegisterMpuCommand(app, opts);
        REQUIRE(ParseArgs(app, {"mpu", "--target", "0.01"}));
        CHECK(opts.target == "0.01");
    }
}

TEST_CASE("unknown option is rejected by the parser")
{
    CLI::App app;
    app.require_subcommand(0);
    TriangulateOptions opts;
    RegisterTriangulateCommand(app, opts);
    CHECK_FALSE(ParseArgs(app, {"triangulate", "--nonexistent-flag"}));
}