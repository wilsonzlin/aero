// Reuse the canonical ACPI PM snapshot tests from `aero-devices` so
// `cargo test -p devices` exercises the same behavior.
include!("../../devices/tests/acpi_pm_snapshot.rs");
