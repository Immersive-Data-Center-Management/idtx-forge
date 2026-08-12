/**
 * @file extend.h
 * @brief Declaration of the "extend" CLI subcommand registration and execution.
 *
 * Usage examples:
 *   idtx-forge -i mesh.usd -o ./out extend --behavior preserve
 *   idtx-forge -i mesh.usd -o ./out extend --behavior overwrite
 *
 * The subcommand traverses every prim of the given USD stage(s) and, for each
 * prim that is a UsdGeomBoundable, (re)computes its extent using the USD
 * built-in helper UsdGeomBoundable::ComputeExtentFromPlugins().
 *
 * Behaviours
 * ----------
 *   preserve  - Existing authored extents are kept untouched. The extent is
 *               only authored for prims that have no previously authored
 *               extent.
 *   overwrite - The extent is always (re)computed. When a previously authored
 *               extent exists and differs from the newly computed one, the new
 *               value is authored and a warning is logged showing the old and
 *               the new value. When the values match, the prim is skipped.
 **/
#pragma once

#include <string>
#include <vector>

#include <thirdparty/cli11/CLI11.hpp>

namespace idtx::commands {

// ---------------------------------------------------------------------------
// ExtendOptions
// ---------------------------------------------------------------------------
/// All parsed options owned by the extend subcommand.
struct ExtendOptions
{
    std::string behavior = "preserve";  ///< "preserve" or "overwrite"
    bool        includeReferenced = false;  ///< when true, also process prims brought in via reference/payload arcs
};

// ---------------------------------------------------------------------------
// RegisterExtendCommand
// ---------------------------------------------------------------------------
/**
 * @brief Registers the "extend" subcommand on the given CLI::App.
 *
 * Adds the subcommand together with all its options and flags. The parsed
 * values are written into @p opts; the struct must remain valid until after
 * CLI11_PARSE() returns.
 *
 * @param app   The parent CLI application to attach the subcommand to.
 * @param opts  Output struct that receives the parsed option values.
 * @return      Pointer to the newly registered CLI::App subcommand.
 */
CLI::App* RegisterExtendCommand(CLI::App& app, ExtendOptions& opts);

// ---------------------------------------------------------------------------
// RunExtendCommand
// ---------------------------------------------------------------------------
/**
 * @brief Executes the extent (re)computation on each input file.
 *
 * Opens every USD stage listed in @p inputFiles, computes and authors the
 * extent for every UsdGeomBoundable prim according to the selected behaviour
 * and writes the result to @p outputDir. Output files are named
 * "<original_stem>_extend.<original_ext>".
 *
 * @param inputFiles  Paths to the input USD files to process.
 * @param outputDir   Directory where the output USD files are written.
 * @param opts        Extend options (behaviour).
 * @param dryRun      When true the function validates inputs but writes nothing.
 * @return            0 on full success, 1 if one or more files failed.
 */
int RunExtendCommand(
    const std::vector<std::string>& inputFiles,
    const std::string&              outputDir,
    const ExtendOptions&            opts,
    bool                            dryRun);

} // namespace idtx::commands