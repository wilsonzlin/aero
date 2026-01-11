use crate::shared_surface::{SharedSurfaceError, SharedSurfaceTable};

#[test]
fn shared_surface_export_import_resolves_alias_and_refcounts() {
    let mut table = SharedSurfaceTable::default();
    let original = 0x10u32;
    let alias = 0x20u32;
    let token = 0x1122_3344_5566_7788u64;

    table.register_handle(original);
    table.export(original, token).unwrap();
    table.import(alias, token).unwrap();

    assert_eq!(table.resolve_handle(alias), original);
    assert_eq!(table.resolve_handle(original), original);

    assert_eq!(table.destroy_handle(alias), Some((original, false)));
    assert_eq!(table.destroy_handle(original), Some((original, true)));

    assert!(matches!(
        table.import(0x30, token),
        Err(SharedSurfaceError::UnknownToken(_))
    ));
}

#[test]
fn shared_surface_export_is_idempotent_but_retarget_is_rejected() {
    let mut table = SharedSurfaceTable::default();
    let token = 0xAAu64;

    table.register_handle(1);
    table.register_handle(2);

    table.export(1, token).unwrap();
    table.export(1, token).unwrap();

    assert!(matches!(
        table.export(2, token),
        Err(SharedSurfaceError::TokenAlreadyExported { .. })
    ));
}

#[test]
fn shared_surface_import_is_idempotent_but_alias_rebind_is_rejected() {
    let mut table = SharedSurfaceTable::default();
    let token_a = 0xA0u64;
    let token_b = 0xB0u64;

    table.register_handle(1);
    table.register_handle(3);

    table.export(1, token_a).unwrap();
    table.export(3, token_b).unwrap();

    table.import(2, token_a).unwrap();
    table.import(2, token_a).unwrap();

    assert!(matches!(
        table.import(2, token_b),
        Err(SharedSurfaceError::AliasAlreadyBound { .. })
    ));

    // Idempotent import must not double-increment refcounts.
    assert_eq!(table.destroy_handle(2), Some((1, false)));
    assert_eq!(table.destroy_handle(1), Some((1, true)));
}

