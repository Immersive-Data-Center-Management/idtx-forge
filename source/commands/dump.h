/**
 * @file dump.h
 * @brief Declaration of the "dump" CLI subcommand registration and execution.
 *
 * Usage examples:
 *   idtx-forge -i model.usd dump
 *   idtx-forge -i a.usd b.usdz dump
 *
 * The subcommand opens each input USD file and prints a summary of its
 * metadata to the log/console. No output files are written; the subcommand is
 * read-only and therefore ignores the global --output-dir and --dry-run flags.
 *
 * For every input file the following information is reported:
 *   - Root layer metadata: up-axis, meters-per-unit, default prim and whether
 *     the stage contains animation data (a non-trivial start/end time range).
 *   - Total prim count.
 *   - Number of prims without triangle faces (mesh prims that are not fully
 *     triangulated, i.e. contain at least one face with != 3 vertices, plus a
 *     count of non-mesh prims for context).
 *   - Number of pseudo-instances (see utils::IsPseudoInstance).
 *   - Number of native instance prototypes and native instances.
 *   - Stage-wide geometry totals across all mesh prims: total points, total
 *     face-vertex indices and total (fan-triangulated) triangles.
 *   - File size on disk.
 *   - The largest mesh prim by point count and by face count.
 **/
#pragma once

#include <string>
#include <vector>

#include <thirdparty/cli11/CLI11.hpp>

namespace idtx::commands {

// ---------------------------------------------------------------------------
// DumpOptions
// ---------------------------------------------------------------------------
/// All parsed options owned by the dump subcommand.
struct DumpOptions
{
    /// When true, also traverse prims brought into the stage via
    /// reference/payload arcs (default: traverse the fully composed stage).
    /// Kept for symmetry with the other subcommands.
    bool includeReferenced = false;
};

// ---------------------------------------------------------------------------
// RegisterDumpCommand
// ---------------------------------------------------------------------------
/**
 * @brief Registers the "dump" subcommand on the given CLI::App.
 *
 * @param app   The parent CLI application to attach the subcommand to.
 * @param opts  Output struct that receives the parsed option values.
 * @return      Pointer to the newly registered CLI::App subcommand.
 */
CLI::App* RegisterDumpCommand(CLI::App& app, DumpOptions& opts);

// ---------------------------------------------------------------------------
// RunDumpCommand
// ---------------------------------------------------------------------------
/**
 * @brief Executes the metadata dump on each input file.
 *
 * Opens every USD stage listed in @p inputFiles and prints a metadata summary.
 * This command is read-only; @p outputDir and @p dryRun are accepted for a
 * uniform call signature with the other subcommands but are not used.
 *
 * @param inputFiles  Paths to the input USD files to inspect.
 * @param outputDir   Unused (dump writes no output files).
 * @param opts        Dump options.
 * @param dryRun      Unused (dump never writes anything).
 * @return            0 on full success, 1 if one or more files failed to open.
 */
int RunDumpCommand(
    const std::vector<std::string>& inputFiles,
    const std::string&              outputDir,
    const DumpOptions&              opts,
    bool                            dryRun);

} // namespace idtx::commands