use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    // `CARGO_MANIFEST_DIR` for integration tests in this package is `<repo>/crates/firmware`.
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn bios_rom_fixture_matches_generator() {
    let fixture_path = repo_root().join("assets").join("bios.bin");
    let fixture = std::fs::read(&fixture_path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", fixture_path.display()));

    let generated = firmware::bios::build_bios_rom();

    if fixture != generated {
        let min_len = fixture.len().min(generated.len());
        let first_diff = (0..min_len).find(|&i| fixture[i] != generated[i]);

        let details = match first_diff {
            Some(i) => format!(
                "first differing byte at 0x{i:04X}: fixture=0x{:02X}, generated=0x{:02X}",
                fixture[i], generated[i]
            ),
            None => format!(
                "length mismatch: fixture={} bytes, generated={} bytes",
                fixture.len(),
                generated.len()
            ),
        };

        panic!(
            "`assets/bios.bin` is out of date or has been modified ({details}).\n\
Regenerate with: cargo run -p firmware --bin gen_bios_rom"
        );
    }
}

