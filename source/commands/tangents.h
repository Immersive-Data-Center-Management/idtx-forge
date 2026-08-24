/**
 * @file tangents.h
 * @brief Declaration of the "tangents" CLI subcommand registration and execution.
 *
 * Usage examples:
 *   idtx-forge -i mesh.usd -o ./out tangents --algorithm mikktspace
 *   idtx-forge -i mesh.usd -o ./out tangents --algorithm gramschmidt
 *
 * The subcommand calculates tangent space vectors (tangents and bitangents)
 * for every UsdGeomMesh prim found in the given USD stage(s). Tangent space is
 * required for correct normal mapping. Two algorithms are supported:
 *
 *   mikktspace  - Industry-standard MikkTSpace algorithm (recommended). Produces
 *                 tangents consistent with most DCC tools and game engines.
 *   gramschmidt - Gram-Schmidt orthogonalisation of the tangent basis derived
 *                 from UV gradients. Faster but less robust for complex meshes.
 *
 * Tangent calculation requires the mesh to have UV coordinates (texture
 * coordinates) and normals present.
 **/
#pragma once

#include <string>
#include <vector>

#include <thirdparty/cli11/CLI11.hpp>

namespace idtx::commands {

// ---------------------------------------------------------------------------
// TangentsOptions
// ---------------------------------------------------------------------------
/// All parsed options owned by the tangents subcommand.
struct TangentsOptions
{
    std::string algorithm;              ///< "mikktspace" or "gramschmidt"
    bool        includeReferenced = false;  ///< when true, also process prims brought in via reference/payload arcs
};

// ---------------------------------------------------------------------------
// RegisterTangentsCommand
// ---------------------------------------------------------------------------
/**
 * @brief Registers the "tangents" subcommand on the given CLI::App.
 *
 * Adds the subcommand together with all its options and flags. The parsed
 * values are written into @p opts; the struct must remain valid until after
 * CLI11_PARSE() returns.
 *
 * @param app   The parent CLI application to attach the subcommand to.
 * @param opts  Output struct that receives the parsed option values.
 * @return      Pointer to the newly registered CLI::App subcommand.
 */
CLI::App* RegisterTangentsCommand(CLI::App& app, TangentsOptions& opts);

// ---------------------------------------------------------------------------
// RunTangentsCommand
// ---------------------------------------------------------------------------
/**
 * @brief Executes the tangent space calculation on each input file.
 *
 * Opens every USD stage listed in @p inputFiles, calculates tangent space
 * vectors for all UsdGeomMesh prims and writes the result to @p outputDir.
 * Output files are named "<original_stem>_tangents.<original_ext>".
 *
 * @param inputFiles  Paths to the input USD files to process.
 * @param outputDir   Directory where the output USD files are written.
 * @param opts        Tangents options (algorithm).
 * @param dryRun      When true the function validates inputs but writes nothing.
 * @return            0 on full success, 1 if one or more files failed.
 */
int RunTangentsCommand(
    const std::vector<std::string>& inputFiles,
    const std::string&              outputDir,
    const TangentsOptions&          opts,
    bool                            dryRun);

} // namespace idtx::commands