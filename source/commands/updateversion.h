/**
 * @file updateversion.h
 * @brief Declaration of the "updateversion" CLI subcommand.
 *
 * Usage examples:
 *   idtx-forge -i old.usdc updateversion
 *   idtx-forge -i scene.usda model.usdz updateversion
 *   idtx-forge --dry-run -i scene.usd updateversion
 *   idtx-forge -i scene.usd updateversion --no-recursive
 *
 * The subcommand updates deprecated OpenUSD binary ("crate") files to the crate
 * version written by the currently linked OpenUSD software, without shelling
 * out to the `usdcat` / `usdupdatecrate` command line tools.
 *
 * For every input stage it recursively discovers all layers that participate in
 * the stage's composition (sublayers, references and payloads), including layers
 * packaged inside `.usdz` archives, builds the de-duplicated set of binary crate
 * files whose version is older than the running software, and re-serializes each
 * one in place at the current version. Updating a `.usdz` entry unpacks, updates
 * and re-packs the archive, preserving every other asset verbatim.
 *
 * The subcommand edits inputs in place and therefore ignores the global
 * `--output-dir`. It honours the global `--dry-run` flag: in dry-run mode it
 * only reports which layers are deprecated and would be updated, writing
 * nothing. Text layers (`.usda`) carry no crate version and are reported and
 * skipped.
 **/
#pragma once

#include <string>
#include <vector>

#include <thirdparty/cli11/CLI11.hpp>

namespace idtx::commands {

// ---------------------------------------------------------------------------
// UpdateVersionOptions
// ---------------------------------------------------------------------------
/// All parsed options owned by the updateversion subcommand.
struct UpdateVersionOptions
{
    /// When true (default) the command follows reference/payload/sublayer arcs
    /// and also updates every crate layer the input stage depends on, including
    /// layers packaged inside `.usdz` archives. When false only the explicitly
    /// named input file(s) are considered.
    bool recursive = true;
};

// ---------------------------------------------------------------------------
// RegisterUpdateVersionCommand
// ---------------------------------------------------------------------------
/**
 * @brief Registers the "updateversion" subcommand on the given CLI::App.
 *
 * @param app   The parent CLI application to attach the subcommand to.
 * @param opts  Output struct that receives the parsed option values.
 * @return      Pointer to the newly registered CLI::App subcommand.
 */
CLI::App* RegisterUpdateVersionCommand(CLI::App& app, UpdateVersionOptions& opts);

// ---------------------------------------------------------------------------
// RunUpdateVersionCommand
// ---------------------------------------------------------------------------
/**
 * @brief Updates deprecated crate layers reachable from each input stage.
 *
 * @param inputFiles  Paths to the input USD stages to process.
 * @param outputDir   Unused: the command edits inputs in place.
 * @param opts        Update options.
 * @param dryRun      When true, only report; no file is written.
 * @return            0 on full success, 1 if any stage failed to open or any
 *                    layer failed to update.
 */
int RunUpdateVersionCommand(
    const std::vector<std::string>& inputFiles,
    const std::string&              outputDir,
    const UpdateVersionOptions&     opts,
    bool                            dryRun);

} // namespace idtx::commands
