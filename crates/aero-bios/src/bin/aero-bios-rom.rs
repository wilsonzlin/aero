use std::path::PathBuf;

fn main() {
    let out: PathBuf = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("assets/bios.bin"));

    let rom = aero_bios::build_bios_rom();

    if let Some(parent) = out.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).expect("create output directory");
        }
    }

    std::fs::write(&out, rom).expect("write BIOS ROM");
    eprintln!("Wrote {} bytes to {}", aero_bios::BIOS_SIZE, out.display());
}
