/**
 * @file ReferenceCleanup.h
 * @brief Detect and remove dangling composition arcs (references, payloads and
 *        sublayers) from a single USD layer, in place.
 *
 * A "dangling" arc is one whose target asset can never be opened:
 *   - AnonymousLayer: the asset path is an anonymous layer identifier
 *     (`anon:0x...:...`). These are session-local, in-memory identifiers that
 *     some DCCs (notably Houdini/Solaris) write into exported files by mistake.
 *     They can never resolve in any other process, so they are always dead.
 *   - Unresolvable: the asset path is a normal path that the asset resolver
 *     cannot resolve to any existing asset (e.g. a referenced file was deleted
 *     or never shipped).
 *
 * Arcs with an empty asset path (internal/self references) are never dangling
 * and are left untouched. Removing an arc strips only the arc itself; the prim
 * spec that carried it is left in place even if it becomes empty.
 **/
#pragma once

#include <string>
#include <vector>

#include <pxr/pxr.h>
#include <pxr/usd/ar/resolverContext.h>
#include <pxr/usd/sdf/layer.h>
#include <pxr/usd/sdf/path.h>

namespace idtx::utils {

/// Which kind of composition arc a dangling target was reached through.
enum class ArcKind
{
    Reference,
    Payload,
    SubLayer,
    Attribute ///< an asset-valued attribute (e.g. a texture `inputs:file`)
};

/// Why an arc is considered dangling.
enum class DanglingReason
{
    AnonymousLayer, ///< target is an anonymous (session-local) layer identifier
    Unresolvable    ///< target is a normal path that resolves to nothing
};

/// A single dangling composition arc found in a layer.
struct DanglingArc
{
    ArcKind          kind    = ArcKind::Reference;
    PXR_NS::SdfPath  primPath;            ///< prim carrying the arc (root for sublayers; the attribute property path for ArcKind::Attribute)
    std::string      assetPath;           ///< the dead target asset path
    std::string      targetPrimPath;      ///< reference/payload target prim (may be empty)
    DanglingReason   reason  = DanglingReason::AnonymousLayer;
};

/// Human-readable label for a reason (for logging).
const char* ToString(DanglingReason reason);
/// Human-readable label for an arc kind (for logging).
const char* ToString(ArcKind kind);

// ---------------------------------------------------------------------------
// FindDanglingArcs
// ---------------------------------------------------------------------------
/**
 * @brief Enumerate every dangling reference/payload/sublayer arc in @p layer.
 *
 * The layer is inspected purely in memory; nothing is modified. All prim specs
 * are visited, including those nested inside variant sets.
 *
 * @param layer    The layer to inspect.
 * @param context  Resolver context bound while resolving non-anonymous asset
 *                 paths, so relative paths are judged exactly as the owning
 *                 stage would resolve them.
 * @param includeAttributes  When true, also inspect asset-valued attributes
 *                 (e.g. texture `inputs:file`) and report any whose path cannot
 *                 be resolved as @ref ArcKind::Attribute. Asset paths carrying an
 *                 unexpanded template token (`<UDIM>`, `<UVTILE>`, `%(...)` ...)
 *                 are skipped, since the resolver expands them at runtime and
 *                 they never resolve to a single on-disk file. Default false.
 * @return         Every dangling arc found (empty if the layer is clean).
 */
std::vector<DanglingArc> FindDanglingArcs(const PXR_NS::SdfLayerHandle&     layer,
                                          const PXR_NS::ArResolverContext&  context,
                                          bool includeAttributes = false);

// ---------------------------------------------------------------------------
// RemoveDanglingArcs
// ---------------------------------------------------------------------------
/**
 * @brief Remove the given dangling arcs from @p layer, in memory.
 *
 * Only the arcs are stripped; prim specs are never deleted. The actual stored
 * arc objects are matched and removed so layer offsets / custom data on the
 * arc do not defeat equality. For @ref ArcKind::Attribute the unresolvable asset
 * value is removed instead: a single-valued asset attribute is removed from its
 * prim entirely, while an `asset[]` attribute keeps its resolvable entries and
 * only drops the dead ones (and is itself removed if that empties it).
 *
 * @param layer     The layer to edit.
 * @param arcs      The arcs to remove (typically from @ref FindDanglingArcs).
 * @param anonOnly  When true, only arcs with reason AnonymousLayer are removed;
 *                  Unresolvable arcs (including every attribute arc) are left in
 *                  place.
 * @return          The number of arcs actually removed.
 */
int RemoveDanglingArcs(const PXR_NS::SdfLayerHandle&   layer,
                       const std::vector<DanglingArc>& arcs,
                       bool                            anonOnly);

// ---------------------------------------------------------------------------
// CleanupLayerFileInPlace
// ---------------------------------------------------------------------------
/// Outcome of a dangling-arc cleanup pass over a single on-disk layer file.
struct CleanRefsResult
{
    bool                     ok      = false; ///< false => the file could not be processed
    int                      found   = 0;     ///< dangling arcs detected
    int                      removed = 0;     ///< dangling arcs removed (0 in dry-run)
    std::vector<DanglingArc> arcs;            ///< every arc found (for reporting)
};

/**
 * @brief Detect and (unless dry-run) remove dangling arcs in a layer file,
 *        rewriting it in place.
 *
 * The file is opened, inspected and - when at least one removable arc is found
 * and @p dryRun is false - re-serialized to a sibling temp file which then
 * atomically replaces the original. In dry-run mode the file is only inspected.
 *
 * @param path     Plain filesystem path to the layer (not a package-relative
 *                 path; `.usdz` entries must be extracted first).
 * @param anonOnly Only remove anonymous-identifier arcs (see RemoveDanglingArcs).
 * @param dryRun   When true, inspect and report only; never write.
 * @param context  Resolver context for the Unresolvable check.
 * @param includeTextures  When true, also detect and remove unresolvable
 *                 asset-valued attributes (textures); see @ref FindDanglingArcs.
 *                 Default false.
 * @return         A populated result. @ref CleanRefsResult::ok is false only if
 *                 the layer could not be opened or the rewrite failed.
 */
CleanRefsResult CleanupLayerFileInPlace(const std::string&               path,
                                        bool                             anonOnly,
                                        bool                             dryRun,
                                        const PXR_NS::ArResolverContext& context,
                                        bool                             includeTextures = false);

} // namespace idtx::utils
