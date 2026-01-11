use std::path::PathBuf;

fn main() {
    let cfg = aero_acpi::AcpiConfig::default();
    let placement = aero_acpi::AcpiPlacement::default();
    let bytes = aero_acpi::AcpiTables::build(&cfg, placement).dsdt;

    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("acpi");
    path.push("dsdt.aml");

    std::fs::write(&path, bytes).expect("failed to write dsdt.aml");
    eprintln!("wrote {}", path.display());
}
