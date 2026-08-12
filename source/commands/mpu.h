/**
 * @file mpu.h
 * @brief Declaration of the "mpu" CLI subcommand registration and execution.
 *
 * Usage examples:
 *   idtx-forge -i model.usd -o ./out mpu --target meter
 *   idtx-forge -i model.usd -o ./out mpu --target cm
 *   idtx-forge -i model.usd -o ./out mpu --target 0.01
 *
 * The subcommand adjusts the stage's `metersPerUnit` (MPU) metadata to a new
 * target value while keeping the rendered/physical size of the model the same.
 *
 * How it works
 * ------------
 * The rendered real-world size of a point authored at coordinate `p` is
 * `p * currentMPU` meters. When we change the metadata to `targetMPU` the same
 * authored coordinate would suddenly represent `p * targetMPU` meters, changing
 * the physical size. To compensate we scale the whole scene by
 *
 *     scale = currentMPU / targetMPU
 *
 * so that `(p * scale) * targetMPU == p * currentMPU`, i.e. the logical size is
 * preserved.
 *
 * Instead of rewriting every mesh and every transform, the scale is applied to
 * the stage's root prim (the layer's default prim). If the stage has no default
 * prim, a single Xform root prim is injected, all existing root-level prims are
 * reparented under it, and it is set as the default prim with an identity
 * transform plus the compensating scale.
 *
 * Target units
 * ------------
 * The `--target` option accepts either a raw meters-per-unit value (e.g. 0.01)
 * or one of the common real-world unit names:
 *   meter / m   -> 1.0
 *   cm          -> 0.01
 *   mm          -> 0.001
 *   feet / ft   -> 0.3048
 *   inch / in   -> 0.0254
 **/
#pragma once

#include <string>
#include <vector>

#include <thirdparty/cli11/CLI11.hpp>

namespace idtx::commands {

// ---------------------------------------------------------------------------
// MpuOptions
// ---------------------------------------------------------------------------
/// All parsed options owned by the mpu subcommand.
struct MpuOptions
{
    /// Target unit. Either a unit name (meter, m, cm, mm, feet, ft, inch, in)
    /// or a raw meters-per-unit value (e.g. "0.01").
    std::string target = "meter";
};

// ---------------------------------------------------------------------------
// RegisterMpuCommand
// ---------------------------------------------------------------------------
/**
 * @brief Registers the "mpu" subcommand on the given CLI::App.
 *
 * @param app   The parent CLI application to attach the subcommand to.
 * @param opts  Output struct that receives the parsed option values.
 * @return      Pointer to the newly registered CLI::App subcommand.
 */
CLI::App* RegisterMpuCommand(CLI::App& app, MpuOptions& opts);

// ---------------------------------------------------------------------------
// RunMpuCommand
// ---------------------------------------------------------------------------
/**
 * @brief Executes the MPU adjustment on each input file.
 *
 * Opens every USD stage listed in @p inputFiles, adjusts the stage's
 * metersPerUnit metadata to the requested target and applies a compensating
 * scale on the (possibly injected) root prim so the physical size is preserved.
 * The result is written to @p outputDir as "<original_stem>_mpu.<original_ext>".
 *
 * @param inputFiles  Paths to the input USD files to process.
 * @param outputDir   Directory where the output USD files are written.
 * @param opts        MPU options (target unit / value).
 * @param dryRun      When true the function validates inputs but writes nothing.
 * @return            0 on full success, 1 if one or more files failed.
 */
int RunMpuCommand(
    const std::vector<std::string>& inputFiles,
    const std::string&              outputDir,
    const MpuOptions&               opts,
    bool                            dryRun);

} // namespace idtx::commands