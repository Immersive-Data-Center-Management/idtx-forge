/**
* @file generatelods.h
* @brief Declaration of the "reduce" CLI subcommand registration and execution.
 **/

#pragma once

#include <thirdparty/cli11/CLI11.hpp>

namespace idtx::commands
{
    
// ---------------------------------------------------------------------------
// LodOptions
// ---------------------------------------------------------------------------
/// All parsed options owned by the reduce/LOD subcommand.
struct LodOptions
{
    std::string  algorithm;                  ///< "qem" or "edgecollapse"
    std::uint8_t lodNum = 3;                 ///< number of LOD levels to emit (including LOD0)
    bool         noTextureReduction = false; ///< when true, textures are NOT downsampled per LOD
    bool         includeReferenced = false;  ///< when true, also process prims brought in via reference/payload arcs
};
    
// ---------------------------------------------------------------------------
// RegisterGenerateLodsCommand
// ---------------------------------------------------------------------------
/**
* @brief Registers the "reduce" subcommand on the given CLI::App.
*
* Adds the subcommand together with all its options and flags. The parsed
* values are written into @p opts; the struct must remain valid until after
* CLI11_PARSE() returns.
*
* @param app   The parent CLI application to attach the subcommand to.
* @param opts  Output struct that receives the parsed option values.
* @return      Pointer to the newly registered CLI::App subcommand.
*/
CLI::App* RegisterGenerateLodsCommand(CLI::App& app, LodOptions& opts);

// ---------------------------------------------------------------------------
// RunGenerateLodsCommand
// ---------------------------------------------------------------------------
/**
* @brief Executes the LOD generation on each input file.
*
* Opens every USD stage listed in @p inputFiles, decimates each UsdGeomMesh
* once per LOD level using the chosen algorithm, and writes the result to
* @p outputDir. Output files are named "<original_stem>_LOD<N>.<original_ext>".
* For .usdz inputs the per-LOD packages additionally have their textures
* downsampled (unless @p opts.noTextureReduction is set) - see the per-LOD
* texture scale table inside the implementation.
*
* @param inputFiles  Paths to the input USD files to process.
* @param outputDir   Directory where the output USD files are written.
* @param opts        LOD options (algorithm, lod count, texture-reduction flag).
* @param dryRun      When true the function validates inputs but writes nothing.
* @return            0 on full success, 1 if one or more files failed.
*/
int RunGenerateLodsCommand(
    const std::vector<std::string>& inputFiles,
    const std::string&              outputDir,
    const LodOptions&               opts,
    bool                            dryRun);

}