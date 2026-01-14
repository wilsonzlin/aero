use aero_machine::{Machine, MachineConfig};
use aero_snapshot::{inspect_snapshot, SectionId};
use pretty_assertions::assert_eq;
use std::io::Cursor;

#[test]
fn smp_snapshot_uses_cpus_and_mmus_sections() {
    let mut cfg = MachineConfig::default();
    cfg.ram_size_bytes = 2 * 1024 * 1024;
    cfg.cpu_count = 2;
    cfg.enable_pc_platform = true;

    let mut m = Machine::new(cfg.clone()).unwrap();
    let snap = m.take_snapshot_full().unwrap();

    let idx = inspect_snapshot(&mut Cursor::new(&snap)).unwrap();

    assert!(
        idx.sections.iter().any(|s| s.id == SectionId::CPUS),
        "expected CPUS section, got: {:?}",
        idx.sections
            .iter()
            .map(|s| s.id.name().unwrap_or("?"))
            .collect::<Vec<_>>()
    );
    assert!(
        idx.sections.iter().any(|s| s.id == SectionId::MMUS),
        "expected MMUS section, got: {:?}",
        idx.sections
            .iter()
            .map(|s| s.id.name().unwrap_or("?"))
            .collect::<Vec<_>>()
    );

    assert!(
        !idx.sections.iter().any(|s| s.id == SectionId::CPU),
        "snapshot unexpectedly used legacy CPU section"
    );
    assert!(
        !idx.sections.iter().any(|s| s.id == SectionId::MMU),
        "snapshot unexpectedly used legacy MMU section"
    );

    let cpus_section = idx
        .sections
        .iter()
        .find(|s| s.id == SectionId::CPUS)
        .unwrap();
    let mmus_section = idx
        .sections
        .iter()
        .find(|s| s.id == SectionId::MMUS)
        .unwrap();

    let cpus_off = usize::try_from(cpus_section.offset).unwrap();
    let mmus_off = usize::try_from(mmus_section.offset).unwrap();

    let cpus_count = u32::from_le_bytes(snap[cpus_off..cpus_off + 4].try_into().unwrap());
    let mmus_count = u32::from_le_bytes(snap[mmus_off..mmus_off + 4].try_into().unwrap());
    assert_eq!(cpus_count, u32::from(cfg.cpu_count));
    assert_eq!(mmus_count, u32::from(cfg.cpu_count));

    let mut restored = Machine::new(cfg).unwrap();
    restored.restore_snapshot_bytes(&snap).unwrap();
}

