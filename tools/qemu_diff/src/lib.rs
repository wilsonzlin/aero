use std::io;
use std::path::PathBuf;
use std::process::Command;
use std::process::Stdio;

const BOOT_SECTOR: &[u8] = include_bytes!("../boot/boot.bin");

const PAYLOAD_SECTORS: usize = 4;
const PAYLOAD_SIZE: usize = 512 * PAYLOAD_SECTORS;

const OFF_AX: usize = 0;
const OFF_BX: usize = 2;
const OFF_CX: usize = 4;
const OFF_DX: usize = 6;
const OFF_SI: usize = 8;
const OFF_DI: usize = 10;
const OFF_BP: usize = 12;
const OFF_SP: usize = 14;
const OFF_FLAGS: usize = 16;
const OFF_DS: usize = 18;
const OFF_ES: usize = 20;
const OFF_SS: usize = 22;
const OFF_CODE_LEN: usize = 24;
const OFF_MEM_INIT: usize = 28;
const MEM_INIT_LEN: usize = 256;
const OFF_CODE: usize = OFF_MEM_INIT + MEM_INIT_LEN;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QemuOutcome {
    pub ax: u16,
    pub bx: u16,
    pub cx: u16,
    pub dx: u16,
    pub si: u16,
    pub di: u16,
    pub bp: u16,
    pub sp: u16,
    pub flags: u16,
    pub mem_hash: u32,
}

impl QemuOutcome {
    pub fn to_json(&self) -> String {
        // Keep dependencies minimal (no serde) for CI environments without crates.io access.
        format!(
            "{{\"ax\":{},\"bx\":{},\"cx\":{},\"dx\":{},\"si\":{},\"di\":{},\"bp\":{},\"sp\":{},\"flags\":{},\"mem_hash\":{}}}",
            self.ax,
            self.bx,
            self.cx,
            self.dx,
            self.si,
            self.di,
            self.bp,
            self.sp,
            self.flags,
            self.mem_hash,
        )
    }
}

#[derive(Debug, Clone)]
pub struct TestCase {
    pub ax: u16,
    pub bx: u16,
    pub cx: u16,
    pub dx: u16,
    pub si: u16,
    pub di: u16,
    pub bp: u16,
    pub sp: u16,
    pub flags: u16,
    pub ds: u16,
    pub es: u16,
    pub ss: u16,
    pub mem_init: [u8; MEM_INIT_LEN],
    pub code: Vec<u8>,
}

pub fn qemu_available() -> bool {
    find_qemu_binary().is_some()
}

fn find_qemu_binary() -> Option<&'static str> {
    // Prefer qemu-system-i386, but fall back to qemu-system-x86_64 (also supports real mode).
    if Command::new("qemu-system-i386")
        .arg("--version")
        .output()
        .is_ok()
    {
        return Some("qemu-system-i386");
    }

    if Command::new("qemu-system-x86_64")
        .arg("--version")
        .output()
        .is_ok()
    {
        return Some("qemu-system-x86_64");
    }

    None
}

pub fn run(case: &TestCase) -> io::Result<QemuOutcome> {
    let qemu = find_qemu_binary()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "qemu-system-* not found"))?;

    let img_bytes = build_disk_image(case)?;
    let img_path = write_temp_file("aero-qemu-diff", "img", &img_bytes)?;

    let output = Command::new(qemu)
        // When using `-chardev stdio`, QEMU may block forever if stdin is left open.
        // Closing stdin ensures the stdio chardev thread can terminate and QEMU exits cleanly.
        .stdin(Stdio::null())
        .arg("-display")
        .arg("none")
        .arg("-machine")
        .arg("pc")
        .arg("-m")
        .arg("16")
        .arg("-drive")
        .arg(format!("if=floppy,format=raw,file={}", img_path.display()))
        .arg("-boot")
        .arg("order=a")
        .arg("-monitor")
        .arg("none")
        .arg("-serial")
        .arg("none")
        .arg("-parallel")
        .arg("none")
        .arg("-device")
        .arg("isa-debugcon,iobase=0xe9,chardev=debugcon")
        .arg("-chardev")
        .arg("stdio,id=debugcon,signal=off")
        .arg("-device")
        .arg("isa-debug-exit,iobase=0xf4,iosize=0x04")
        .arg("-no-reboot")
        .output();

    // Best-effort cleanup.
    let _ = std::fs::remove_file(&img_path);

    let output = output?;
    let mut combined = String::new();
    combined.push_str(&String::from_utf8_lossy(&output.stdout));
    combined.push_str(&String::from_utf8_lossy(&output.stderr));

    parse_output(&combined).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

fn build_disk_image(case: &TestCase) -> io::Result<Vec<u8>> {
    if BOOT_SECTOR.len() != 512 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("boot sector size is {}, expected 512", BOOT_SECTOR.len()),
        ));
    }

    let max_code_len = PAYLOAD_SIZE - OFF_CODE;
    if case.code.len() > max_code_len {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("code too large: {} > {}", case.code.len(), max_code_len),
        ));
    }

    let mut img = vec![0u8; 512 + PAYLOAD_SIZE];
    img[..512].copy_from_slice(BOOT_SECTOR);

    let mut payload = vec![0u8; PAYLOAD_SIZE];
    write_u16(&mut payload, OFF_AX, case.ax);
    write_u16(&mut payload, OFF_BX, case.bx);
    write_u16(&mut payload, OFF_CX, case.cx);
    write_u16(&mut payload, OFF_DX, case.dx);
    write_u16(&mut payload, OFF_SI, case.si);
    write_u16(&mut payload, OFF_DI, case.di);
    write_u16(&mut payload, OFF_BP, case.bp);
    write_u16(&mut payload, OFF_SP, case.sp);
    write_u16(&mut payload, OFF_FLAGS, case.flags | 0x2);
    write_u16(&mut payload, OFF_DS, case.ds);
    write_u16(&mut payload, OFF_ES, case.es);
    write_u16(&mut payload, OFF_SS, case.ss);

    write_u16(&mut payload, OFF_CODE_LEN, case.code.len() as u16);
    payload[OFF_MEM_INIT..OFF_MEM_INIT + MEM_INIT_LEN].copy_from_slice(&case.mem_init);
    payload[OFF_CODE..OFF_CODE + case.code.len()].copy_from_slice(&case.code);

    img[512..].copy_from_slice(&payload);
    Ok(img)
}

fn write_u16(buf: &mut [u8], off: usize, val: u16) {
    buf[off] = (val & 0xFF) as u8;
    buf[off + 1] = (val >> 8) as u8;
}

fn write_temp_file(prefix: &str, ext: &str, bytes: &[u8]) -> io::Result<PathBuf> {
    let mut path = std::env::temp_dir();
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    path.push(format!("{prefix}-{pid}-{nanos}.{ext}"));

    std::fs::write(&path, bytes)?;
    Ok(path)
}

fn parse_output(out: &str) -> Result<QemuOutcome, String> {
    let marker = "AERODIFF";
    let line = out
        .lines()
        .find(|l| l.trim_start().starts_with(marker))
        .ok_or_else(|| format!("missing '{marker}' marker in QEMU output:\n{out}"))?;

    let mut ax = None;
    let mut bx = None;
    let mut cx = None;
    let mut dx = None;
    let mut si = None;
    let mut di = None;
    let mut bp = None;
    let mut sp = None;
    let mut flags = None;
    let mut mem_hash = None;

    for token in line.split_whitespace().skip(1) {
        let Some((k, v)) = token.split_once('=') else {
            continue;
        };
        match k {
            "AX" => ax = Some(parse_hex_u16(v)?),
            "BX" => bx = Some(parse_hex_u16(v)?),
            "CX" => cx = Some(parse_hex_u16(v)?),
            "DX" => dx = Some(parse_hex_u16(v)?),
            "SI" => si = Some(parse_hex_u16(v)?),
            "DI" => di = Some(parse_hex_u16(v)?),
            "BP" => bp = Some(parse_hex_u16(v)?),
            "SP" => sp = Some(parse_hex_u16(v)?),
            "FL" => flags = Some(parse_hex_u16(v)?),
            "MH" => mem_hash = Some(parse_hex_u32(v)?),
            _ => {}
        }
    }

    Ok(QemuOutcome {
        ax: ax.ok_or("missing AX")?,
        bx: bx.ok_or("missing BX")?,
        cx: cx.ok_or("missing CX")?,
        dx: dx.ok_or("missing DX")?,
        si: si.ok_or("missing SI")?,
        di: di.ok_or("missing DI")?,
        bp: bp.ok_or("missing BP")?,
        sp: sp.ok_or("missing SP")?,
        flags: flags.ok_or("missing FL")?,
        mem_hash: mem_hash.ok_or("missing MH")?,
    })
}

fn parse_hex_u16(s: &str) -> Result<u16, String> {
    let trimmed = s.trim_start_matches("0x");
    u16::from_str_radix(trimmed, 16).map_err(|e| format!("invalid u16 hex '{s}': {e}"))
}

fn parse_hex_u32(s: &str) -> Result<u32, String> {
    let trimmed = s.trim_start_matches("0x");
    u32::from_str_radix(trimmed, 16).map_err(|e| format!("invalid u32 hex '{s}': {e}"))
}
