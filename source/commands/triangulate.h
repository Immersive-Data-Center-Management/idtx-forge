/**
 * @file triangulate.h
 * @brief Declaration of the "triangulate" CLI subcommand registration and execution.
 *
 * Usage examples:
 *   idtx-forge -i mesh.usd -o ./out triangulate --algorithm fan
 *   idtx-forge -i mesh.usd -o ./out triangulate --algorithm beauty
 *   idtx-forge -i mesh.usd -o ./out triangulate --algorithm fan --native
 *
 * The subcommand triangulates every UsdGeomMesh prim found in the given USD
 * stage(s). Two algorithm modes are supported:
 *
 *   fan    - Simple fan triangulation anchored at the first vertex of each polygon.
 *   beauty - Angle-optimised ear-clipping triangulation that maximises the minimum
 *            interior angle of the produced triangles (better quality for convex
 *            polygons, requires vertex positions to be present on the mesh).
 **/
#pragma once

#include <string>
#include <vector>

#include <thirdparty/cli11/CLI11.hpp>

namespace idtx::commands {

// ---------------------------------------------------------------------------
// TriangulateOptions
// ---------------------------------------------------------------------------
/// All parsed options owned by the triangulate subcommand.
struct TriangulateOptions
{
    std::string algorithm;                  ///< "fan" or "beauty"
    bool        includeReferenced = false;  ///< when true, also process prims brought in via reference/payload arcs
};

// ---------------------------------------------------------------------------
// RegisterTriangulateCommand
// ---------------------------------------------------------------------------
/**
 * @brief Registers the "triangulate" subcommand on the given CLI::App.
 *
 * Adds the subcommand together with all its options and flags. The parsed
 * values are written into @p opts; the struct must remain valid until after
 * CLI11_PARSE() returns.
 *
 * @param app   The parent CLI application to attach the subcommand to.
 * @param opts  Output struct that receives the parsed option values.
 * @return      Pointer to the newly registered CLI::App subcommand.
 */
CLI::App* RegisterTriangulateCommand(CLI::App& app, TriangulateOptions& opts);

// ---------------------------------------------------------------------------
// RunTriangulateCommand
// ---------------------------------------------------------------------------
/**
 * @brief Executes the triangulate operation on each input file.
 *
 * Opens every USD stage listed in @p inputFiles, triangulates all UsdGeomMesh
 * prims and writes the result to @p outputDir. Output files are named
 * "<original_stem>_triangulated.<original_ext>".
 *
 * @param inputFiles  Paths to the input USD files to process.
 * @param outputDir   Directory where the output USD files are written.
 * @param opts        Triangulation options (algorithm).
 * @param dryRun      When true the function validates inputs but writes nothing.
 * @return            0 on full success, 1 if one or more files failed.
 */
int RunTriangulateCommand(
    const std::vector<std::string>& inputFiles,
    const std::string&              outputDir,
    const TriangulateOptions&       opts,
    bool                            dryRun);

} // namespace idtx::commands
