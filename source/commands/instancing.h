/**
 * @file instancing.h
 * @brief Declaration of the "instancing" CLI subcommand registration and execution.
 *
 * Usage examples:
 *   idtx-forge -i pseudo.usda -o ./out instancing
 *   idtx-forge -i pseudo.usda -o ./out instancing --mode pseudo
 *   idtx-forge -i model.usdz -o ./out instancing --mode identical-mesh
 *
 * The subcommand converts prims into native USD scenegraph instances. It
 * supports two selectable modes:
 *
 *  1. `pseudo` (default): convert *pseudo-instances* into native USD
 *     scenegraph instances.
 *
 *     A pseudo-instance is a prim that references a prototype prim declared
 *     with the `over` specifier in the same layer, but that has not been
 *     marked `instanceable = true`. As a result USD's composition engine does
 *     not treat it as a real instance and cannot share the prototype between
 *     multiple references.
 *
 *     The command performs two transformations:
 *       1. For every unique prototype (the `over` prim referenced by one or
 *          more pseudo-instances) the specifier is flipped from `over` to
 *          `class`, so the prototype becomes a defining opinion suitable as an
 *          instanceable reference target while still remaining non-renderable
 *          when not referenced.
 *       2. Every pseudo-instance prim gets `instanceable = true` authored on
 *          it, turning it into a real scenegraph instance.
 *
 *     Naming convention: the existing prototype prim keeps its name; only the
 *     specifier changes. This avoids retargeting any existing references on
 *     the instance prims.
 *
 *  2. `identical-mesh`: scan the stage for *identical mesh prims* (an Xform
 *     wrapper with exactly one GeomMesh child whose geometry arrays match
 *     another such wrapper), author the first occurrence as a shared prototype
 *     `class` under `/__Prototypes__`, and turn every identical prim into a
 *     native instance that inherits that prototype. Per-instance transforms on
 *     the Xform wrapper are preserved.
 *
 * Both modes only consider prims authored on the current stage's root layer
 * (unless `--include-referenced` is passed) and leave every untouched prim
 * verbatim in the output.
 **/
#pragma once

#include <string>
#include <vector>

#include <thirdparty/cli11/CLI11.hpp>

namespace idtx::commands {

// ---------------------------------------------------------------------------
// InstancingMode
// ---------------------------------------------------------------------------
/// Selectable instancing strategies for the "instancing" subcommand.
enum class InstancingMode
{
    Pseudo,        ///< Convert pseudo-instances (over prototype + reference).
    IdenticalMesh, ///< De-duplicate identical mesh subtrees into a prototype.
};

// ---------------------------------------------------------------------------
// InstancingOptions
// ---------------------------------------------------------------------------
/// All parsed options owned by the instancing subcommand.
struct InstancingOptions
{
    /// Selected instancing mode. Defaults to Pseudo to preserve the historical
    /// behaviour of the subcommand.
    InstancingMode mode = InstancingMode::Pseudo;

    /// When true, also consider prims that are brought into the stage via
    /// reference/payload arcs (default: only prims authored on the root layer
    /// are modified). Kept for symmetry with the `reduce` subcommand.
    bool includeReferenced = false;
};

// ---------------------------------------------------------------------------
// RegisterInstancingCommand
// ---------------------------------------------------------------------------
/**
 * @brief Registers the "instancing" subcommand on the given CLI::App.
 *
 * @param app   The parent CLI application to attach the subcommand to.
 * @param opts  Output struct that receives the parsed option values.
 * @return      Pointer to the newly registered CLI::App subcommand.
 */
CLI::App* RegisterInstancingCommand(CLI::App& app, InstancingOptions& opts);

// ---------------------------------------------------------------------------
// RunInstancingCommand
// ---------------------------------------------------------------------------
/**
 * @brief Executes the selected instancing conversion on each input file.
 *
 * Opens every USD stage listed in @p inputFiles, converts prims into native
 * instances according to @p opts.mode and writes the result to @p outputDir.
 * Output files are named "<original_stem>_instanced.<original_ext>".
 *
 * @param inputFiles  Paths to the input USD files to process.
 * @param outputDir   Directory where the output USD files are written.
 * @param opts        Instancing options (mode + include-referenced flag).
 * @param dryRun      When true the function validates inputs but writes nothing.
 * @return            0 on full success, 1 if one or more files failed.
 */
int RunInstancingCommand(
    const std::vector<std::string>& inputFiles,
    const std::string&              outputDir,
    const InstancingOptions&        opts,
    bool                            dryRun);

} // namespace idtx::commands