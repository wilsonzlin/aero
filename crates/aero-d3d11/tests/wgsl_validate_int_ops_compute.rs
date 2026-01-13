#[test]
fn wgsl_compute_integer_ops_bitcast_patterns_validate() {
    // This test is intentionally WGSL-only: it validates that the bitcast-based patterns we use
    // when lowering DXBC integer ops are accepted by naga for a compute entry point.
    //
    // The `aero-d3d11` SM4/SM5 translator models temporaries as `vec4<f32>` and treats them as an
    // untyped register file. Integer ALU ops are implemented by `bitcast`ing to `vec4<u32>` /
    // `vec4<i32>`, performing the integer operation, then `bitcast`ing back to `vec4<f32>` for
    // storage.
    let wgsl = r#"
struct OutBuf {
  data: array<u32>,
}

@group(0) @binding(0) var<storage, read_write> out_buf: OutBuf;

@compute @workgroup_size(1)
fn cs_main() {
  // Untyped temporaries (DXBC-style register file).
  var r0: vec4<f32> = bitcast<vec4<f32>>(vec4<u32>(1u, 2u, 3u, 4u));
  var r1: vec4<f32> = bitcast<vec4<f32>>(vec4<u32>(10u, 20u, 30u, 40u));

  // Integer add / mul (operate on i32 lanes, then store raw bits back to f32 regs).
  let iadd = bitcast<vec4<f32>>(bitcast<vec4<i32>>(r0) + bitcast<vec4<i32>>(r1));
  let imul = bitcast<vec4<f32>>(bitcast<vec4<i32>>(r0) * bitcast<vec4<i32>>(r1));

  // Bitwise ops (operate on u32 lanes).
  let vand = bitcast<vec4<f32>>(bitcast<vec4<u32>>(r0) & bitcast<vec4<u32>>(r1));
  let vor = bitcast<vec4<f32>>(bitcast<vec4<u32>>(r0) | bitcast<vec4<u32>>(r1));
  let vxor = bitcast<vec4<f32>>(bitcast<vec4<u32>>(r0) ^ bitcast<vec4<u32>>(r1));

  // Shifts (mask the shift amount like typical DXBC behaviour).
  let sh = bitcast<vec4<u32>>(r0) & vec4<u32>(31u);
  let ishl = bitcast<vec4<f32>>(bitcast<vec4<u32>>(r1) << sh);
  let ushr = bitcast<vec4<f32>>(bitcast<vec4<u32>>(r1) >> sh);
  let ishr = bitcast<vec4<f32>>(bitcast<vec4<i32>>(r1) >> sh);

  // Conversions:
  // - itof/utof: numeric int->float conversion, storing float values.
  let itof = vec4<f32>(bitcast<vec4<i32>>(r0));
  let utof = vec4<f32>(bitcast<vec4<u32>>(r0));
  // - ftoi/ftou: numeric float->int conversion, storing integer raw bits back into f32 regs.
  let ftoi = bitcast<vec4<f32>>(vec4<i32>(itof));
  let ftou = bitcast<vec4<f32>>(vec4<u32>(utof));

  // movc-style conditional select: `dst = cond != 0 ? a : b`.
  let cond = bitcast<vec4<u32>>(r0) != vec4<u32>(0u);
  let movc = select(r1, r0, cond);

  // Store a subset of results to keep everything live.
  out_buf.data[0] = bitcast<vec4<u32>>(iadd).x;
  out_buf.data[1] = bitcast<vec4<u32>>(imul).x;
  out_buf.data[2] = bitcast<vec4<u32>>(vand).x;
  out_buf.data[3] = bitcast<vec4<u32>>(vor).x;
  out_buf.data[4] = bitcast<vec4<u32>>(vxor).x;
  out_buf.data[5] = bitcast<vec4<u32>>(ishl).x;
  out_buf.data[6] = bitcast<vec4<u32>>(ushr).x;
  out_buf.data[7] = bitcast<vec4<u32>>(ishr).x;
  out_buf.data[8] = bitcast<u32>(itof.x);
  out_buf.data[9] = bitcast<vec4<u32>>(ftoi).x;
  out_buf.data[10] = bitcast<vec4<u32>>(ftou).x;
  out_buf.data[11] = bitcast<u32>(movc.x);
}
"#;

    let module = naga::front::wgsl::parse_str(wgsl).expect("WGSL failed to parse");
    let mut validator = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    );
    validator.validate(&module).expect("WGSL failed to validate");
}

