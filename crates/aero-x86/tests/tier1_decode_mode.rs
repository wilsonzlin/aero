use aero_types::{Gpr, Width};
use aero_x86::tier1::{decode_one_mode, InstKind, Operand, Reg};

#[test]
fn decode_one_mode_32bit_dec_ecx_is_not_rex() {
    let inst = decode_one_mode(0x1000, &[0x49], 32);
    assert_eq!(inst.len, 1);
    assert_eq!(
        inst.kind,
        InstKind::Dec {
            dst: Operand::Reg(Reg {
                gpr: Gpr::Rcx,
                width: Width::W32,
                high8: false,
            }),
            width: Width::W32,
        }
    );
}

