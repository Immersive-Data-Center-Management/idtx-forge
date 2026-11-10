//! Post-processing of MaterialX-generated WGSL source — safety net for known generator bugs.
//!
//! The primary `MaterialX` path is `GlslShaderGenerator` → naga GLSL→WGSL transpile. naga's
//! output is standard WGSL and does not trigger the bool-in-uniform issue below. This module
//! is kept as a defensive pass in case edge cases arise with other generator configurations.
//!
//! Currently handles: `bool` fields in `var<uniform>` / `var<storage>` structs.
//! WGSL §4.4 forbids `bool` in host-shareable types; some `MaterialX` generator versions
//! emit bool fields for "enabled" flags in the public-input uniform struct.
//!
//! Fix strategy
//! ─────────────
//! 1. Find every `var<uniform>` / `var<storage...>` declaration and record its struct type.
//! 2. For each such struct definition, replace `bool`-typed fields with `u32`.
//! 3. Wrap every read of a patched field (`var_name.field`) with `!= 0u` so the
//!    expression remains `bool`-typed at all existing usage sites.
//!    Uniforms are read-only in WGSL so we never need to fix write sites.

/// Apply all known patches to `MaterialX`-generated WGSL.
/// Returns `(patched_source, diagnostics)`.
#[must_use]
pub fn patch_materialx_wgsl(src: &str) -> (String, Vec<String>) {
    patch_bool_uniform_fields(src)
}

/// Optionally dump WGSL source to the system temp directory for inspection.
///
/// Activated by setting the `IDTX_DUMP_FAILED_WGSL` env var to any value.
pub fn maybe_dump_wgsl(key: u64, label: &str, src: &str) {
    if std::env::var_os("IDTX_DUMP_FAILED_WGSL").is_none() {
        return;
    }
    let path = std::env::temp_dir().join(format!("idtx_wgsl_{label}_{key:#018x}.wgsl"));
    match std::fs::write(&path, src.as_bytes()) {
        Ok(()) => eprintln!("idtx-render: dumped WGSL ({label}) to {}", path.display()),
        Err(e) => eprintln!(
            "idtx-render: could not dump WGSL to {}: {e}",
            path.display()
        ),
    }
}

// ── internals ─────────────────────────────────────────────────────────────────

fn patch_bool_uniform_fields(src: &str) -> (String, Vec<String>) {
    let mut diags: Vec<String> = Vec::new();

    let uniform_vars = collect_uniform_vars(src);
    if uniform_vars.is_empty() {
        return (src.to_owned(), diags);
    }

    let mut out = src.to_owned();
    // Track (var_name, field_name) pairs whose type was changed to u32.
    let mut changed: Vec<(String, String)> = Vec::new();
    let mut patched_struct_types: Vec<String> = Vec::new();

    for (var_name, struct_type) in &uniform_vars {
        if patched_struct_types.contains(struct_type) {
            // Struct already patched — just register additional var bindings.
            continue;
        }
        patched_struct_types.push(struct_type.clone());

        let (new_src, fields) = patch_struct_bool_fields(&out, struct_type);
        if !fields.is_empty() {
            diags.push(format!(
                "wgsl_patch: struct '{}' (bound as '{}'): bool→u32 for field(s): {}",
                struct_type,
                var_name,
                fields.join(", ")
            ));
            // Record all var names bound to this struct type.
            for (vn, st) in &uniform_vars {
                if st == struct_type {
                    for f in &fields {
                        changed.push((vn.clone(), f.clone()));
                    }
                }
            }
        }
        out = new_src;
    }

    if changed.is_empty() {
        return (out, diags);
    }

    // Fix read sites: `var_name.field` → `(var_name.field != 0u)`.
    // Uniforms are read-only so there are no write sites to worry about.
    for (var_name, field_name) in &changed {
        let pattern = format!("{var_name}.{field_name}");
        let replacement = format!("({pattern} != 0u)");
        let prev_len = out.len();
        out = out.replace(&pattern, &replacement);
        if out.len() != prev_len {
            diags.push(format!(
                "wgsl_patch: wrapped reads of '{var_name}.{field_name}' with '!= 0u'"
            ));
        }
    }

    (out, diags)
}

/// Parse all `var<uniform>` / `var<storage...>` declarations.
/// Returns `(var_name, struct_type_name)` for each one found.
fn collect_uniform_vars(src: &str) -> Vec<(String, String)> {
    let mut result = Vec::new();
    for line in src.lines() {
        let t = line.trim();
        if t.starts_with("//") {
            continue;
        }
        if !(t.contains("var<uniform>") || t.contains("var<storage")) {
            continue;
        }
        let Some(var_pos) = t.find("var<") else {
            continue;
        };
        let after_var = &t[var_pos..];
        let Some(gt_pos) = after_var.find('>') else {
            continue;
        };
        let after_gt = after_var[gt_pos + 1..].trim();
        let Some(colon_pos) = after_gt.find(':') else {
            continue;
        };
        let var_name = after_gt[..colon_pos].trim();
        let type_raw = after_gt[colon_pos + 1..]
            .trim()
            .trim_end_matches(';')
            .trim();
        if !var_name.is_empty() && is_ident(var_name) && !type_raw.is_empty() && is_ident(type_raw)
        {
            result.push((var_name.to_string(), type_raw.to_string()));
        }
    }
    result
}

/// Within `struct struct_name { ... }`, replace every `bool`-typed field with `u32`.
/// Returns `(patched_source, changed_field_names)`.
fn patch_struct_bool_fields(src: &str, struct_name: &str) -> (String, Vec<String>) {
    let marker = format!("struct {struct_name} {{");
    let Some(struct_start) = src.find(&marker) else {
        return (src.to_owned(), Vec::new());
    };
    let body_start = struct_start + marker.len();

    // Walk to the matching closing `}`.
    let mut depth = 1usize;
    let mut end = body_start;
    #[allow(clippy::sliced_string_as_bytes)]
    for &b in src[body_start..].as_bytes() {
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            _ => {}
        }
        end += 1;
    }
    // src[body_start..end] = struct body (without closing `}`)

    let body = &src[body_start..end];
    let mut changed: Vec<String> = Vec::new();
    for line in body.lines() {
        if let Some(name) = bool_field_name(line) {
            changed.push(name.to_string());
        }
    }
    if changed.is_empty() {
        return (src.to_owned(), Vec::new());
    }

    // Replace `fieldname: bool` and `fieldname : bool` within the body.
    let mut new_body = body.to_string();
    for field in &changed {
        new_body = new_body.replace(&format!("{field}: bool"), &format!("{field}: u32"));
        new_body = new_body.replace(&format!("{field} : bool"), &format!("{field} : u32"));
    }

    let mut out = String::with_capacity(src.len());
    out.push_str(&src[..body_start]);
    out.push_str(&new_body);
    out.push_str(&src[end..]);
    (out, changed)
}

/// If `line` is a bool-typed struct field declaration, return the field name.
fn bool_field_name(line: &str) -> Option<&str> {
    let t = line.trim();
    if t.is_empty() || t.starts_with("//") || t.starts_with("/*") {
        return None;
    }
    let colon = t.find(':')?;
    let name = t[..colon].trim();
    // Must be a plain identifier (no attribute prefix, no spaces).
    if name.is_empty() || name.contains(' ') || name.contains('@') || !is_ident(name) {
        return None;
    }
    let type_part = t[colon + 1..].trim();
    // Accept `bool,` (middle field) or `bool` (last field, no comma).
    if type_part == "bool"
        || type_part == "bool,"
        || type_part.starts_with("bool,")
        || (type_part.starts_with("bool") && type_part[4..].trim_start().starts_with(','))
    {
        Some(name)
    } else {
        None
    }
}

fn is_ident(s: &str) -> bool {
    !s.is_empty()
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !s.chars().next().unwrap().is_ascii_digit()
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patches_bool_field_in_uniform_struct() {
        let src = "struct MX_Pub {\n    base: f32,\n    enabled: bool,\n    rough: f32,\n}\n\
                   @group(0) @binding(1)\nvar<uniform> mx_pub: MX_Pub;\n\n\
                   fn shade() -> vec4<f32> {\n    if mx_pub.enabled {\n        return vec4(1.0);\n    }\n    return vec4(0.0);\n}\n";

        let (patched, diags) = patch_materialx_wgsl(src);

        assert!(
            patched.contains("enabled: u32"),
            "struct field not patched:\n{patched}"
        );
        assert!(
            patched.contains("mx_pub.enabled != 0u"),
            "read site not wrapped:\n{patched}"
        );
        assert!(!diags.is_empty(), "expected diagnostics");
    }

    #[test]
    fn no_change_when_no_bool_fields() {
        let src = "struct MX_Pub {\n    base: f32,\n    rough: f32,\n}\n\
                   @group(0) @binding(0)\nvar<uniform> mx_pub: MX_Pub;\n";
        let (patched, diags) = patch_materialx_wgsl(src);
        assert_eq!(patched, src);
        assert!(diags.is_empty());
    }

    #[test]
    fn no_change_for_non_uniform_struct_with_bool() {
        // bool in a purely function-local struct must NOT be patched.
        let src = "struct Local {\n    ok: bool,\n}\nfn foo() -> bool {\n    var s: Local;\n    s.ok = true;\n    return s.ok;\n}\n";
        let (patched, diags) = patch_materialx_wgsl(src);
        assert_eq!(patched, src);
        assert!(diags.is_empty());
    }

    #[test]
    fn multiple_bool_fields_and_multiple_reads() {
        let src = "struct S {\n    a: bool,\n    b: bool,\n    c: f32,\n}\n\
                   var<uniform> u: S;\n\
                   fn f() { if u.a { } if u.b { } }\n";
        let (patched, diags) = patch_materialx_wgsl(src);
        assert!(patched.contains("a: u32"));
        assert!(patched.contains("b: u32"));
        assert!(patched.contains("u.a != 0u"));
        assert!(patched.contains("u.b != 0u"));
        assert!(diags.len() >= 2);
    }
}
