//! UDIM texture-tiling math.
//!
//! `OpenUSD` materials reference tiled textures with a `<UDIM>` token in the file
//! path (e.g. `albedo.<UDIM>.png`). At render time a UV coordinate greater than
//! 1.0 selects a sibling tile file numbered
//!
//! ```text
//! tile = 1001 + tile_u + 10 * tile_v
//! ```
//!
//! where `tile_u = floor(u)` (0..=9, the standard 10-wide block) and
//! `tile_v = floor(v)` (0..=9 for the 100-entry table this crate uploads).
//! The right/top edge of a tile belongs to the *higher* tile (USD convention),
//! so `floor(1.0) == 1` is intentional and no epsilon is applied.
//!
//! This module is pure — no wgpu, no I/O — so the tile arithmetic is unit
//! tested in isolation. GPU array construction lives in
//! [`crate::texture_cache`]; extract-time tile discovery lives in
//! [`crate::scene_extract`].

use std::borrow::Cow;
use std::collections::BTreeSet;

use crate::convert::{ceil_to_i32, floor_to_i32};

/// The `<UDIM>` token substituted into a texture path template.
///
/// This is the only placeholder `OpenUSD`'s own asset resolution recognizes, and
/// the canonical form every function in this module expects. Paths authored
/// with an alternate convention are folded to this token by
/// [`normalize_udim_token`] at ingest.
pub const UDIM_TOKEN: &str = "<UDIM>";

/// Number of tiles the per-material lookup table (and the GPU array) covers:
/// the standard 10×10 block, tiles 1001..=1100.
pub const UDIM_BLOCK_TILES: u32 = 100;

/// The lowest UDIM tile number (`tile_u = 0`, `tile_v = 0`).
pub const UDIM_BASE_TILE: u32 = 1001;

/// Returns `true` when `path` contains the `<UDIM>` token and therefore names a
/// tiled texture set rather than a single file.
#[must_use]
pub fn is_udim_template(path: &str) -> bool {
    path.contains(UDIM_TOKEN)
}

/// Rewrites recognized alternate UDIM placeholders in `path` to the canonical
/// [`UDIM_TOKEN`] (`<UDIM>`), so tile discovery, per-channel masking, and
/// template expansion — all keyed on `<UDIM>` — apply uniformly regardless of
/// the tool that authored the texture path.
///
/// `OpenUSD`'s own asset resolution recognizes only `<UDIM>`. Other DCC and
/// render pipelines instead author the tile index as a printf/Python-style
/// placeholder (`%(UDIM)d`, or zero-padded like `%(UDIM)04d`) or as a
/// non-canonical-case angle form (`<udim>`, `<Udim>`). Any of these — matched
/// case-insensitively on the `UDIM` keyword — normalize to `<UDIM>`. A path
/// already using the canonical token, or naming no UDIM set at all, is returned
/// borrowed and unchanged.
///
/// The four-digit tile substitution [`expand_template`] performs matches the
/// on-disk naming both conventions produce (`1001`, `1002`, …), so rewriting the
/// token is sufficient — no width/padding metadata needs to be carried through.
#[must_use]
pub fn normalize_udim_token(path: &str) -> Cow<'_, str> {
    // Already canonical → nothing to do.
    if path.contains(UDIM_TOKEN) {
        return Cow::Borrowed(path);
    }
    // Cheap reject: no `udim` keyword in any case means no placeholder to fold.
    let lower = path.to_ascii_lowercase();
    if !lower.contains("udim") {
        return Cow::Borrowed(path);
    }

    let src = path.as_bytes();
    let low = lower.as_bytes();
    // Original UTF-8 bytes are copied verbatim and only the ASCII `<UDIM>` token
    // is inserted, so `out` is always valid UTF-8.
    let mut out: Vec<u8> = Vec::with_capacity(src.len());
    let mut i = 0;
    let mut rewrote = false;
    while i < src.len() {
        if let Some(consumed) = printf_udim_len(&low[i..]) {
            out.extend_from_slice(UDIM_TOKEN.as_bytes());
            i += consumed;
            rewrote = true;
        } else if low[i..].starts_with(b"<udim>") {
            out.extend_from_slice(UDIM_TOKEN.as_bytes());
            i += b"<udim>".len();
            rewrote = true;
        } else {
            out.push(src[i]);
            i += 1;
        }
    }

    if rewrote {
        Cow::Owned(String::from_utf8(out).unwrap_or_else(|_| path.to_owned()))
    } else {
        // `udim` appeared only in some non-placeholder context (a folder named
        // "udim", say) — leave the path untouched.
        Cow::Borrowed(path)
    }
}

/// If `b` (a lowercased path slice) begins with a printf/Python UDIM
/// placeholder — `%(udim)` followed by an optional width/flags run of ASCII
/// digits and a single conversion character (`d`, `i`, or `u`) — returns the
/// byte length of the entire placeholder. `None` otherwise.
fn printf_udim_len(b: &[u8]) -> Option<usize> {
    const PREFIX: &[u8] = b"%(udim)";
    if !b.starts_with(PREFIX) {
        return None;
    }
    let mut j = PREFIX.len();
    while b.get(j).is_some_and(u8::is_ascii_digit) {
        j += 1;
    }
    match b.get(j) {
        Some(b'd' | b'i' | b'u') => Some(j + 1),
        _ => None,
    }
}

/// Converts integer tile coordinates to a UDIM tile number, or `None` when they
/// fall outside the 10×10 block covered by the lookup table.
#[must_use]
pub fn tile_from_coords(tile_u: i32, tile_v: i32) -> Option<u32> {
    if (0..=9).contains(&tile_u) && (0..=9).contains(&tile_v) {
        // Both guarded to 0..=9 above, so the conversions always succeed.
        Some(
            UDIM_BASE_TILE
                + u32::try_from(tile_u).unwrap_or(0)
                + 10 * u32::try_from(tile_v).unwrap_or(0),
        )
    } else {
        None
    }
}

/// The UDIM tile number a `(u, v)` coordinate lands in, or `None` when it lies
/// outside the addressable 10×10 block (negative, or beyond tile column/row 9).
#[must_use]
pub fn tile_number(u: f32, v: f32) -> Option<u32> {
    if !u.is_finite() || !v.is_finite() {
        return None;
    }
    tile_from_coords(floor_to_i32(u), floor_to_i32(v))
}

/// The lookup-table / GPU-array-relative index for a tile number, or `None`
/// when the tile is outside the 1001..=1100 block.
#[must_use]
pub fn tile_index(tile: u32) -> Option<u32> {
    if (UDIM_BASE_TILE..UDIM_BASE_TILE + UDIM_BLOCK_TILES).contains(&tile) {
        Some(tile - UDIM_BASE_TILE)
    } else {
        None
    }
}

/// Expands a `<UDIM>` template to the concrete path for one tile.
///
/// UDIM tile numbers are 1001..=9999 (always four digits), so a plain decimal
/// substitution matches the on-disk naming convention.
#[must_use]
pub fn expand_template(template: &str, tile: u32) -> String {
    template.replace(UDIM_TOKEN, &tile.to_string())
}

/// The set of UDIM tiles touched by a triangulated UV layout.
///
/// Iterates per triangle (not per vertex) and inserts every tile in each
/// triangle's integer-UV bounding box, so a large triangle spanning a tile with
/// no vertex inside it is still discovered. The returned [`BTreeSet`] is sorted,
/// giving a deterministic tile→layer assignment when the GPU array is built.
///
/// `indices` are triangle-list indices into `texcoords`; triangles referencing
/// missing coordinates are skipped.
#[must_use]
pub fn touched_tiles(texcoords: &[[f32; 2]], indices: &[u32]) -> BTreeSet<u32> {
    let mut tiles = BTreeSet::new();
    for tri in indices.chunks_exact(3) {
        let Some(uvs) = tri
            .iter()
            .map(|&i| texcoords.get(i as usize).copied())
            .collect::<Option<Vec<_>>>()
        else {
            continue;
        };
        let mut umin = f32::INFINITY;
        let mut umax = f32::NEG_INFINITY;
        let mut vmin = f32::INFINITY;
        let mut vmax = f32::NEG_INFINITY;
        for [u, v] in uvs {
            if !u.is_finite() || !v.is_finite() {
                umin = f32::NAN;
                break;
            }
            umin = umin.min(u);
            umax = umax.max(u);
            vmin = vmin.min(v);
            vmax = vmax.max(v);
        }
        if !umin.is_finite() {
            continue;
        }
        let (tu0, tv0) = (floor_to_i32(umin), floor_to_i32(vmin));
        // The upper bound is half-open: a span ending exactly on an integer
        // edge does not enter the interior of the next tile, so `ceil(max) - 1`
        // (clamped to the lower bound) is the last covered column/row. This
        // keeps a [0,2]×[0,2] quad on tiles {1001,1002,1011,1012} rather than
        // spuriously pulling in column/row 2.
        let tu1 = (ceil_to_i32(umax) - 1).max(tu0);
        let tv1 = (ceil_to_i32(vmax) - 1).max(tv0);
        for tv in tv0..=tv1 {
            for tu in tu0..=tu1 {
                if let Some(tile) = tile_from_coords(tu, tv) {
                    tiles.insert(tile);
                }
            }
        }
    }
    tiles
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_udim_templates() {
        assert!(is_udim_template("albedo.<UDIM>.png"));
        assert!(is_udim_template("/abs/dir/tex_<UDIM>_diffuse.exr"));
        assert!(!is_udim_template("albedo.1001.png"));
        assert!(!is_udim_template("plain.png"));
    }

    #[test]
    fn normalizes_printf_udim_placeholders() {
        // The exact form seen in the wild that triggered this fix.
        assert_eq!(
            normalize_udim_token("../../bar_island_sink_%(UDIM)d_BaseColor.exr"),
            "../../bar_island_sink_<UDIM>_BaseColor.exr"
        );
        // Zero-padded / explicit-width variants and other conversion letters.
        assert_eq!(normalize_udim_token("t_%(UDIM)04d.png"), "t_<UDIM>.png");
        assert_eq!(normalize_udim_token("t_%(UDIM)4d.png"), "t_<UDIM>.png");
        assert_eq!(normalize_udim_token("t_%(UDIM)i.png"), "t_<UDIM>.png");
        assert_eq!(normalize_udim_token("t_%(udim)u.png"), "t_<UDIM>.png");
        // After normalization the tiled pipeline recognizes it.
        assert!(is_udim_template(&normalize_udim_token("a_%(UDIM)d.exr")));
    }

    #[test]
    fn normalizes_non_canonical_angle_case() {
        assert_eq!(normalize_udim_token("a.<udim>.png"), "a.<UDIM>.png");
        assert_eq!(normalize_udim_token("a.<Udim>.png"), "a.<UDIM>.png");
    }

    #[test]
    fn normalize_leaves_canonical_and_non_udim_paths_untouched() {
        // Canonical token is returned borrowed, unchanged.
        assert!(matches!(
            normalize_udim_token("a.<UDIM>.png"),
            Cow::Borrowed("a.<UDIM>.png")
        ));
        // A plain file, and one whose only "udim" is a folder name (no
        // placeholder), are left alone.
        assert!(matches!(
            normalize_udim_token("plain.1001.png"),
            Cow::Borrowed("plain.1001.png")
        ));
        assert!(matches!(
            normalize_udim_token("/tex/udim_set/color.png"),
            Cow::Borrowed("/tex/udim_set/color.png")
        ));
        // `%(UDIM)` without a valid printf conversion char is not a placeholder.
        assert_eq!(
            normalize_udim_token("weird_%(UDIM)_x.png"),
            "weird_%(UDIM)_x.png"
        );
    }

    #[test]
    fn normalize_preserves_non_ascii_bytes() {
        // Multi-byte UTF-8 around the token must survive the byte-level rewrite.
        assert_eq!(
            normalize_udim_token("téxtüré_%(UDIM)d_café.exr"),
            "téxtüré_<UDIM>_café.exr"
        );
    }

    #[test]
    fn tile_numbers_at_origin_and_neighbours() {
        assert_eq!(tile_number(0.0, 0.0), Some(1001));
        assert_eq!(tile_number(0.5, 0.5), Some(1001));
        assert_eq!(tile_number(1.5, 0.3), Some(1002));
        assert_eq!(tile_number(0.2, 1.9), Some(1011));
        assert_eq!(tile_number(1.5, 1.5), Some(1012));
    }

    #[test]
    fn tile_boundary_belongs_to_higher_tile() {
        // USD convention: an exact integer UV lands in the next tile.
        assert_eq!(tile_number(1.0, 0.0), Some(1002));
        assert_eq!(tile_number(0.0, 1.0), Some(1011));
    }

    #[test]
    fn tile_edges_of_the_block() {
        assert_eq!(tile_number(9.9, 0.0), Some(1010));
        assert_eq!(tile_number(9.9, 9.9), Some(1100));
        // Beyond the addressable 10×10 block.
        assert_eq!(tile_number(10.0, 0.0), None);
        assert_eq!(tile_number(0.0, 10.0), None);
    }

    #[test]
    fn negative_and_non_finite_uvs_have_no_tile() {
        assert_eq!(tile_number(-0.5, 0.0), None);
        assert_eq!(tile_number(0.0, -0.1), None);
        assert_eq!(tile_number(f32::NAN, 0.0), None);
        assert_eq!(tile_number(f32::INFINITY, 0.0), None);
    }

    #[test]
    fn tile_index_maps_the_block_only() {
        assert_eq!(tile_index(1001), Some(0));
        assert_eq!(tile_index(1002), Some(1));
        assert_eq!(tile_index(1100), Some(99));
        assert_eq!(tile_index(1000), None);
        assert_eq!(tile_index(1101), None);
    }

    #[test]
    fn expands_template_to_concrete_tile_paths() {
        assert_eq!(expand_template("a.<UDIM>.png", 1001), "a.1001.png");
        assert_eq!(
            expand_template("/d/t_<UDIM>_c.exr", 1012),
            "/d/t_1012_c.exr"
        );
        // No token → unchanged.
        assert_eq!(expand_template("plain.png", 1001), "plain.png");
    }

    #[test]
    fn touched_tiles_over_a_quad_spanning_four_tiles() {
        // A quad spanning u,v in [0,2] as two triangles.
        let uv = [[0.0, 0.0], [2.0, 0.0], [2.0, 2.0], [0.0, 2.0]];
        let idx = [0, 1, 2, 0, 2, 3];
        let tiles = touched_tiles(&uv, &idx);
        assert_eq!(
            tiles.into_iter().collect::<Vec<_>>(),
            vec![1001, 1002, 1011, 1012]
        );
    }

    #[test]
    fn touched_tiles_catches_a_triangle_crossing_a_tile_with_no_interior_vertex() {
        // A single triangle whose bounding box spans two horizontal tiles even
        // though each vertex sits in a different one — the middle column must
        // still be discovered via the bbox sweep.
        let uv = [[0.5, 0.5], [2.5, 0.5], [1.5, 0.9]];
        let idx = [0, 1, 2];
        let tiles = touched_tiles(&uv, &idx);
        assert_eq!(
            tiles.into_iter().collect::<Vec<_>>(),
            vec![1001, 1002, 1003]
        );
    }

    #[test]
    fn touched_tiles_skips_triangles_with_missing_indices() {
        let uv = [[0.5, 0.5], [0.7, 0.7], [0.9, 0.6]];
        // Second triangle references index 9 which does not exist → skipped,
        // no panic; only the first (valid) triangle contributes.
        let idx = [0, 1, 2, 0, 1, 9];
        assert_eq!(
            touched_tiles(&uv, &idx).into_iter().collect::<Vec<_>>(),
            vec![1001]
        );
    }

    #[test]
    fn touched_tiles_excludes_out_of_block_columns() {
        // Triangle bbox spans u in [0.5, 1.5] but v negative for one vertex →
        // negative rows are dropped, only the in-block tiles survive.
        let uv = [[0.5, 0.5], [1.5, 0.5], [1.5, -0.5]];
        let tiles = touched_tiles(&uv, &[0, 1, 2]);
        // v range [-0.5, 0.5] → rows -1 and 0; row -1 dropped. u range → cols 0,1.
        assert_eq!(tiles.into_iter().collect::<Vec<_>>(), vec![1001, 1002]);
    }

    #[test]
    fn touched_tiles_empty_inputs() {
        assert!(touched_tiles(&[], &[]).is_empty());
        assert!(touched_tiles(&[[0.5, 0.5]], &[]).is_empty());
    }
}
