use aero_cpu_core::{
    sse_state::SseState, state as core_state, state::CpuState as CanonicalCpuState,
};
use aero_jit_x86::abi as jit_abi;
use memoffset::offset_of;

fn assert_fits_u32(name: &str, value: usize) -> u32 {
    u32::try_from(value).unwrap_or_else(|_| panic!("{name}={value} does not fit in u32"))
}

#[test]
fn cpu_state_abi_matches_aero_cpu_core() {
    for i in 0..16 {
        assert_eq!(
            jit_abi::CPU_GPR_OFF[i],
            assert_fits_u32("CPU_GPR_OFF[i]", core_state::CPU_GPR_OFF[i]),
            "GPR[{i}] offset mismatch"
        );
        assert_eq!(
            jit_abi::CPU_XMM_OFF[i],
            assert_fits_u32("CPU_XMM_OFF[i]", core_state::CPU_XMM_OFF[i]),
            "XMM[{i}] offset mismatch"
        );
    }

    assert_eq!(
        jit_abi::CPU_RIP_OFF,
        assert_fits_u32("CPU_RIP_OFF", core_state::CPU_RIP_OFF)
    );
    assert_eq!(
        jit_abi::CPU_RFLAGS_OFF,
        assert_fits_u32("CPU_RFLAGS_OFF", core_state::CPU_RFLAGS_OFF)
    );
    assert_eq!(
        jit_abi::CPU_STATE_SIZE,
        assert_fits_u32("CPU_STATE_SIZE", core_state::CPU_STATE_SIZE)
    );
    assert_eq!(
        jit_abi::CPU_STATE_ALIGN,
        assert_fits_u32("CPU_STATE_ALIGN", core_state::CPU_STATE_ALIGN)
    );
}

#[test]
fn cpu_state_abi_satisfies_wasm_codegen_constraints() {
    let max = u32::MAX as usize;

    assert!(
        core_state::CPU_STATE_SIZE <= max,
        "CpuState size {} exceeds u32::MAX; wasm32 codegen cannot address it",
        core_state::CPU_STATE_SIZE
    );

    assert!(
        core_state::CPU_STATE_ALIGN <= max,
        "CpuState alignment {} exceeds u32::MAX",
        core_state::CPU_STATE_ALIGN
    );

    for (i, off) in core_state::CPU_GPR_OFF.iter().enumerate() {
        assert!(
            *off <= max,
            "CPU_GPR_OFF[{i}]={off} exceeds u32::MAX; wasm32 codegen cannot encode it"
        );
        assert_eq!(
            off % 8,
            0,
            "CPU_GPR_OFF[{i}]={off} is not 8-byte aligned (u64 GPR load/store)"
        );
    }

    for (i, off) in core_state::CPU_XMM_OFF.iter().enumerate() {
        assert!(
            *off <= max,
            "CPU_XMM_OFF[{i}]={off} exceeds u32::MAX; wasm32 codegen cannot encode it"
        );
        assert_eq!(
            off % 16,
            0,
            "CPU_XMM_OFF[{i}]={off} is not 16-byte aligned (u128 XMM load/store)"
        );
    }

    assert_eq!(
        core_state::CPU_RIP_OFF % 8,
        0,
        "CPU_RIP_OFF={} is not 8-byte aligned",
        core_state::CPU_RIP_OFF
    );
    assert_eq!(
        core_state::CPU_RFLAGS_OFF % 8,
        0,
        "CPU_RFLAGS_OFF={} is not 8-byte aligned",
        core_state::CPU_RFLAGS_OFF
    );
    assert_eq!(
        core_state::CPU_STATE_SIZE % core_state::CPU_STATE_ALIGN,
        0,
        "CPU_STATE_SIZE={} is not a multiple of CPU_STATE_ALIGN={}",
        core_state::CPU_STATE_SIZE,
        core_state::CPU_STATE_ALIGN
    );
}

#[test]
fn cpu_state_layout_matches_abi_constants() {
    assert_eq!(
        offset_of!(CanonicalCpuState, gpr) as u32,
        jit_abi::CPU_GPR_OFF[0]
    );
    assert_eq!(
        offset_of!(CanonicalCpuState, rip) as u32,
        jit_abi::CPU_RIP_OFF
    );
    assert_eq!(
        offset_of!(CanonicalCpuState, rflags) as u32,
        jit_abi::CPU_RFLAGS_OFF
    );

    for i in 0..16 {
        assert_eq!(
            jit_abi::gpr_offset(i),
            jit_abi::CPU_GPR_OFF[0] + (i as u32) * 8
        );
    }

    let xmm_base = offset_of!(CanonicalCpuState, sse) + offset_of!(SseState, xmm);
    for i in 0..16 {
        assert_eq!(
            jit_abi::CPU_XMM_OFF[i],
            (xmm_base + i * core::mem::size_of::<u128>()) as u32
        );
    }

    assert_eq!(
        core::mem::size_of::<CanonicalCpuState>() as u32,
        jit_abi::CPU_STATE_SIZE
    );
    assert_eq!(
        core::mem::align_of::<CanonicalCpuState>() as u32,
        jit_abi::CPU_STATE_ALIGN
    );
}
