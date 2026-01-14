use aero_d3d9::sm3::decode::{SrcModifier, Swizzle, SwizzleComponent, WriteMask};
use aero_d3d9::sm3::ir::{
    Block, ConstDefF32, Dst, InstModifiers, IrOp, RegFile, RegRef, RelativeRef, ShaderIr, Src,
    Stmt,
};
use aero_d3d9::sm3::types::{ShaderStage, ShaderVersion};
use aero_d3d9::sm3::generate_wgsl;

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
