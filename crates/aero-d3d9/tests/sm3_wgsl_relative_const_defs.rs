use aero_d3d9::sm3::decode::{SrcModifier, Swizzle, SwizzleComponent, WriteMask};
use aero_d3d9::sm3::generate_wgsl;
use aero_d3d9::sm3::ir::{
    Block, ConstDefF32, Dst, InstModifiers, IrOp, RegFile, RegRef, RelativeRef, ShaderIr, Src, Stmt,
};
use aero_d3d9::sm3::types::{ShaderStage, ShaderVersion};

#[test]
fn relative_constant_def_overrides_do_not_blow_up_wgsl_size() {
    // Regression test: previously, relative constant indexing would expand to a nested `select(...)`
    // chain for each embedded `def` constant, leading to enormous WGSL output for shaders that
    // combined many defs with heavy relative addressing.
    let mut const_defs_f32 = Vec::new();
    for i in 0..256u32 {
        const_defs_f32.push(ConstDefF32 {
            index: i,
            value: [i as f32, 0.0, 0.0, 0.0],
        });
    }

    // mov oC0, c0[a0.x]
    let src = Src {
        reg: RegRef {
            file: RegFile::Const,
            index: 0,
            relative: Some(Box::new(RelativeRef {
                reg: Box::new(RegRef {
                    file: RegFile::Addr,
                    index: 0,
                    relative: None,
                }),
                component: SwizzleComponent::X,
            })),
        },
        swizzle: Swizzle::identity(),
        modifier: SrcModifier::None,
    };

    let dst = Dst {
        reg: RegRef {
            file: RegFile::ColorOut,
            index: 0,
            relative: None,
        },
        mask: WriteMask::all(),
    };

    let ir = ShaderIr {
        version: ShaderVersion {
            stage: ShaderStage::Pixel,
            major: 3,
            minor: 0,
        },
        inputs: Vec::new(),
        outputs: Vec::new(),
        samplers: Vec::new(),
        const_defs_f32,
        const_defs_i32: Vec::new(),
        const_defs_bool: Vec::new(),
        body: Block {
            stmts: vec![Stmt::Op(IrOp::Mov {
                dst,
                src,
                modifiers: InstModifiers::none(),
            })],
        },
        uses_semantic_locations: false,
    };

    // This should stay comfortably below a few hundred KiB even with 256 defs.
    let wgsl = generate_wgsl(&ir).unwrap();
    assert!(
        wgsl.wgsl.len() < 200_000,
        "WGSL unexpectedly large ({} bytes)\n{}",
        wgsl.wgsl.len(),
        wgsl.wgsl
    );
    // Sanity: ensure the helper was used.
    assert!(wgsl.wgsl.contains("fn aero_read_const"), "{}", wgsl.wgsl);
}

#[test]
fn lrp_uses_aero_read_const_for_relative_constants_when_defs_present() {
    // Ensure the `lrp` WGSL lowering threads `f32_defs` through `src_expr` so relative constant
    // indexing continues to route through `aero_read_const` (which applies embedded `def` overrides
    // without expanding into enormous per-access `select(...)` chains).
    let const_defs_f32 = vec![ConstDefF32 {
        index: 0,
        value: [1.0, 0.0, 0.0, 0.0],
    }];

    // lrp oC0, c0[a0.x], c1, c2
    let rel_const = Src {
        reg: RegRef {
            file: RegFile::Const,
            index: 0,
            relative: Some(Box::new(RelativeRef {
                reg: Box::new(RegRef {
                    file: RegFile::Addr,
                    index: 0,
                    relative: None,
                }),
                component: SwizzleComponent::X,
            })),
        },
        swizzle: Swizzle::identity(),
        modifier: SrcModifier::None,
    };
    let c1 = Src {
        reg: RegRef {
            file: RegFile::Const,
            index: 1,
            relative: None,
        },
        swizzle: Swizzle::identity(),
        modifier: SrcModifier::None,
    };
    let c2 = Src {
        reg: RegRef {
            file: RegFile::Const,
            index: 2,
            relative: None,
        },
        swizzle: Swizzle::identity(),
        modifier: SrcModifier::None,
    };

    let dst = Dst {
        reg: RegRef {
            file: RegFile::ColorOut,
            index: 0,
            relative: None,
        },
        mask: WriteMask::all(),
    };

    let ir = ShaderIr {
        version: ShaderVersion {
            stage: ShaderStage::Pixel,
            major: 3,
            minor: 0,
        },
        inputs: Vec::new(),
        outputs: Vec::new(),
        samplers: Vec::new(),
        const_defs_f32,
        const_defs_i32: Vec::new(),
        const_defs_bool: Vec::new(),
        body: Block {
            stmts: vec![Stmt::Op(IrOp::Lrp {
                dst,
                src0: rel_const,
                src1: c1,
                src2: c2,
                modifiers: InstModifiers::none(),
            })],
        },
        uses_semantic_locations: false,
    };

    let wgsl = generate_wgsl(&ir).unwrap();
    assert!(
        wgsl.wgsl.contains("mix("),
        "expected mix() in WGSL for lrp\n{}",
        wgsl.wgsl
    );
    assert!(
        wgsl.wgsl.contains("aero_read_const("),
        "expected aero_read_const() for relative constant indexing\n{}",
        wgsl.wgsl
    );
}
