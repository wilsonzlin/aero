use aero_snapshot::SectionId;

#[test]
fn section_ids_have_stable_names_and_numbers() {
    let cases = [
        (SectionId::META, 1u32, "META"),
        (SectionId::CPU, 2u32, "CPU"),
        (SectionId::MMU, 3u32, "MMU"),
        (SectionId::DEVICES, 4u32, "DEVICES"),
        (SectionId::DISKS, 5u32, "DISKS"),
        (SectionId::RAM, 6u32, "RAM"),
        (SectionId::CPUS, 7u32, "CPUS"),
    ];

    for (id, expected_num, expected_name) in cases {
        assert_eq!(
            id.0, expected_num,
            "{expected_name} SectionId number changed; must remain stable"
        );
        assert_eq!(id.name(), Some(expected_name));
        assert_eq!(format!("{id}"), format!("{expected_name}({expected_num})"));
    }
}
