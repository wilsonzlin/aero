use std::path::PathBuf;

fn main() {
    let bytes = firmware::acpi::dsdt::generate_dsdt_aml();

    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("acpi");
    path.push("dsdt.aml");

    std::fs::write(&path, bytes).expect("failed to write dsdt.aml");
    eprintln!("wrote {}", path.display());
}

