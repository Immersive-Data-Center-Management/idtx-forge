//! AAS/JSON path accessor (`no_std`, allocation-free).
//!
//! A guest-side companion to [`crate::net::json_string_field`] that resolves a
//! *path* into a JSON document and returns the scalar at that location. The path
//! grammar is a subset of the host's C++ `idtx::aas_json` accessor
//! (`crates/idtx-usd-exec/cpp/AasJson.h`) — the same `valueFromAasJson` style
//! used by the `Compute_ValueFromAASJson` node — so a behavior can read an Asset
//! Administration Shell (AAS) response the same way the compute graph does.
//!
//! # Path grammar
//!
//! Segments are `/`-separated (an optional leading `/` is ignored):
//!
//! | Segment      | Meaning                                                        |
//! |--------------|----------------------------------------------------------------|
//! | `name`       | object key lookup                                              |
//! | `name[i]`    | key `name`, then array index `i`                               |
//! | `name[idShort=X]` | key `name`, then the AAS child whose `idShort == X`        |
//! | `@X`         | shorthand for an `idShort == X` search in the current container |
//!
//! An `idShort` search (`@X` / `[idShort=X]`) descends through the AAS container
//! duality — an object's `submodelElements` array, else its `value` array — and
//! matches each element object's `idShort` field. Example paths: `/data/value`,
//! `result[0]/cursor`, `outputArguments[idShort=Seg]/@Record000/@Temp/value`.
//!
//! # Scope vs the host accessor
//!
//! This is a *subset* reimplementation, not a byte-identical port: it reads
//! well-formed JSON, compares raw (still-escaped) key/`idShort` slices, and does
//! not interpret `OperationVariable` wrappers beyond the `value`-array fallback.
//! It allocates nothing (operates on the caller's `&[u8]`), so it stays within
//! the `no_std`, allocator-free guest and is unit-testable without a host.

/// Resolve `path` into `json` and return the scalar at that location as a raw
/// `&str`: for a JSON string the inner (still-escaped) text between the quotes;
/// for a number / `true` / `false` / `null` the token verbatim. Returns [`None`]
/// when any segment fails to resolve or the document is malformed.
///
/// The returned slice borrows `json`, so it is cheap to hand straight to
/// [`crate::stage_edit::StageEdit::set_widget_value`].
#[must_use]
pub fn value_from_aas_json<'a>(json: &'a [u8], path: &str) -> Option<&'a str> {
    let root_start = skip_ws(json, 0);
    let root_end = skip_value(json, root_start)?;
    let mut cur = (root_start, root_end);

    let trimmed = path.strip_prefix('/').unwrap_or(path);
    for seg in trimmed.split('/') {
        if seg.is_empty() {
            continue;
        }
        cur = resolve_segment(json, cur.0, cur.1, seg)?;
    }
    scalar_str(json, cur.0, cur.1)
}

/// Resolve one path segment against the value spanning `[start, end)`.
fn resolve_segment(b: &[u8], start: usize, end: usize, seg: &str) -> Option<(usize, usize)> {
    // `@X` — an idShort search directly in the current container.
    if let Some(idshort) = seg.strip_prefix('@') {
        let (arr_start, arr_end) = child_array(b, start, end)?;
        return find_by_idshort(b, arr_start, arr_end, idshort);
    }

    // `name[...]` — a key lookup followed by an index or idShort selector.
    if let Some(open) = seg.find('[') {
        if !seg.ends_with(']') {
            return None;
        }
        let key = &seg[..open];
        let inside = &seg[open + 1..seg.len() - 1];
        let (val_start, val_end) = object_get(b, start, end, key)?;
        if let Some(idshort) = inside.strip_prefix("idShort=") {
            let (arr_start, arr_end) = child_array(b, val_start, val_end)?;
            return find_by_idshort(b, arr_start, arr_end, idshort);
        }
        let index: usize = inside.parse().ok()?;
        return array_index(b, val_start, val_end, index);
    }

    // Plain object key.
    object_get(b, start, end, seg)
}

// ===========================================================================
// JSON navigation primitives — span-based, no allocation.
// ===========================================================================

fn skip_ws(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

/// Index just past the string whose opening quote is at `b[i]`.
fn skip_string(b: &[u8], mut i: usize) -> Option<usize> {
    i += 1; // past the opening quote
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,              // skip the escaped byte
            b'"' => return Some(i + 1),
            _ => i += 1,
        }
    }
    None
}

/// Index just past the complete value starting at (ws-skipped) `b[i]`.
fn skip_value(b: &[u8], i: usize) -> Option<usize> {
    let i = skip_ws(b, i);
    match b.get(i)? {
        b'"' => skip_string(b, i),
        b'{' => skip_container(b, i, b'{', b'}'),
        b'[' => skip_container(b, i, b'[', b']'),
        _ => {
            // number / true / false / null — read to the next structural char.
            let mut j = i;
            while j < b.len() && !matches!(b[j], b',' | b'}' | b']') && !b[j].is_ascii_whitespace() {
                j += 1;
            }
            (j != i).then_some(j)
        }
    }
}

/// Index just past the `open`/`close`-delimited container at `b[i] == open`.
/// Only the one bracket type is counted (strings are skipped), which balances
/// correctly for well-formed JSON since nested arrays/objects reuse the pair.
fn skip_container(b: &[u8], i: usize, open: u8, close: u8) -> Option<usize> {
    let mut depth = 0usize;
    let mut j = i;
    while j < b.len() {
        let c = b[j];
        if c == b'"' {
            j = skip_string(b, j)?;
        } else if c == open {
            depth += 1;
            j += 1;
        } else if c == close {
            depth -= 1;
            j += 1;
            if depth == 0 {
                return Some(j);
            }
        } else {
            j += 1;
        }
    }
    None
}

fn is_array(b: &[u8], start: usize) -> bool {
    b.get(skip_ws(b, start)) == Some(&b'[')
}

/// Find member `key` in the object spanning `[start, end)`; returns the value's
/// span (ws-skipped start).
fn object_get(b: &[u8], start: usize, end: usize, key: &str) -> Option<(usize, usize)> {
    let obj = skip_ws(b, start);
    if b.get(obj) != Some(&b'{') {
        return None;
    }
    let mut i = skip_ws(b, obj + 1);
    while i < end {
        if b[i] == b'}' {
            break;
        }
        if b[i] != b'"' {
            return None; // malformed member
        }
        let key_start = i + 1;
        let after_key = skip_string(b, i)?;
        let this_key = core::str::from_utf8(&b[key_start..after_key - 1]).ok()?;

        let colon = skip_ws(b, after_key);
        if b.get(colon) != Some(&b':') {
            return None;
        }
        let val_start = skip_ws(b, colon + 1);
        let val_end = skip_value(b, val_start)?;
        if this_key == key {
            return Some((val_start, val_end));
        }
        i = skip_ws(b, val_end);
        if i < end && b[i] == b',' {
            i = skip_ws(b, i + 1);
        }
    }
    None
}

/// Return the span of element `index` in the array spanning `[start, end)`.
fn array_index(b: &[u8], start: usize, end: usize, index: usize) -> Option<(usize, usize)> {
    let arr = skip_ws(b, start);
    if b.get(arr) != Some(&b'[') {
        return None;
    }
    let mut i = skip_ws(b, arr + 1);
    let mut n = 0usize;
    while i < end {
        if b[i] == b']' {
            break;
        }
        let val_end = skip_value(b, i)?;
        if n == index {
            return Some((i, val_end));
        }
        n += 1;
        i = skip_ws(b, val_end);
        if i < end && b[i] == b',' {
            i = skip_ws(b, i + 1);
        }
    }
    None
}

/// Resolve the AAS "child array" of a node: an array is itself; an object yields
/// its `submodelElements` array, else its `value` array.
fn child_array(b: &[u8], start: usize, end: usize) -> Option<(usize, usize)> {
    let s = skip_ws(b, start);
    match b.get(s)? {
        b'[' => Some((s, end)),
        b'{' => {
            for key in ["submodelElements", "value"] {
                if let Some((vs, ve)) = object_get(b, s, end, key)
                    && is_array(b, vs)
                {
                    return Some((vs, ve));
                }
            }
            None
        }
        _ => None,
    }
}

/// Find the element object whose `idShort` equals `idshort` in the array
/// spanning `[start, end)`.
fn find_by_idshort(b: &[u8], start: usize, end: usize, idshort: &str) -> Option<(usize, usize)> {
    let arr = skip_ws(b, start);
    if b.get(arr) != Some(&b'[') {
        return None;
    }
    let mut i = skip_ws(b, arr + 1);
    while i < end {
        if b[i] == b']' {
            break;
        }
        let el_end = skip_value(b, i)?;
        if let Some((ks, ke)) = object_get(b, i, el_end, "idShort")
            && str_value_eq(b, ks, ke, idshort)
        {
            return Some((i, el_end));
        }
        // OperationVariable wrapper: AAS `inputVariables`/`outputArguments` entries
        // are shaped `{"value":{"idShort":...}}`. Mirror the host accessor and match
        // the inner submodel element, returning its span so the resolved value is
        // the wrapped element, not the wrapper.
        if let Some((vs, ve)) = object_get(b, i, el_end, "value")
            && let Some((ks, ke)) = object_get(b, vs, ve, "idShort")
            && str_value_eq(b, ks, ke, idshort)
        {
            return Some((vs, ve));
        }
        i = skip_ws(b, el_end);
        if i < end && b[i] == b',' {
            i = skip_ws(b, i + 1);
        }
    }
    None
}

/// `true` when the JSON string value spanning `[start, end)` equals `expected`
/// (raw, still-escaped comparison).
fn str_value_eq(b: &[u8], start: usize, end: usize, expected: &str) -> bool {
    let s = skip_ws(b, start);
    if b.get(s) != Some(&b'"') {
        return false;
    }
    match skip_string(b, s) {
        Some(close) if close - 1 <= end => &b[s + 1..close - 1] == expected.as_bytes(),
        _ => false,
    }
}

/// Extract the scalar text from the value spanning `[start, end)`: the inner text
/// of a string, else the trimmed token (number / literal). Returns [`None`] for a
/// container (object / array), which is not a scalar.
fn scalar_str(b: &[u8], start: usize, end: usize) -> Option<&str> {
    let s = skip_ws(b, start);
    match b.get(s)? {
        b'"' => {
            let close = skip_string(b, s)?;
            core::str::from_utf8(&b[s + 1..close - 1]).ok()
        }
        b'{' | b'[' => None,
        _ => {
            let mut e = end;
            while e > s && b[e - 1].is_ascii_whitespace() {
                e -= 1;
            }
            core::str::from_utf8(&b[s..e]).ok()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_nested_string() {
        let body = br#"{"data":{"value":"42.0"}}"#;
        assert_eq!(value_from_aas_json(body, "/data/value"), Some("42.0"));
        // Leading slash is optional.
        assert_eq!(value_from_aas_json(body, "data/value"), Some("42.0"));
    }

    #[test]
    fn number_scalar_returned_verbatim() {
        let body = br#"{ "data" : { "value" : 42.5 } }"#;
        assert_eq!(value_from_aas_json(body, "/data/value"), Some("42.5"));
        let ints = br#"{"data":{"value":-7}}"#;
        assert_eq!(value_from_aas_json(ints, "/data/value"), Some("-7"));
    }

    #[test]
    fn array_index_segment() {
        let body = br#"{"items":[{"x":"a"},{"x":"b"},{"x":"c"}]}"#;
        assert_eq!(value_from_aas_json(body, "items[1]/x"), Some("b"));
        assert_eq!(value_from_aas_json(body, "items[2]/x"), Some("c"));
        // Out-of-range index → None.
        assert_eq!(value_from_aas_json(body, "items[9]/x"), None);
    }

    #[test]
    fn idshort_search_under_submodel_elements() {
        let body = br#"{"submodelElements":[
            {"idShort":"Temp","value":"21.3"},
            {"idShort":"Hum","value":"55"}
        ]}"#;
        assert_eq!(value_from_aas_json(body, "@Temp/value"), Some("21.3"));
        assert_eq!(value_from_aas_json(body, "@Hum/value"), Some("55"));
        assert_eq!(value_from_aas_json(body, "@Missing/value"), None);
    }

    #[test]
    fn nested_aas_chain_through_value_containers() {
        // Mirrors the host fixture's
        // `outputArguments[idShort=DerivedSegment]/@Record000/@CPB_CPU0_Temp/value`.
        let body = br#"{
            "outputArguments":[
                {"idShort":"DerivedSegment","value":[
                    {"idShort":"Record000","value":[
                        {"idShort":"CPB_CPU0_Temp","value":"63.5"}
                    ]}
                ]}
            ]
        }"#;
        assert_eq!(
            value_from_aas_json(
                body,
                "outputArguments[idShort=DerivedSegment]/@Record000/@CPB_CPU0_Temp/value",
            ),
            Some("63.5"),
        );
    }

    #[test]
    fn missing_and_malformed_paths_are_none() {
        let body = br#"{"data":{"value":"x"}}"#;
        assert_eq!(value_from_aas_json(body, "/data/nope"), None);
        assert_eq!(value_from_aas_json(body, "/nope/value"), None);
        // A container is not a scalar.
        assert_eq!(value_from_aas_json(body, "/data"), None);
        assert_eq!(value_from_aas_json(b"", "/data"), None);
    }

    #[test]
    fn empty_path_reads_root_scalar() {
        assert_eq!(value_from_aas_json(br#""hello""#, ""), Some("hello"));
        assert_eq!(value_from_aas_json(b"123", "/"), Some("123"));
    }
}
