/**
 * @file collision.cpp
 * @brief Implementation of the "collision" CLI subcommand.
 *
 * Generates optimised collision shapes for every UsdGeomMesh prim in the
 * given USD file(s).
 *
 * Algorithms
 * ----------
 * primitive
 *   Fit a primitive shape (box, sphere, capsule, cylinder) around the mesh
 *   geometry. Cheapest at runtime but least accurate. The specific shape is
 *   selected via the --shape option.
 *
 * quickhull
 *   Generate a convex hull via the QuickHull algorithm. More accurate than a
 *   primitive while remaining convex and cheap to collide against.
 *
 * The --complexity option controls the level of detail of the generated
 * collision geometry (low, medium, high).
 *
 * NOTE: This is currently a skeleton - the actual generation is not yet
 *       implemented.
 **/

#include "commands/collision.h"

#include <string>
#include <vector>

#include <idtx/utils/Logger.h>

IDTX_LOG_CATEGORY("Collision")

namespace idtx::commands {

// ===========================================================================
// Public API
// ===========================================================================

CLI::App* RegisterCollisionCommand(CLI::App& app, CollisionOptions& opts)
{
    auto* sub = app.add_subcommand("collision",
        "Generate optimised collision shapes for the mesh\n"
        "  primitive - fit a primitive shape (box, sphere, capsule, cylinder)\n"
        "  quickhull - generate a convex hull via the QuickHull algorithm");

    sub->add_option("--algorithm", opts.algorithm,
            "Collision shape algorithm to use")
        ->required()
        ->type_name("primitive|quickhull")
        ->check(CLI::IsMember({"primitive", "quickhull"}));

    sub->add_option("--shape", opts.shape,
            "Primitive shape to fit (default: box)")
        ->type_name("box|sphere|capsule|cylinder")
        ->check(CLI::IsMember({"box", "sphere", "capsule", "cylinder"}));

    sub->add_option("--complexity", opts.complexity,
            "Collision geometry complexity (default: medium)")
        ->type_name("low|medium|high")
        ->check(CLI::IsMember({"low", "medium", "high"}));

    return sub;
}

int RunCollisionCommand(
    const std::vector<std::string>& inputFiles,
    const std::string&              outputDir,
    const CollisionOptions&         opts,
    bool                            dryRun)
{
    IDTX_LOGF(IDTX_INFO, "collision  algorithm={}  shape={}  complexity={}",
        opts.algorithm, opts.shape, opts.complexity);

    // TODO: Implement the collision shape generation.
    //       for (const auto& inputPath : inputFiles)
    //           if (!ProcessStage(inputPath, outputDir, opts, dryRun)) ...

    (void)inputFiles;
    (void)outputDir;
    (void)dryRun;

    return 0;
}

} // namespace idtx::commands