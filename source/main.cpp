/**
 * @file main.cpp
 * @brief Entry point and CLI definition for IDTXForge
 *
 * Usage:  idtx-forge [global options] <subcommand> [subcommand options]
 *
 * General help:    idtx-forge --help
 * Subcommand help: idtx-forge <subcommand> --help
 *
 * Examples:
 *   idtx-forge -i mesh.usd -o ./out triangulate --algorithm fan
 *   idtx-forge -v --log run.log -i a.usd b.usd reduce --algorithm qem --lods 5
 *   idtx-forge --dry-run -i model.usd collision --algorithm quickhull --shape capsule --complexity high
 **/
#include <filesystem>
#include <string>
#include <vector>
#include "version.h"

#include <pxr/base/tf/errorMark.h>

#include <idtx/utils/Logger.h>

#include <thirdparty/cli11/CLI11.hpp>
#include "commands/generatelods.h"
#include "utils/IDTXForgeLogger.h"
#include "commands/triangulate.h"
#include "commands/instancing.h"
#include "commands/extend.h"
#include "commands/normals.h"
#include "commands/tangents.h"
#include "commands/collision.h"
#include "commands/dump.h"
#include "commands/mpu.h"

namespace fs = std::filesystem;

// static instance of the specialized logger to be used for the lifetime of the app
static idtx::utils::IDTXForgeLogger g_logger;

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------
int main(int argc, char* argv[])
{
    // setup the logger
    idtx::utils::Log::set_logger(&g_logger);
    
    try
    {
        CLI::App app{
            "IDTXForge - Immersive Digital Twin Experience mesh processing tool\n"
            "Use: idtx-forge <subcommand> --help for subcommand-specific help.",
            "idtx-forge"
        };

        app.set_version_flag("--version", IDTXFORGE_VERSION_STRING, "Show version and exit");
        app.require_subcommand(1);
        app.failure_message(CLI::FailureMessage::help);

        // -----------------------------------------------------------------------
        // Global options
        // -----------------------------------------------------------------------
        bool        optVerbose = false;
        bool        optQuiet   = false;
        bool        optDryRun  = false;
        std::string optLogFile;
        std::string optOutputDir = "./output";
        std::vector<std::string> optInputFiles;

        app.add_flag("-v,--verbose",  optVerbose, "Enable verbose/debug output");
        app.add_flag("--quiet",       optQuiet,   "Suppress standard output (errors still shown)");
        app.add_flag("--dry-run",     optDryRun,  "Validate inputs but write no output files");

        app.add_option("--log", optLogFile, "Write log output to FILE")
            ->type_name("FILE");

        app.add_option("-i,--input", optInputFiles, "One or more input files to process")
            ->required()
            ->type_name("FILE [FILE ...]")
            ->check(CLI::ExistingFile);

        app.add_option("-o,--output-dir", optOutputDir, "Output directory (default: ./output)")
            ->type_name("DIR");

        // -----------------------------------------------------------------------
        // Subcommand: triangulate
        // -----------------------------------------------------------------------
        idtx::commands::TriangulateOptions triangulateOptions;
        auto* subTri = idtx::commands::RegisterTriangulateCommand(app, triangulateOptions);

        // -----------------------------------------------------------------------
        // Subcommand: instancing
        // -----------------------------------------------------------------------
        idtx::commands::InstancingOptions instancingOptions;
        auto* subInst = idtx::commands::RegisterInstancingCommand(app, instancingOptions);

        // -----------------------------------------------------------------------
        // Subcommand: extend
        // -----------------------------------------------------------------------
        idtx::commands::ExtendOptions extendOptions;
        auto* subExt = idtx::commands::RegisterExtendCommand(app, extendOptions);

        // -----------------------------------------------------------------------
        // Subcommand: reduce
        // -----------------------------------------------------------------------
        idtx::commands::LodOptions lodOptions;
        auto* subLod = idtx::commands::RegisterGenerateLodsCommand(app, lodOptions);

        // -----------------------------------------------------------------------
        // Subcommand: normals
        // -----------------------------------------------------------------------
        idtx::commands::NormalsOptions normalsOptions;
        auto* subNrm = idtx::commands::RegisterNormalsCommand(app, normalsOptions);

        // -----------------------------------------------------------------------
        // Subcommand: tangents
        // -----------------------------------------------------------------------
        idtx::commands::TangentsOptions tangentsOptions;
        auto* subTan = idtx::commands::RegisterTangentsCommand(app, tangentsOptions);

        // -----------------------------------------------------------------------
        // Subcommand: collision
        // -----------------------------------------------------------------------
        idtx::commands::CollisionOptions collisionOptions;
        auto* subCol = idtx::commands::RegisterCollisionCommand(app, collisionOptions);

        // -----------------------------------------------------------------------
        // Subcommand: dump
        // -----------------------------------------------------------------------
        idtx::commands::DumpOptions dumpOptions;
        auto* subDump = idtx::commands::RegisterDumpCommand(app, dumpOptions);

        // -----------------------------------------------------------------------
        // Subcommand: mpu
        // -----------------------------------------------------------------------
        idtx::commands::MpuOptions mpuOptions;
        auto* subMpu = idtx::commands::RegisterMpuCommand(app, mpuOptions);

        // -----------------------------------------------------------------------
        // Parse — handles --help / --version automatically; exits cleanly on error
        // -----------------------------------------------------------------------
        CLI11_PARSE(app, argc, argv);

        // -----------------------------------------------------------------------
        // Post-parse runtime setup
        // -----------------------------------------------------------------------
        IDTX_LOGF(IDTX_INFO, "{} starting", IDTXFORGE_VERSION_STRING);

        if (optDryRun)
            IDTX_LOGF(IDTX_WARN, "Dry-run mode - no files will be written");

        if (!optDryRun)
        {
            std::error_code ec;
            fs::create_directories(optOutputDir, ec);
            if (ec)
            {
                IDTX_LOGF(IDTX_ERROR, "Failed to create output directory '{}': {}", optOutputDir , ec.message());
                return 1;
            }
        }

        IDTX_LOGF(IDTX_DEBUG, "Output dir  : {}", optOutputDir);
        IDTX_LOGF(IDTX_DEBUG, "Input count : {}", optInputFiles.size());
        for (const auto& f : optInputFiles)
            IDTX_LOGF(IDTX_DEBUG, "  input: {}", f);

        pxr::TfErrorMark usd_error_mark;
        // -----------------------------------------------------------------------
        // Subcommand dispatch
        // -----------------------------------------------------------------------
        if (app.got_subcommand(subTri))
        {
            idtx::commands::RunTriangulateCommand(optInputFiles, optOutputDir, triangulateOptions, optDryRun);
        }
        else if (app.got_subcommand(subInst))
        {
            idtx::commands::RunInstancingCommand(optInputFiles, optOutputDir, instancingOptions, optDryRun);
        }
        else if (app.got_subcommand(subExt))
        {
            idtx::commands::RunExtendCommand(optInputFiles, optOutputDir, extendOptions, optDryRun);
        }    
        else if (app.got_subcommand(subLod))
        {
            idtx::commands::RunGenerateLodsCommand(optInputFiles, optOutputDir, lodOptions, optDryRun);
        }
        else if (app.got_subcommand(subNrm))
        {
            idtx::commands::RunNormalsCommand(optInputFiles, optOutputDir, normalsOptions, optDryRun);
        }
        else if (app.got_subcommand(subTan))
        {
            idtx::commands::RunTangentsCommand(optInputFiles, optOutputDir, tangentsOptions, optDryRun);
        }
        else if (app.got_subcommand(subCol))
        {
            idtx::commands::RunCollisionCommand(optInputFiles, optOutputDir, collisionOptions, optDryRun);
        }
        else if (app.got_subcommand(subDump))
        {
            idtx::commands::RunDumpCommand(optInputFiles, optOutputDir, dumpOptions, optDryRun);
        }
        else if (app.got_subcommand(subMpu))
        {
            idtx::commands::RunMpuCommand(optInputFiles, optOutputDir, mpuOptions, optDryRun);
        }
        
        if (!usd_error_mark.IsClean())
        {
            IDTX_LOGF(IDTX_ERROR, "USD raised errors:");
            for (const pxr::TfError& error : usd_error_mark)
            {
                IDTX_LOGF(IDTX_ERROR, "  {}", error.GetCommentary().c_str());
            }
        }
    } catch (std::exception& e)
    {
        IDTX_LOGF(IDTX_ERROR, "{}", e.what());
    }

    IDTX_LOGF(IDTX_INFO, "Done.");
    return 0;
}
