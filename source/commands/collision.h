/**
 * @file collision.h
 * @brief Declaration of the "collision" CLI subcommand registration and execution.
 *
 * Usage examples:
 *   idtx-forge -i mesh.usd -o ./out collision --algorithm primitive --shape box
 *   idtx-forge -i mesh.usd -o ./out collision --algorithm quickhull --complexity high
 *
 * The subcommand generates optimised collision shapes for every UsdGeomMesh
 * prim found in the given USD stage(s). Two algorithms are supported:
 *
 *   primitive - Fit a primitive shape (box, sphere, capsule, cylinder) around
 *               the mesh geometry. Cheapest at runtime but least accurate.
 *   quickhull - Generate a convex hull via the QuickHull algorithm. More
 *               accurate than a primitive while remaining convex.
 *
 * For the primitive algorithm a specific shape can be selected. The complexity
 * option controls the level of detail of the generated collision geometry.
 **/
#pragma once

#include <string>
#include <vector>

#include <thirdparty/cli11/CLI11.hpp>

namespace idtx::commands {

// ---------------------------------------------------------------------------
// CollisionOptions
// ---------------------------------------------------------------------------
/// All parsed options owned by the collision subcommand.
struct CollisionOptions
{
    std::string algorithm;              ///< "primitive" or "quickhull"
    std::string shape      = "box";     ///< "box", "sphere", "capsule" or "cylinder"
    std::string complexity = "medium";  ///< "low", "medium" or "high"
};

// ---------------------------------------------------------------------------
// RegisterCollisionCommand
// ---------------------------------------------------------------------------
/**
 * @brief Registers the "collision" subcommand on the given CLI::App.
 *
 * Adds the subcommand together with all its options and flags. The parsed
 * values are written into @p opts; the struct must remain valid until after
 * CLI11_PARSE() returns.
 *
 * @param app   The parent CLI application to attach the subcommand to.
 * @param opts  Output struct that receives the parsed option values.
 * @return      Pointer to the newly registered CLI::App subcommand.
 */
CLI::App* RegisterCollisionCommand(CLI::App& app, CollisionOptions& opts);

// ---------------------------------------------------------------------------
// RunCollisionCommand
// ---------------------------------------------------------------------------
/**
 * @brief Executes the collision shape generation on each input file.
 *
 * Opens every USD stage listed in @p inputFiles, generates collision shapes
 * for all UsdGeomMesh prims and writes the result to @p outputDir. Output
 * files are named "<original_stem>_collision.<original_ext>".
 *
 * @param inputFiles  Paths to the input USD files to process.
 * @param outputDir   Directory where the output USD files are written.
 * @param opts        Collision options (algorithm, shape, complexity).
 * @param dryRun      When true the function validates inputs but writes nothing.
 * @return            0 on full success, 1 if one or more files failed.
 */
int RunCollisionCommand(
    const std::vector<std::string>& inputFiles,
    const std::string&              outputDir,
    const CollisionOptions&         opts,
    bool                            dryRun);

} // namespace idtx::commands