/**
 * @file cleanrefs.h
 * @brief Declaration of the "cleanrefs" CLI subcommand.
 *
 * Usage examples:
 *   idtx-forge -i scene.usd cleanrefs
 *   idtx-forge --dry-run -i scene.usd cleanrefs
 *   idtx-forge -i scene.usd cleanrefs --no-recursive
 *   idtx-forge -i scene.usd cleanrefs --anon-only
 *   idtx-forge -i scene.usd cleanrefs --udim
 *   idtx-forge -i scene.usd cleanrefs --textures
 *
 * The subcommand detects and removes "dangling" composition arcs - references,
 * payloads and sublayers whose target asset can never be opened - and rewrites
 * the affected layers in place.
 *
 * Two kinds of dangling target are recognised:
 *   - anonymous layer identifiers (`anon:0x...:...`): session-local, in-memory
 *     identifiers that some DCCs (notably Houdini/Solaris) incorrectly bake into
 *     exported files. They can never resolve in any other process. This is the
 *     cause of the common "Could not open asset @anon:...@" composition warning.
 *   - unresolvable paths: normal asset paths that the resolver cannot resolve to
 *     any existing asset.
 *
 * For every input stage it recursively discovers all layers that participate in
 * the stage's composition (sublayers, references and payloads), including layers
 * packaged inside `.usdz` archives, strips the dangling arcs from each one and
 * re-serializes it in place. Removing an arc from a `.usdz` entry unpacks,
 * cleans and re-packs the archive, preserving every other asset verbatim.
 *
 * The subcommand edits inputs in place and therefore ignores the global
 * `--output-dir`. It honours the global `--dry-run` flag: in dry-run mode it
 * lists every dangling arc and its containing layer, writing nothing.
 **/
#pragma once

#include <string>
#include <vector>

#include <thirdparty/cli11/CLI11.hpp>

namespace idtx::commands {

// ---------------------------------------------------------------------------
// CleanRefsOptions
// ---------------------------------------------------------------------------
/// All parsed options owned by the cleanrefs subcommand.
struct CleanRefsOptions
{
    /// When true (default) the command follows reference/payload/sublayer arcs
    /// and cleans every layer the input stage depends on, including layers
    /// packaged inside `.usdz` archives. When false only the explicitly named
    /// input file(s) are considered.
    bool recursive = true;

    /// When true, only remove arcs that point at an anonymous layer identifier
    /// (the always-safe case). Unresolvable on-disk arcs are still reported but
    /// left in place. Default false: both kinds are removed.
    bool anonOnly = false;

    /// When true, additionally scan asset-valued attributes for non-standard
    /// UDIM tokens (`%(UDIM)d` and its width variants, or a lower/mixed-case
    /// `<udim>`) and rewrite them to the canonical OpenUSD `<UDIM>` token. This
    /// pass runs in addition to the dangling-arc cleanup. Default false.
    bool udim = false;

    /// When true, additionally detect asset-valued attributes (e.g. a texture
    /// `inputs:file`) whose path cannot be resolved and remove them from the
    /// prim: a single-valued attribute is removed entirely, an `asset[]` keeps
    /// its resolvable entries and only drops the dead ones. Asset paths carrying
    /// an unexpanded template token (`<UDIM>`, `%(...)`, ...) are never touched.
    /// Default false.
    bool textures = false;
};

// ---------------------------------------------------------------------------
// RegisterCleanRefsCommand
// ---------------------------------------------------------------------------
/**
 * @brief Registers the "cleanrefs" subcommand on the given CLI::App.
 *
 * @param app   The parent CLI application to attach the subcommand to.
 * @param opts  Output struct that receives the parsed option values.
 * @return      Pointer to the newly registered CLI::App subcommand.
 */
CLI::App* RegisterCleanRefsCommand(CLI::App& app, CleanRefsOptions& opts);

// ---------------------------------------------------------------------------
// RunCleanRefsCommand
// ---------------------------------------------------------------------------
/**
 * @brief Detects and removes dangling arcs reachable from each input stage.
 *
 * @param inputFiles  Paths to the input USD stages to process.
 * @param outputDir   Unused: the command edits inputs in place.
 * @param opts        Cleanup options.
 * @param dryRun      When true, only report; no file is written.
 * @return            0 on full success, 1 if any stage/layer/package failed to
 *                    process.
 */
int RunCleanRefsCommand(
    const std::vector<std::string>& inputFiles,
    const std::string&              outputDir,
    const CleanRefsOptions&         opts,
    bool                            dryRun);

} // namespace idtx::commands
