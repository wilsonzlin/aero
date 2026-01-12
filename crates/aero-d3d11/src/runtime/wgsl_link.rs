use std::collections::BTreeSet;

use anyhow::Result;

fn parse_location_attr(line: &str) -> Option<u32> {
    let idx = line.find("@location(")?;
    let rest = &line[idx + "@location(".len()..];
    let end = rest.find(')')?;
    rest[..end].trim().parse().ok()
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
            if (b'0'..=b'9').contains(&b) {
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
}
