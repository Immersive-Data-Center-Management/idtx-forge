/**
 * @file normals.h
 * @brief Declaration of the "normals" CLI subcommand registration and execution.
 *
 * Usage examples:
 *   idtx-forge -i mesh.usd -o ./out normals --algorithm faceweighted
 *   idtx-forge -i mesh.usd -o ./out normals --algorithm angleweighted
 *   idtx-forge -i mesh.usd -o ./out normals --algorithm faceweighted --behaviour overwrite
 *
 * The subcommand calculates vertex normals for every UsdGeomMesh prim found in
 * the given USD stage(s). Two weighting algorithms are supported:
 *
 *   faceweighted  - Weight the contribution of each adjacent polygon by its
 *                   face area, producing smooth normals biased toward larger
 *                   faces.
 *   angleweighted - Weight the contribution of each adjacent polygon by the
 *                   interior angle at the shared vertex, producing normals that
 *                   are less sensitive to tessellation density.
 *
 * Existing normals can either be preserved (only filling in missing normals)
 * or overwritten (recalculating and replacing all normals) via the
 * `--behaviour` option.
 **/
#pragma once

#include <string>
#include <vector>

#include <thirdparty/cli11/CLI11.hpp>

namespace idtx::commands {

// ---------------------------------------------------------------------------
// NormalsOptions
// ---------------------------------------------------------------------------
/// All parsed options owned by the normals subcommand.
struct NormalsOptions
{
    std::string algorithm;              ///< "faceweighted" or "angleweighted"
    std::string behaviour = "preserve"; ///< "preserve" or "overwrite"
    bool        includeReferenced = false;  ///< when true, also process prims brought in via reference/payload arcs
};

// ---------------------------------------------------------------------------
// RegisterNormalsCommand
// ---------------------------------------------------------------------------
/**
 * @brief Registers the "normals" subcommand on the given CLI::App.
 *
 * Adds the subcommand together with all its options and flags. The parsed
 * values are written into @p opts; the struct must remain valid until after
 * CLI11_PARSE() returns.
 *
 * @param app   The parent CLI application to attach the subcommand to.
 * @param opts  Output struct that receives the parsed option values.
 * @return      Pointer to the newly registered CLI::App subcommand.
 */
CLI::App* RegisterNormalsCommand(CLI::App& app, NormalsOptions& opts);

// ---------------------------------------------------------------------------
// RunNormalsCommand
// ---------------------------------------------------------------------------
/**
 * @brief Executes the normals calculation on each input file.
 *
 * Opens every USD stage listed in @p inputFiles, calculates normals for all
 * UsdGeomMesh prims and writes the result to @p outputDir. Output files are
 * named "<original_stem>_normals.<original_ext>".
 *
 * @param inputFiles  Paths to the input USD files to process.
 * @param outputDir   Directory where the output USD files are written.
 * @param opts        Normals options (algorithm, behaviour).
 * @param dryRun      When true the function validates inputs but writes nothing.
 * @return            0 on full success, 1 if one or more files failed.
 */
int RunNormalsCommand(
    const std::vector<std::string>& inputFiles,
    const std::string&              outputDir,
    const NormalsOptions&           opts,
    bool                            dryRun);

} // namespace idtx::commands