use std::collections::BTreeSet;

use anyhow::Result;

fn parse_location_attr(line: &str) -> Option<u32> {
    let idx = line.find("@location(")?;
    let rest = &line[idx + "@location(".len()..];
    let end = rest.find(')')?;
    rest[..end].trim().parse().ok()
}

fn parse_struct_member_name(line: &str) -> Option<&str> {
    let before_colon = line.split(':').next()?;
    before_colon.split_whitespace().last()
}

pub(crate) fn locations_in_struct(wgsl: &str, struct_name: &str) -> Result<BTreeSet<u32>> {
    let start_pat = format!("struct {struct_name} {{");
    let mut in_struct = false;
    let mut found = false;
    let mut out = BTreeSet::new();

    for line in wgsl.lines() {
        let trimmed = line.trim();
        if !in_struct {
            if trimmed == start_pat {
                in_struct = true;
                found = true;
            }
            continue;
        }

        if trimmed == "};" {
            in_struct = false;
            continue;
        }

        if let Some(loc) = parse_location_attr(line) {
            out.insert(loc);
        }
    }

    if !found {
        // Signature-driven pixel shaders can omit the `PsIn` struct entirely when they do not
        // consume any varyings (the entry point becomes `fn fs_main() -> ...`), so treat a missing
        // struct as "no @location values".
        return Ok(BTreeSet::new());
    }
    Ok(out)
}

fn location_in_fs_main_return(wgsl: &str) -> Option<u32> {
    for line in wgsl.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with("fn fs_main(") {
            continue;
        }
        let arrow_idx = line.find("->")?;
        return parse_location_attr(&line[arrow_idx..]);
    }
    None
}

pub(crate) fn declared_ps_output_locations(wgsl: &str) -> Result<BTreeSet<u32>> {
    let mut out = locations_in_struct(wgsl, "PsOut")?;
    if let Some(loc) = location_in_fs_main_return(wgsl) {
        out.insert(loc);
    }
    Ok(out)
}

pub(crate) fn referenced_ps_input_locations(wgsl: &str) -> BTreeSet<u32> {
    let bytes = wgsl.as_bytes();
    let mut out = BTreeSet::new();
    let mut i = 0usize;
    while i + 7 <= bytes.len() {
        if &bytes[i..i + 7] != b"input.v" {
            i += 1;
            continue;
        }
        let mut j = i + 7;
        let mut value: u32 = 0;
        let mut has_digit = false;
        while j < bytes.len() {
            let b = bytes[j];
            if b.is_ascii_digit() {
                has_digit = true;
                value = value.saturating_mul(10).saturating_add((b - b'0') as u32);
                j += 1;
            } else {
                break;
            }
        }
        if has_digit {
            out.insert(value);
        }
        i = j;
    }
    out
}

pub(crate) fn trim_vs_outputs_to_locations(
    vs_wgsl: &str,
    keep_locations: &BTreeSet<u32>,
) -> String {
    let mut out = String::with_capacity(vs_wgsl.len());
    let mut in_vs_out = false;

    for line in vs_wgsl.lines() {
        let trimmed = line.trim();
        if !in_vs_out && trimmed == "struct VsOut {" {
            in_vs_out = true;
            out.push_str(line);
            out.push('\n');
            continue;
        }

        if in_vs_out {
            if trimmed == "};" {
                in_vs_out = false;
                out.push_str(line);
                out.push('\n');
                continue;
            }

            if let Some(loc) = parse_location_attr(line) {
                if !keep_locations.contains(&loc) {
                    continue;
                }
            }

            out.push_str(line);
            out.push('\n');
            continue;
        }

        // Drop return-struct assignments to trimmed varyings.
        let line_trimmed_start = line.trim_start();
        if let Some(rest) = line_trimmed_start.strip_prefix("out.o") {
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if !digits.is_empty() {
                if let Ok(loc) = digits.parse::<u32>() {
                    if !keep_locations.contains(&loc) {
                        continue;
                    }
                }
            }
        }

        out.push_str(line);
        out.push('\n');
    }

    out
}

pub(crate) fn trim_ps_inputs_to_locations(ps_wgsl: &str, keep_locations: &BTreeSet<u32>) -> String {
    fn ps_in_has_builtin_members(ps_wgsl: &str) -> bool {
        let mut in_struct = false;
        for line in ps_wgsl.lines() {
            let trimmed = line.trim();
            if !in_struct {
                if trimmed == "struct PsIn {" {
                    in_struct = true;
                }
                continue;
            }

            if trimmed == "};" {
                break;
            }

            if trimmed.contains("@builtin(") {
                return true;
            }
        }
        false
    }

    if keep_locations.is_empty() && !ps_in_has_builtin_members(ps_wgsl) {
        // WGSL forbids empty structs. If trimming would remove every struct member, rewrite the
        // shader to drop `PsIn` entirely and switch `fs_main` to take no parameters.
        let mut out = String::with_capacity(ps_wgsl.len());
        let mut in_ps_in = false;
        for line in ps_wgsl.lines() {
            let trimmed = line.trim();
            if !in_ps_in && trimmed == "struct PsIn {" {
                in_ps_in = true;
                continue;
            }
            if in_ps_in {
                if trimmed == "};" {
                    in_ps_in = false;
                }
                continue;
            }

            if trimmed.starts_with("fn fs_main(") && trimmed.contains("input: PsIn") {
                let replaced = line.replace("fn fs_main(input: PsIn)", "fn fs_main()");
                out.push_str(&replaced);
                out.push('\n');
                continue;
            }

            out.push_str(line);
            out.push('\n');
        }
        return out;
    }

    let mut out = String::with_capacity(ps_wgsl.len());
    let mut in_ps_in = false;

    for line in ps_wgsl.lines() {
        let trimmed = line.trim();
        if !in_ps_in && trimmed == "struct PsIn {" {
            in_ps_in = true;
            out.push_str(line);
            out.push('\n');
            continue;
        }

        if in_ps_in {
            if trimmed == "};" {
                in_ps_in = false;
                out.push_str(line);
                out.push('\n');
                continue;
            }

            if let Some(loc) = parse_location_attr(line) {
                if !keep_locations.contains(&loc) {
                    continue;
                }
            }

            out.push_str(line);
            out.push('\n');
            continue;
        }

        out.push_str(line);
        out.push('\n');
    }

    out
}

pub(crate) fn trim_ps_outputs_to_locations(
    ps_wgsl: &str,
    keep_locations: &BTreeSet<u32>,
) -> String {
    // First pass: collect all `@location` members and which should be trimmed.
    let mut in_ps_out = false;
    let mut ps_out_found = false;
    let mut removed_member_names = std::collections::HashSet::<String>::new();
    let mut kept_member_count = 0usize;

    for line in ps_wgsl.lines() {
        let trimmed = line.trim();
        if !in_ps_out {
            if trimmed == "struct PsOut {" {
                in_ps_out = true;
                ps_out_found = true;
            }
            continue;
        }

        if trimmed == "};" {
            in_ps_out = false;
            continue;
        }

        if let Some(loc) = parse_location_attr(line) {
            let Some(name) = parse_struct_member_name(line) else {
                continue;
            };
            if !keep_locations.contains(&loc) {
                removed_member_names.insert(name.to_owned());
            } else {
                kept_member_count += 1;
            }
            continue;
        }

        // Count non-location members (e.g. `@builtin(frag_depth)` outputs) so we can decide whether
        // trimming leaves an empty struct.
        if parse_struct_member_name(line).is_some() {
            kept_member_count += 1;
        }
    }

    let mut drop_ps_out_struct = false;
    if ps_out_found && kept_member_count == 0 {
        // WGSL forbids empty structs; if trimming would remove every member (including builtins),
        // rewrite the shader to drop `PsOut` entirely and switch `fs_main` to return `()`.
        drop_ps_out_struct = true;
    }

    // Direct-return pixel shaders (`-> @location(0) vec4<f32>`) don't have a `PsOut` struct. If the
    // declared return location isn't bound, rewrite `fs_main` to return `()`.
    let mut drop_fs_return_location = false;
    if !ps_out_found {
        if let Some(loc) = location_in_fs_main_return(ps_wgsl) {
            if !keep_locations.contains(&loc) {
                drop_fs_return_location = true;
            }
        }
    }

    if !ps_out_found && !drop_fs_return_location {
        // Nothing to do.
        return ps_wgsl.to_owned();
    }

    let mut out = String::with_capacity(ps_wgsl.len());
    let mut in_ps_out = false;
    let mut trim_tmp_counter = 0usize;

    for line in ps_wgsl.lines() {
        let trimmed = line.trim();

        if drop_ps_out_struct {
            // Drop the entire `PsOut` declaration.
            if !in_ps_out && trimmed == "struct PsOut {" {
                in_ps_out = true;
                continue;
            }
            if in_ps_out {
                if trimmed == "};" {
                    in_ps_out = false;
                }
                continue;
            }

            // Rewrite `fs_main` to return `()`.
            if trimmed.starts_with("fn fs_main(") && trimmed.contains("->") {
                if let Some(arrow_idx) = line.find("->") {
                    let brace = line.find('{').unwrap_or(line.len());
                    let before = &line[..arrow_idx];
                    let after = &line[brace..];
                    out.push_str(before.trim_end());
                    out.push(' ');
                    out.push_str(after.trim_start());
                    out.push('\n');
                    continue;
                }
            }

            // Remove `var out: PsOut;`, and rewrite `return out;` into `return;` to preserve
            // early-return control flow (WGSL allows `return;` in `()`-returning entry points).
            if trimmed == "var out: PsOut;" {
                continue;
            }
            if trimmed == "return out;" {
                let line_trimmed_start = line.trim_start();
                let indent_len = line.len().saturating_sub(line_trimmed_start.len());
                let indent = &line[..indent_len];
                out.push_str(indent);
                out.push_str("return;\n");
                continue;
            }

            // Strip `out.<field> = <expr>;` lines, but preserve RHS evaluation in case it has side
            // effects.
            let line_trimmed_start = line.trim_start();
            if let Some(rest) = line_trimmed_start.strip_prefix("out.") {
                if let Some((_, rhs)) = rest.split_once('=') {
                    let rhs = rhs.trim().trim_end_matches(';').trim();
                    let indent_len = line.len().saturating_sub(line_trimmed_start.len());
                    let indent = &line[..indent_len];
                    let tmp_name = format!("_aero_trim_tmp{trim_tmp_counter}");
                    trim_tmp_counter += 1;
                    out.push_str(indent);
                    out.push_str("let ");
                    out.push_str(&tmp_name);
                    out.push_str(" = ");
                    out.push_str(rhs);
                    out.push_str(";\n");
                    continue;
                }
                // If we can't parse the assignment, just drop it (best-effort).
                continue;
            }

            out.push_str(line);
            out.push('\n');
            continue;
        }

        // Trim members of `struct PsOut`.
        if ps_out_found {
            if !in_ps_out && trimmed == "struct PsOut {" {
                in_ps_out = true;
                out.push_str(line);
                out.push('\n');
                continue;
            }

            if in_ps_out {
                if trimmed == "};" {
                    in_ps_out = false;
                    out.push_str(line);
                    out.push('\n');
                    continue;
                }

                if let Some(loc) = parse_location_attr(line) {
                    if !keep_locations.contains(&loc) {
                        continue;
                    }
                }

                out.push_str(line);
                out.push('\n');
                continue;
            }

            // Drop return-struct assignments to trimmed outputs, but preserve RHS evaluation.
            let line_trimmed_start = line.trim_start();
            if let Some(rest) = line_trimmed_start.strip_prefix("out.") {
                let ident: String = rest
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .collect();
                if !ident.is_empty() && removed_member_names.contains(&ident) {
                    if let Some((_, rhs)) = rest.split_once('=') {
                        let rhs = rhs.trim().trim_end_matches(';').trim();
                        let indent_len = line.len().saturating_sub(line_trimmed_start.len());
                        let indent = &line[..indent_len];
                        let tmp_name = format!("_aero_trim_tmp{trim_tmp_counter}");
                        trim_tmp_counter += 1;
                        out.push_str(indent);
                        out.push_str("let ");
                        out.push_str(&tmp_name);
                        out.push_str(" = ");
                        out.push_str(rhs);
                        out.push_str(";\n");
                    }
                    continue;
                }
            }

            out.push_str(line);
            out.push('\n');
            continue;
        }

        // Direct return: drop `-> @location(...) ...` and rewrite `return expr;`.
        if drop_fs_return_location {
            if trimmed.starts_with("fn fs_main(") && trimmed.contains("->") {
                if let Some(arrow_idx) = line.find("->") {
                    let brace = line.find('{').unwrap_or(line.len());
                    let before = &line[..arrow_idx];
                    let after = &line[brace..];
                    out.push_str(before.trim_end());
                    out.push(' ');
                    out.push_str(after.trim_start());
                    out.push('\n');
                    continue;
                }
            }

            let line_trimmed_start = line.trim_start();
            if let Some(rest) = line_trimmed_start.strip_prefix("return ") {
                let expr = rest.trim().trim_end_matches(';').trim();
                let indent_len = line.len().saturating_sub(line_trimmed_start.len());
                let indent = &line[..indent_len];
                let tmp_name = format!("_aero_trim_tmp{trim_tmp_counter}");
                trim_tmp_counter += 1;
                out.push_str(indent);
                out.push_str("let ");
                out.push_str(&tmp_name);
                out.push_str(" = ");
                out.push_str(expr);
                out.push_str(";\n");
                out.push_str(indent);
                out.push_str("return;\n");
                continue;
            }
        }

        out.push_str(line);
        out.push('\n');
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_locations_in_struct() {
        let wgsl = r#"
            struct PsIn {
                @builtin(position) pos: vec4<f32>,
                @location(1) v1: vec4<f32>,
                @location(10) v10: vec4<f32>,
            };
        "#;
        let locs = locations_in_struct(wgsl, "PsIn").unwrap();
        assert_eq!(locs.iter().copied().collect::<Vec<_>>(), vec![1, 10]);
    }

    #[test]
    fn trims_vs_outputs_and_out_assignments() {
        let wgsl = r#"
            struct VsOut {
                @builtin(position) pos: vec4<f32>,
                @location(1) o1: vec4<f32>,
                @location(2) o2: vec4<f32>,
            };

            @vertex
            fn vs_main() -> VsOut {
                var out: VsOut;
                out.pos = vec4<f32>(0.0);
                out.o1 = vec4<f32>(1.0);
                out.o2 = vec4<f32>(2.0);
                return out;
            }
        "#;

        let keep = BTreeSet::from([2u32]);
        let trimmed = trim_vs_outputs_to_locations(wgsl, &keep);
        assert!(!trimmed.contains("@location(1)"));
        assert!(trimmed.contains("@location(2)"));
        assert!(!trimmed.contains("out.o1 ="));
        assert!(trimmed.contains("out.o2 ="));
    }

    #[test]
    fn missing_struct_is_treated_as_empty_locations() {
        let wgsl = r#"
            @fragment
            fn fs_main() -> @location(0) vec4<f32> { return vec4<f32>(0.0); }
        "#;
        let locs = locations_in_struct(wgsl, "PsIn").unwrap();
        assert!(locs.is_empty());
    }

    #[test]
    fn finds_referenced_ps_locations() {
        let wgsl = r#"
            @fragment
            fn fs_main(input: PsIn) -> @location(0) vec4<f32> {
                let a = input.v1;
                let b = input.v10;
                return a + b;
            }
        "#;
        let refs = referenced_ps_input_locations(wgsl);
        assert_eq!(refs.iter().copied().collect::<Vec<_>>(), vec![1, 10]);
    }

    #[test]
    fn trims_ps_inputs() {
        let wgsl = r#"
            struct PsIn {
                @builtin(position) pos: vec4<f32>,
                @location(1) v1: vec4<f32>,
                @location(2) v2: vec4<f32>,
            };
        "#;
        let keep = BTreeSet::from([2u32]);
        let trimmed = trim_ps_inputs_to_locations(wgsl, &keep);
        assert!(!trimmed.contains("@location(1)"));
        assert!(trimmed.contains("@location(2)"));
    }

    #[test]
    fn trims_ps_inputs_to_empty_drops_struct_and_param() {
        let wgsl = r#"
            struct PsIn {
                @location(1) v1: vec4<f32>,
            };

            @fragment
            fn fs_main(input: PsIn) -> @location(0) vec4<f32> {
                return vec4<f32>(1.0);
            }
        "#;
        let keep = BTreeSet::new();
        let trimmed = trim_ps_inputs_to_locations(wgsl, &keep);
        assert!(!trimmed.contains("struct PsIn"));
        assert!(trimmed.contains("fn fs_main()"));
    }

    #[test]
    fn finds_ps_output_locations_from_struct_or_return() {
        let wgsl_struct = r#"
            struct PsOut {
                @location(0) t0: vec4<f32>,
                @location(2) t2: vec4<f32>,
            };
        "#;
        let locs = declared_ps_output_locations(wgsl_struct).unwrap();
        assert_eq!(locs.iter().copied().collect::<Vec<_>>(), vec![0, 2]);

        let wgsl_return = r#"
            @fragment
            fn fs_main() -> @location(0) vec4<f32> { return vec4<f32>(0.0); }
        "#;
        let locs = declared_ps_output_locations(wgsl_return).unwrap();
        assert_eq!(locs.iter().copied().collect::<Vec<_>>(), vec![0]);
    }

    #[test]
    fn trims_ps_outputs_struct_and_out_assignments() {
        let wgsl = r#"
            struct PsOut {
                @location(0) t0: vec4<f32>,
                @location(2) t2: vec4<f32>,
            };

            @fragment
            fn fs_main() -> PsOut {
                var out: PsOut;
                out.t0 = vec4<f32>(1.0);
                out.t2 = vec4<f32>(2.0);
                return out;
            }
        "#;
        let keep = BTreeSet::from([0u32]);
        let trimmed = trim_ps_outputs_to_locations(wgsl, &keep);
        assert!(trimmed.contains("@location(0)"));
        assert!(!trimmed.contains("@location(2)"));
        assert!(trimmed.contains("out.t0 ="));
        assert!(!trimmed.contains("out.t2 ="));
        assert!(trimmed.contains("let _aero_trim_tmp"));
    }

    #[test]
    fn trims_ps_outputs_to_empty_rewrites_entrypoint_to_void() {
        let wgsl = r#"
            struct PsOut {
                @location(2) t2: vec4<f32>,
            };

            @fragment
            fn fs_main() -> PsOut {
                var out: PsOut;
                out.t2 = vec4<f32>(2.0);
                return out;
            }
        "#;
        let keep = BTreeSet::new();
        let trimmed = trim_ps_outputs_to_locations(wgsl, &keep);
        assert!(!trimmed.contains("struct PsOut"));
        assert!(trimmed.contains("fn fs_main() {"));
        assert!(!trimmed.contains("var out: PsOut;"));
        assert!(!trimmed.contains("return out;"));
    }

    #[test]
    fn trims_ps_outputs_to_empty_preserves_early_returns() {
        // When trimming removes every output member, `PsOut` is dropped and `fs_main` is rewritten
        // to return `()`. Ensure we preserve `return out;` control flow as `return;` so early
        // returns remain correct.
        let wgsl = r#"
            struct PsOut {
                @location(0) t0: vec4<f32>,
            };

            @fragment
            fn fs_main() -> PsOut {
                var out: PsOut;
                if true {
                    return out;
                }
                out.t0 = vec4<f32>(1.0);
                return out;
            }
        "#;
        let keep = BTreeSet::new();
        let trimmed = trim_ps_outputs_to_locations(wgsl, &keep);
        assert!(!trimmed.contains("return out;"));
        assert!(
            trimmed.contains("return;"),
            "expected trimmed shader to contain `return;` to preserve early returns:\n{trimmed}"
        );
    }

    #[test]
    fn trims_ps_outputs_direct_return_rewrites_entrypoint_to_void() {
        // Direct-return pixel shaders (`fn fs_main() -> @location(N) ...`) don't have a `PsOut`
        // struct. If the location isn't kept, we must rewrite the entrypoint to return `()`.
        let wgsl = r#"
            @fragment
            fn fs_main() -> @location(1) vec4<f32> {
                return vec4<f32>(0.0, 1.0, 0.0, 1.0);
            }
        "#;
        let keep = BTreeSet::new();
        let trimmed = trim_ps_outputs_to_locations(wgsl, &keep);

        assert!(
            !trimmed.contains("@location(1)"),
            "expected return location attribute to be removed:\n{trimmed}"
        );
        assert!(
            trimmed.contains("fn fs_main() {"),
            "expected fs_main to be rewritten to return void:\n{trimmed}"
        );
        assert!(
            trimmed.contains("let _aero_trim_tmp"),
            "expected return expression to be preserved via a temp binding:\n{trimmed}"
        );
        assert!(
            trimmed.contains("return;"),
            "expected rewritten function to contain `return;`:\n{trimmed}"
        );
    }

    #[test]
    fn trim_ps_outputs_direct_return_is_noop_when_location_kept() {
        let wgsl = r#"
            @fragment
            fn fs_main() -> @location(1) vec4<f32> {
                return vec4<f32>(0.0, 1.0, 0.0, 1.0);
            }
        "#;
        let keep = BTreeSet::from([1u32]);
        let trimmed = trim_ps_outputs_to_locations(wgsl, &keep);
        assert_eq!(trimmed.trim(), wgsl.trim());
    }
}
