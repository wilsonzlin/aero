# 04 - Graphics Subsystem (DirectX → WebGPU)

## Overview

The graphics subsystem is one of the most challenging components. Windows 7 uses DirectX 9, 10, and 11 for rendering, plus the legacy VGA/SVGA stack for boot. We must translate all of these to WebGPU.

---

## Graphics Stack Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                    Windows 7 Graphics Stack                      │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│  Applications                                                    │
│       │                                                          │
│       ▼                                                          │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │ Direct3D 9/10/11  │    GDI/GDI+    │    DirectDraw     │    │
│  └─────────────────────────────────────────────────────────┘    │
│       │                     │                  │                 │
│       ▼                     ▼                  ▼                 │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │              WDDM Driver (User Mode)                     │    │
│  │    - D3D Runtime                                         │    │
│  │    - User Mode Driver (UMD)                              │    │
│  └─────────────────────────────────────────────────────────┘    │
│       │                                                          │
│       │ IOCTL                                                    │
│       ▼                                                          │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │              WDDM Driver (Kernel Mode)                   │    │
│  │    - DXGKernel (dxgkrnl.sys)                            │    │
│  │    - Kernel Mode Driver (KMD)                            │    │
│  │    - Video Memory Manager                                │    │
│  └─────────────────────────────────────────────────────────┘    │
│       │                                                          │
│       │ Hardware Commands                                        │
│       ▼                                                          │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │                    GPU Hardware                          │    │
│  └─────────────────────────────────────────────────────────┘    │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
                              │
                    ┌─────────┴─────────┐
                    │    EMULATION      │
                    │    BOUNDARY       │
                    └─────────┬─────────┘
                              │
┌─────────────────────────────────────────────────────────────────┐
│                    Aero Graphics Emulation                       │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │              Virtual GPU Device                          │    │
│  │    - MMIO Register Emulation                            │    │
│  │    - Command Buffer Processing                           │    │
│  │    - Video Memory Management                             │    │
│  └─────────────────────────────────────────────────────────┘    │
│       │                                                          │
│       │ Translated Commands                                      │
│       ▼                                                          │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │              DirectX → WebGPU Translator                 │    │
│  │    - Shader Translation (HLSL → WGSL)                   │    │
│  │    - State Translation                                   │    │
│  │    - Resource Management                                 │    │
│  └─────────────────────────────────────────────────────────┘    │
│       │                                                          │
│       │ WebGPU Commands                                          │
│       ▼                                                          │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │                    WebGPU API                            │    │
│  │    - GPUDevice                                           │    │
│  │    - GPUQueue                                            │    │
│  │    - GPURenderPipeline                                   │    │
│  └─────────────────────────────────────────────────────────┘    │
│       │                                                          │
│       ▼                                                          │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │              Browser GPU Acceleration                    │    │
│  └─────────────────────────────────────────────────────────┘    │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

---

## Emulation Approaches

### Approach 1: VGA/SVGA Emulation (Boot & Legacy)

For BIOS, boot loader, and legacy applications:

For Windows 7 specifically, the **primary boot display** must be provided by the same virtual GPU that will later run WDDM. In Aero, this means the AeroGPU virtual PCI device must be **VGA/VBE-compatible** (legacy VGA ports + legacy VRAM window + VBE linear framebuffer modes) and the emulator must present that framebuffer on the canvas until the WDDM driver claims scanout.

See: [AeroGPU Legacy VGA/VBE Compatibility](./16-aerogpu-vga-vesa-compat.md)

```rust
pub struct VgaEmulator {
    // VGA registers
    sequencer: [u8; 5],      // Sequencer registers
    graphics: [u8; 9],       // Graphics controller
    attribute: [u8; 21],     // Attribute controller
    crtc: [u8; 25],          // CRT controller
    
    // Memory
    vram: [u8; 256 * 1024],  // 256KB VGA memory
    
    // State
    mode: VgaMode,
    plane_mask: u8,
    read_map: u8,
    
    // Framebuffer output
    framebuffer: Vec<u32>,   // RGBA output
    width: u32,
    height: u32,
}

impl VgaEmulator {
    pub fn write_port(&mut self, port: u16, value: u8) {
        match port {
            0x3C0 => self.write_attribute(value),
            0x3C4 => self.sequencer_index = value,
            0x3C5 => self.sequencer[self.sequencer_index as usize] = value,
            0x3CE => self.graphics_index = value,
            0x3CF => self.graphics[self.graphics_index as usize] = value,
            0x3D4 => self.crtc_index = value,
            0x3D5 => self.crtc[self.crtc_index as usize] = value,
            _ => {}
        }
        self.update_mode();
    }
    
    pub fn write_vram(&mut self, addr: u32, value: u8) {
        let offset = (addr - 0xA0000) as usize;
        
        match self.mode {
            VgaMode::Text => {
                // Text mode: character + attribute
                self.vram[offset] = value;
            }
            VgaMode::Planar => {
                // Planar mode: write to planes based on mask
                for plane in 0..4 {
                    if self.plane_mask & (1 << plane) != 0 {
                        let plane_offset = offset + plane * 0x10000;
                        self.vram[plane_offset] = value;
                    }
                }
            }
            VgaMode::Linear => {
                // Linear/SVGA mode
                self.vram[offset] = value;
            }
        }
    }
    
    pub fn render_to_framebuffer(&mut self) {
        match self.mode {
            VgaMode::Text => self.render_text_mode(),
            VgaMode::Mode13h => self.render_mode_13h(),
            VgaMode::Svga => self.render_svga(),
            _ => {}
        }
    }
    
    fn render_text_mode(&mut self) {
        let cols = 80;
        let rows = 25;
        let char_width = 9;
        let char_height = 16;
        
        self.framebuffer.resize((cols * char_width * rows * char_height) as usize, 0);
        
        for row in 0..rows {
            for col in 0..cols {
                let offset = (row * cols + col) * 2;
                let char_code = self.vram[offset as usize];
                let attribute = self.vram[offset as usize + 1];
                
                let fg_color = self.palette[(attribute & 0x0F) as usize];
                let bg_color = self.palette[((attribute >> 4) & 0x0F) as usize];
                
                self.draw_character(col, row, char_code, fg_color, bg_color);
            }
        }
    }
}
```

### Approach 2: GPU Command Interception

For DirectX, we intercept GPU commands from the virtual WDDM driver:

> The concrete Win7/WDDM AeroGPU PCI/MMIO device model and the versioned ring/command ABI are
> defined by the canonical AeroGPU protocol headers:
>
> - [`drivers/aerogpu/protocol/README.md`](../drivers/aerogpu/protocol/README.md)
>   (`aerogpu_pci.h`, `aerogpu_ring.h`, `aerogpu_cmd.h`)
> - [`emulator/protocol`](../emulator/protocol) (Rust/TypeScript mirror)
>
> See also [`graphics/aerogpu-protocols.md`](./graphics/aerogpu-protocols.md) for an overview of
> similarly named in-tree protocols.

```rust
pub struct GpuCommandProcessor {
    command_ring: RingBuffer,
    context: GpuContext,
    translator: DxToWebGpuTranslator,
}

#[repr(C)]
pub struct GpuCommand {
    opcode: u32,
    size: u32,
    data: [u32; 62],  // Variable-length payload
}

impl GpuCommandProcessor {
    pub fn process_commands(&mut self, webgpu: &WebGpuDevice) {
        while let Some(cmd) = self.command_ring.dequeue() {
            match cmd.opcode {
                CMD_SET_RENDER_TARGET => {
                    let rt_addr = cmd.data[0] as u64 | ((cmd.data[1] as u64) << 32);
                    self.context.render_target = rt_addr;
                }
                CMD_SET_VIEWPORT => {
                    let viewport = Viewport {
                        x: f32::from_bits(cmd.data[0]),
                        y: f32::from_bits(cmd.data[1]),
                        width: f32::from_bits(cmd.data[2]),
                        height: f32::from_bits(cmd.data[3]),
                        min_depth: f32::from_bits(cmd.data[4]),
                        max_depth: f32::from_bits(cmd.data[5]),
                    };
                    self.context.viewport = viewport;
                }
                CMD_SET_SHADER => {
                    let shader_addr = cmd.data[0] as u64 | ((cmd.data[1] as u64) << 32);
                    let shader_type = ShaderType::from(cmd.data[2]);
                    self.load_shader(shader_addr, shader_type);
                }
                CMD_DRAW => {
                    let vertex_count = cmd.data[0];
                    let start_vertex = cmd.data[1];
                    self.draw(webgpu, vertex_count, start_vertex);
                }
                CMD_DRAW_INDEXED => {
                    let index_count = cmd.data[0];
                    let start_index = cmd.data[1];
                    let base_vertex = cmd.data[2] as i32;
                    self.draw_indexed(webgpu, index_count, start_index, base_vertex);
                }
                CMD_PRESENT => {
                    self.present(webgpu);
                }
                _ => {
                    log::warn!("Unknown GPU command: 0x{:08x}", cmd.opcode);
                }
            }
        }
    }
}
```

---

## Shader Translation (DXBC → WGSL)

This section sketches the DXBC parsing and WGSL generation pipeline at a high level.
For the D3D10/11-specific details (SM4/SM5 resource binding, constant buffers, SRV/UAV/RTV/DSV, input layouts, pipeline-state caching, and shader-stage coverage), see:

- [16 - Direct3D 10/11 Translation (SM4/SM5 → WebGPU)](./16-d3d10-11-translation.md)

### Translation Pipeline

```
┌─────────────┐    ┌─────────────┐    ┌─────────────┐    ┌─────────────┐
│ HLSL Binary │───▶│  Disassemble│───▶│  IR Build   │───▶│ WGSL Output │
│  (DXBC)     │    │   (parse)   │    │  (analyze)  │    │  (generate) │
└─────────────┘    └─────────────┘    └─────────────┘    └─────────────┘
```

### DXBC Parser

```rust
pub struct DxbcParser {
    // DXBC bytecode format
}

pub struct DxbcShader {
    version: ShaderVersion,
    shader_type: ShaderType,
    instructions: Vec<DxbcInstruction>,
    inputs: Vec<ShaderInput>,
    outputs: Vec<ShaderOutput>,
    constants: Vec<ConstantBuffer>,
    samplers: Vec<SamplerBinding>,
    textures: Vec<TextureBinding>,
}

impl DxbcParser {
    pub fn parse(&self, bytecode: &[u8]) -> Result<DxbcShader, ParseError> {
        let mut cursor = Cursor::new(bytecode);
        
        // Parse DXBC header
        let magic = cursor.read_u32_le()?;
        assert_eq!(magic, 0x43425844);  // "DXBC"
        
        let checksum = cursor.read_bytes(16)?;
        let one = cursor.read_u32_le()?;
        let total_size = cursor.read_u32_le()?;
        let chunk_count = cursor.read_u32_le()?;
        
        // Parse chunk offsets
        let chunk_offsets: Vec<u32> = (0..chunk_count)
            .map(|_| cursor.read_u32_le())
            .collect::<Result<_, _>>()?;
        
        let mut shader = DxbcShader::default();
        
        for offset in chunk_offsets {
            cursor.seek(SeekFrom::Start(offset as u64))?;
            
            let chunk_type = cursor.read_u32_le()?;
            let chunk_size = cursor.read_u32_le()?;
            
            match chunk_type {
                CHUNK_SHDR | CHUNK_SHEX => {
                    // Shader bytecode
                    shader.instructions = self.parse_shader_bytecode(&mut cursor)?;
                }
                // Note: some toolchains emit signature chunk variants with a
                // trailing `1` (`ISG1`/`OSG1`/`PSG1`). Treat these as equivalent
                // signature chunks.
                CHUNK_ISGN | CHUNK_ISG1 => {
                    // Input signature
                    shader.inputs = self.parse_signature(&mut cursor)?;
                }
                CHUNK_OSGN | CHUNK_OSG1 => {
                    // Output signature
                    shader.outputs = self.parse_signature(&mut cursor)?;
                }
                CHUNK_RDEF => {
                    // Resource definitions
                    self.parse_resources(&mut cursor, &mut shader)?;
                }
                _ => {}
            }
        }
        
        Ok(shader)
    }
}
```

### IR (Intermediate Representation)

```rust
pub enum IrOp {
    // Arithmetic
    Add { dst: IrReg, src0: IrReg, src1: IrReg },
    Sub { dst: IrReg, src0: IrReg, src1: IrReg },
    Mul { dst: IrReg, src0: IrReg, src1: IrReg },
    Div { dst: IrReg, src0: IrReg, src1: IrReg },
    Mad { dst: IrReg, src0: IrReg, src1: IrReg, src2: IrReg },  // multiply-add
    
    // Vector ops
    Dot3 { dst: IrReg, src0: IrReg, src1: IrReg },
    Dot4 { dst: IrReg, src0: IrReg, src1: IrReg },
    Cross { dst: IrReg, src0: IrReg, src1: IrReg },
    Normalize { dst: IrReg, src: IrReg },
    
    // Comparison
    Lt { dst: IrReg, src0: IrReg, src1: IrReg },
    Ge { dst: IrReg, src0: IrReg, src1: IrReg },
    Eq { dst: IrReg, src0: IrReg, src1: IrReg },
    
    // Flow control
    If { condition: IrReg },
    Else,
    EndIf,
    Loop,
    EndLoop,
    Break,
    Continue,
    
    // Memory
    Load { dst: IrReg, buffer: u32, offset: IrReg },
    Store { buffer: u32, offset: IrReg, src: IrReg },
    
    // Texture
    Sample { dst: IrReg, texture: u32, sampler: u32, coords: IrReg },
    SampleLevel { dst: IrReg, texture: u32, sampler: u32, coords: IrReg, lod: IrReg },
    SampleGrad { dst: IrReg, texture: u32, sampler: u32, coords: IrReg, ddx: IrReg, ddy: IrReg },
}

pub struct ShaderIr {
    shader_type: ShaderType,
    inputs: Vec<IrInput>,
    outputs: Vec<IrOutput>,
    uniforms: Vec<IrUniform>,
    instructions: Vec<IrOp>,
}
```

### WGSL Code Generator

```rust
pub struct WgslGenerator {
    output: String,
    indent: usize,
}

impl WgslGenerator {
    pub fn generate(&mut self, ir: &ShaderIr) -> String {
        self.output.clear();
        
        // Generate struct for inputs
        self.generate_input_struct(ir);
        
        // Generate struct for outputs
        self.generate_output_struct(ir);
        
        // Generate uniform bindings
        self.generate_uniforms(ir);
        
        // Generate texture/sampler bindings
        self.generate_bindings(ir);
        
        // Generate main function
        self.generate_main_function(ir);
        
        self.output.clone()
    }
    
    fn generate_main_function(&mut self, ir: &ShaderIr) {
        let (entry_point, input_type, output_type) = match ir.shader_type {
            ShaderType::Vertex => ("vs_main", "VertexInput", "VertexOutput"),
            ShaderType::Pixel => ("fs_main", "FragmentInput", "FragmentOutput"),
            ShaderType::Geometry => ("gs_main", "GeometryInput", "GeometryOutput"), // lowered/emulated on WebGPU
            ShaderType::Hull => ("hs_main", "HullInput", "HullOutput"),             // lowered/emulated on WebGPU
            ShaderType::Domain => ("ds_main", "DomainInput", "DomainOutput"),       // lowered/emulated on WebGPU
            ShaderType::Compute => ("cs_main", "ComputeInput", "void"),
        };
        
        self.emit(&format!(
            "@{} fn {}(input: {}) -> {} {{\n",
            self.shader_stage_attribute(ir.shader_type),
            entry_point,
            input_type,
            output_type
        ));
        
        self.indent += 1;
        
        // Declare temporary registers
        self.emit("var r0: vec4<f32>;\n");
        self.emit("var r1: vec4<f32>;\n");
        // ... more as needed
        
        // Generate instructions
        for inst in &ir.instructions {
            self.generate_instruction(inst);
        }
        
        self.indent -= 1;
        self.emit("}\n");
    }
    
    fn generate_instruction(&mut self, inst: &IrOp) {
        match inst {
            IrOp::Add { dst, src0, src1 } => {
                self.emit(&format!(
                    "{} = {} + {};\n",
                    self.reg_name(dst),
                    self.reg_name(src0),
                    self.reg_name(src1)
                ));
            }
            IrOp::Mul { dst, src0, src1 } => {
                self.emit(&format!(
                    "{} = {} * {};\n",
                    self.reg_name(dst),
                    self.reg_name(src0),
                    self.reg_name(src1)
                ));
            }
            IrOp::Mad { dst, src0, src1, src2 } => {
                self.emit(&format!(
                    "{} = fma({}, {}, {});\n",
                    self.reg_name(dst),
                    self.reg_name(src0),
                    self.reg_name(src1),
                    self.reg_name(src2)
                ));
            }
            IrOp::Sample { dst, texture, sampler, coords } => {
                self.emit(&format!(
                    "{} = textureSample(tex{}, samp{}, {}.xy);\n",
                    self.reg_name(dst),
                    texture,
                    sampler,
                    self.reg_name(coords)
                ));
            }
            IrOp::If { condition } => {
                self.emit(&format!("if ({}.x != 0.0) {{\n", self.reg_name(condition)));
                self.indent += 1;
            }
            IrOp::Else => {
                self.indent -= 1;
                self.emit("} else {\n");
                self.indent += 1;
            }
            IrOp::EndIf => {
                self.indent -= 1;
                self.emit("}\n");
            }
            // ... more operations
        }
    }
}
```

---

## DirectX State Translation

### Render State Mapping

The table below illustrates a D3D9-style “render state” mapping.
D3D10/11 replaces most of these knobs with explicit state objects (blend/depth-stencil/rasterizer/sampler) and view-based binding; see:

- [16 - Direct3D 10/11 Translation (SM4/SM5 → WebGPU)](./16-d3d10-11-translation.md)

| DirectX State | WebGPU Equivalent |
|---------------|-------------------|
| D3DRS_CULLMODE | GPURenderPipelineDescriptor.primitive.cullMode |
| D3DRS_FILLMODE | GPURenderPipelineDescriptor.primitive.topology |
| D3DRS_ZENABLE | GPUDepthStencilState.depthCompare |
| D3DRS_ZWRITEENABLE | GPUDepthStencilState.depthWriteEnabled |
| D3DRS_ALPHABLENDENABLE | GPUColorTargetState.blend |
| D3DRS_SRCBLEND | GPUBlendState.srcFactor |
| D3DRS_DESTBLEND | GPUBlendState.dstFactor |

### Pipeline State Object

```rust
pub struct DxToWebGpuState {
    // Cached pipeline states
    pipeline_cache: HashMap<PipelineKey, GPURenderPipeline>,
    
    // Current state
    vertex_shader: Option<WgslShader>,
    pixel_shader: Option<WgslShader>,
    blend_state: BlendState,
    depth_stencil_state: DepthStencilState,
    rasterizer_state: RasterizerState,
}

impl DxToWebGpuState {
    pub fn get_or_create_pipeline(&mut self, device: &GPUDevice) -> &GPURenderPipeline {
        let key = self.compute_pipeline_key();
        
        if !self.pipeline_cache.contains_key(&key) {
            let pipeline = self.create_pipeline(device);
            self.pipeline_cache.insert(key.clone(), pipeline);
        }
        
        self.pipeline_cache.get(&key).unwrap()
    }
    
    fn create_pipeline(&self, device: &GPUDevice) -> GPURenderPipeline {
        let vertex_module = device.create_shader_module(&GPUShaderModuleDescriptor {
            code: &self.vertex_shader.as_ref().unwrap().wgsl_code,
        });
        
        let fragment_module = device.create_shader_module(&GPUShaderModuleDescriptor {
            code: &self.pixel_shader.as_ref().unwrap().wgsl_code,
        });
        
        device.create_render_pipeline(&GPURenderPipelineDescriptor {
            layout: "auto",
            vertex: GPUVertexState {
                module: &vertex_module,
                entry_point: "vs_main",
                buffers: &self.vertex_buffer_layouts,
            },
            fragment: Some(GPUFragmentState {
                module: &fragment_module,
                entry_point: "fs_main",
                targets: &[GPUColorTargetState {
                    format: self.render_target_format,
                    blend: Some(self.blend_state.to_webgpu()),
                    write_mask: GPUColorWriteFlags::ALL,
                }],
            }),
            primitive: GPUPrimitiveState {
                topology: self.primitive_topology,
                cull_mode: self.rasterizer_state.cull_mode,
                front_face: self.rasterizer_state.front_face,
            },
            depth_stencil: self.depth_stencil_state.to_webgpu(),
            multisample: GPUMultisampleState::default(),
        })
    }
}
```

---

## Texture Management

### Texture Format Translation

```rust
pub fn dx_to_webgpu_format(dx_format: DXGI_FORMAT) -> GPUTextureFormat {
    match dx_format {
        DXGI_FORMAT_R8G8B8A8_UNORM => GPUTextureFormat::RGBA8Unorm,
        DXGI_FORMAT_R8G8B8A8_UNORM_SRGB => GPUTextureFormat::RGBA8UnormSrgb,
        DXGI_FORMAT_B8G8R8A8_UNORM => GPUTextureFormat::BGRA8Unorm,
        DXGI_FORMAT_R16G16B16A16_FLOAT => GPUTextureFormat::RGBA16Float,
        DXGI_FORMAT_R32G32B32A32_FLOAT => GPUTextureFormat::RGBA32Float,
        DXGI_FORMAT_R32_FLOAT => GPUTextureFormat::R32Float,
        DXGI_FORMAT_D24_UNORM_S8_UINT => GPUTextureFormat::Depth24PlusStencil8,
        DXGI_FORMAT_D32_FLOAT => GPUTextureFormat::Depth32Float,
        DXGI_FORMAT_BC1_UNORM => GPUTextureFormat::BC1RGBAUnorm,
        DXGI_FORMAT_BC2_UNORM => GPUTextureFormat::BC2RGBAUnorm,
        DXGI_FORMAT_BC3_UNORM => GPUTextureFormat::BC3RGBAUnorm,
        DXGI_FORMAT_BC7_UNORM => GPUTextureFormat::BC7RGBAUnorm,
        // ... more formats
        _ => {
            log::warn!("Unknown format {:?}, using RGBA8", dx_format);
            GPUTextureFormat::RGBA8Unorm
        }
    }
}
```

### Texture Cache

```rust
pub struct TextureCache {
    textures: HashMap<u64, CachedTexture>,
    total_memory: usize,
    max_memory: usize,
}

pub struct CachedTexture {
    gpu_texture: GPUTexture,
    guest_address: u64,
    width: u32,
    height: u32,
    format: GPUTextureFormat,
    mip_levels: u32,
    last_access: Instant,
    dirty: bool,
}

impl TextureCache {
    pub fn get_or_create(
        &mut self,
        device: &GPUDevice,
        guest_addr: u64,
        desc: &TextureDesc,
        memory: &MemoryBus,
    ) -> &GPUTexture {
        if let Some(cached) = self.textures.get_mut(&guest_addr) {
            cached.last_access = Instant::now();
            if !cached.dirty {
                return &cached.gpu_texture;
            }
        }
        
        // Create new texture
        let texture = device.create_texture(&GPUTextureDescriptor {
            size: GPUExtent3D {
                width: desc.width,
                height: desc.height,
                depth_or_array_layers: desc.depth,
            },
            mip_level_count: desc.mip_levels,
            sample_count: 1,
            dimension: desc.dimension,
            format: dx_to_webgpu_format(desc.format),
            usage: GPUTextureUsage::TEXTURE_BINDING | GPUTextureUsage::COPY_DST,
        });
        
        // Upload texture data
        self.upload_texture_data(device, &texture, guest_addr, desc, memory);
        
        // Evict old textures if needed
        self.maybe_evict();
        
        self.textures.insert(guest_addr, CachedTexture {
            gpu_texture: texture,
            guest_address: guest_addr,
            width: desc.width,
            height: desc.height,
            format: dx_to_webgpu_format(desc.format),
            mip_levels: desc.mip_levels,
            last_access: Instant::now(),
            dirty: false,
        });
        
        &self.textures.get(&guest_addr).unwrap().gpu_texture
    }
}
```

---

## Windows Aero Glass Effect

### D3D9Ex requirement (DWM composition path)

Windows 7’s Desktop Window Manager (DWM) typically uses **Direct3D 9Ex** (`Direct3DCreate9Ex`, `CreateDeviceEx`, `PresentEx`) rather than legacy D3D9. Supporting the D3D9Ex API surface and its frame pacing/statistics semantics is therefore a prerequisite for stable Aero composition.

See: [D3D9Ex / DWM Compatibility](./16-d3d9ex-dwm-compatibility.md) for the concrete guest UMD + host protocol requirements (PresentEx flags, present stats, shared surfaces, and fences).

### Aero Implementation Strategy

Windows 7's Aero glass effect requires:
1. **Desktop Window Manager (DWM)** compositing
2. **Blur shader** for glass effect
3. **Alpha blending** for transparency

```rust
pub struct AeroCompositor {
    blur_pipeline: GPURenderPipeline,
    composite_pipeline: GPURenderPipeline,
    blur_texture: GPUTexture,
}

impl AeroCompositor {
    pub fn render_glass_effect(
        &self,
        encoder: &mut GPUCommandEncoder,
        source: &GPUTextureView,
        mask: &GPUTextureView,  // Glass region mask
        output: &GPUTextureView,
    ) {
        // Pass 1: Horizontal blur
        {
            let pass = encoder.begin_render_pass(&GPURenderPassDescriptor {
                color_attachments: &[GPURenderPassColorAttachment {
                    view: &self.blur_texture.create_view(&Default::default()),
                    load_op: GPULoadOp::Clear,
                    store_op: GPUStoreOp::Store,
                }],
            });
            
            pass.set_pipeline(&self.blur_pipeline);
            pass.set_bind_group(0, &self.create_blur_bind_group(source, true));
            pass.draw(6, 1, 0, 0);  // Fullscreen quad
            pass.end();
        }
        
        // Pass 2: Vertical blur
        {
            let pass = encoder.begin_render_pass(&GPURenderPassDescriptor {
                color_attachments: &[GPURenderPassColorAttachment {
                    view: output,
                    load_op: GPULoadOp::Clear,
                    store_op: GPUStoreOp::Store,
                }],
            });
            
            pass.set_pipeline(&self.blur_pipeline);
            pass.set_bind_group(0, &self.create_blur_bind_group(&self.blur_texture_view, false));
            pass.draw(6, 1, 0, 0);
            pass.end();
        }
        
        // Pass 3: Composite with original using mask
        // ... composite pass
    }
}

// WGSL Blur Shader
const BLUR_SHADER: &str = r#"
struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@group(0) @binding(0) var source_texture: texture_2d<f32>;
@group(0) @binding(1) var source_sampler: sampler;
@group(0) @binding(2) var<uniform> params: BlurParams;

struct BlurParams {
    direction: vec2<f32>,
    radius: f32,
    sigma: f32,
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    var color = vec4<f32>(0.0);
    var total_weight = 0.0;
    
    let pixel_size = params.direction / vec2<f32>(textureDimensions(source_texture));
    
    for (var i = -i32(params.radius); i <= i32(params.radius); i++) {
        let offset = f32(i);
        let weight = exp(-(offset * offset) / (2.0 * params.sigma * params.sigma));
        let sample_uv = input.uv + pixel_size * offset;
        
        color += textureSample(source_texture, source_sampler, sample_uv) * weight;
        total_weight += weight;
    }
    
    return color / total_weight;
}
"#;
```

---

## Framebuffer Presentation

### Presentation Color Policy (sRGB + Alpha Mode)

To match Windows swapchain expectations and avoid backend-dependent output differences, the presenter must have an explicit and consistent policy:

- **Input framebuffer encoding (default):** `RGBA8` **linear** (`rgba8unorm`).
  - All GPU shading/blending math should happen in linear space.
- **Presented output encoding (default):** **sRGB**.
  - Prefer an `*Srgb` canvas/surface format (or sRGB view format) when available.
  - Fall back to **manual sRGB encoding in the blit shader** only when presenting to a linear surface.
  - Do **not** apply gamma in both places (avoid “double gamma”).
- **Presented alpha mode (default):** **opaque**.
  - Web output should not accidentally blend with the page background; Windows desktop scanout is effectively opaque.
  - WebGPU: configure the canvas with `alphaMode: "opaque"`.
  - WebGL2: create the context with `{ alpha: false }` and force `alpha=1.0` in the blit shader.
- **UV / Y convention:** use **top-left UV origin** for presentation across backends, and ensure any required flips are explicit.

### WebGPU Canvas Configuration Notes

`navigator.gpu.getPreferredCanvasFormat()` is typically a **linear** format (often `bgra8unorm`). To present in sRGB without shader gamma:

- Configure the canvas with `format: preferred` and include the `-srgb` variant in `viewFormats` when supported.
- Create the render attachment view using the `-srgb` view format and render linear output into it.
- Only enable shader sRGB encoding when an sRGB view/format is unavailable.

Chrome currently requires this `viewFormats` mechanism for sRGB presentation (the preferred format may remain `bgra8unorm`).

### WebGL2 Raw Presenter Notes

WebGL2’s default framebuffer sRGB behavior is historically inconsistent across browsers and context attributes. For deterministic output, the raw presenter should:

- Treat the input framebuffer as **linear** by default.
- Apply **manual sRGB encoding in the fragment shader** when sRGB output is requested.
- Use a top-left UV convention and keep `UNPACK_FLIP_Y_WEBGL = 0` (so CPU buffers written top-to-bottom match the UV convention).

### Known Browser Differences (as of early WebGPU rollouts)

- **WebGPU sRGB swapchain formats:** some browsers expose only a *linear* preferred canvas format and require `viewFormats` for `*-srgb` views; others may reject `viewFormats` entirely (use the shader fallback).
- **Canvas alpha behavior:** WebGPU `alphaMode` defaults and WebGL2 `premultipliedAlpha` defaults differ; set them explicitly to avoid subtle halos/darkening.

### Validation Scene (Test Card)

Add a GPU validation scene that renders:

- A **grayscale ramp** (gamma correctness).
- An **alpha gradient** test region (premultiplied vs opaque correctness).
- **Corner markers** (Y-flip / UV convention correctness).

Automate this using Playwright by hashing the presented pixel output per backend and ensuring WebGPU and WebGL2 match.

### Double Buffering

```rust
pub struct Framebuffer {
    front_buffer: GPUTexture,
    back_buffer: GPUTexture,
    current_back: usize,
}

impl Framebuffer {
    pub fn present(&mut self, device: &GPUDevice, queue: &GPUQueue) {
        // Copy back buffer to front
        let encoder = device.create_command_encoder(&Default::default());
        
        encoder.copy_texture_to_texture(
            GPUImageCopyTexture {
                texture: &self.back_buffer,
                mip_level: 0,
                origin: GPUOrigin3D::default(),
            },
            GPUImageCopyTexture {
                texture: &self.front_buffer,
                mip_level: 0,
                origin: GPUOrigin3D::default(),
            },
            self.size,
        );
        
        queue.submit(&[encoder.finish()]);
        
        // Swap
        self.current_back = 1 - self.current_back;
    }
    
    pub fn render_to_canvas(&self, canvas_context: &GPUCanvasContext) {
        // Blit front buffer to canvas
        let current_texture = canvas_context.get_current_texture();
        
        // ... copy front_buffer to current_texture
    }
}
```

---

## GPU Reliability & Diagnostics

Browser GPU subsystems fail in ways that desktop apps rarely encounter: GPU process resets, device loss, surface reconfiguration requirements, and WebGL context loss. Aero treats these as *expected* events and routes them through a structured diagnostics + recovery pipeline.

### Structured error events

Both Rust and TypeScript use a shared event shape:

```
GpuErrorEvent { time_ms, backend_kind, severity, category, message, details? }
```

Categories are intentionally coarse so they can be aggregated and alerted on: `Init`, `DeviceLost`, `Surface`, `ShaderCompile`, `PipelineCreate`, `Validation`, `OutOfMemory`, `Unknown`.

### Surface recovery during present

When presenting a frame, handle surface errors deterministically:

- `Lost` / `Outdated`: reconfigure the surface and retry once.
- `Timeout`: drop the frame (warn) and continue.
- `OutOfMemory`: emit a fatal event and stop rendering.

### Device-lost recovery

Device loss triggers a recovery state machine:

`Running → Recovering → Running | Failed`

Recovery attempts re-init on the current backend first, and falls back to the other backend when available (WebGPU ↔ WebGL2). Every attempt and outcome should emit a `GpuErrorEvent` so the main thread can surface actionable diagnostics.

### Telemetry counters

Track cheap counters for visibility and regression testing:

- presents attempted/succeeded
- recoveries attempted/succeeded
- surface reconfigures

Expose these via a `get_gpu_stats()` JSON method so they can be polled over IPC.

## Performance Considerations

### Batching Draw Calls

```rust
pub struct DrawCallBatcher {
    pending_draws: Vec<DrawCall>,
    current_state: PipelineState,
}

impl DrawCallBatcher {
    pub fn queue_draw(&mut self, draw: DrawCall) {
        // Check if we can batch with previous draw
        if let Some(last) = self.pending_draws.last_mut() {
            if last.can_merge_with(&draw) {
                last.merge(draw);
                return;
            }
        }
        
        self.pending_draws.push(draw);
    }
    
    pub fn flush(&mut self, encoder: &mut GPUCommandEncoder) {
        // Sort draws by state to minimize pipeline switches
        self.pending_draws.sort_by_key(|d| d.state_key());
        
        let mut current_pipeline = None;
        
        for draw in &self.pending_draws {
            if current_pipeline != Some(draw.pipeline_key()) {
                encoder.set_pipeline(draw.pipeline);
                current_pipeline = Some(draw.pipeline_key());
            }
            
            encoder.draw(draw.vertex_count, draw.instance_count, draw.first_vertex, 0);
        }
        
        self.pending_draws.clear();
    }
}
```

### Shader Caching

```rust
pub struct ShaderCache {
    compiled_shaders: HashMap<ShaderKey, CompiledShader>,
    wgsl_cache: HashMap<Vec<u8>, String>,  // DXBC -> WGSL
}

impl ShaderCache {
    pub fn get_shader(&mut self, dxbc: &[u8], device: &GPUDevice) -> &CompiledShader {
        let key = ShaderKey::from_bytecode(dxbc);
        
        if !self.compiled_shaders.contains_key(&key) {
            // Check WGSL cache first
            let wgsl = if let Some(cached) = self.wgsl_cache.get(dxbc) {
                cached.clone()
            } else {
                let wgsl = self.translate_dxbc_to_wgsl(dxbc);
                self.wgsl_cache.insert(dxbc.to_vec(), wgsl.clone());
                wgsl
            };
            
            // Compile WGSL
            let module = device.create_shader_module(&GPUShaderModuleDescriptor {
                code: &wgsl,
            });
            
            self.compiled_shaders.insert(key.clone(), CompiledShader {
                module,
                wgsl,
                reflection: self.reflect_shader(dxbc),
            });
        }
        
        self.compiled_shaders.get(&key).unwrap()
    }
}
```

### Persistent GPU Cache (Phase 1: Shader Translation + Reflection)

In practice, an in-memory `HashMap` is not enough: the DXBC → WGSL translation and reflection can dominate startup time on repeat runs. Persisting translation artifacts across sessions (IndexedDB, with optional OPFS indirection for large blobs) avoids multi-second stalls while remaining safe.

Note: this is a host-layer cache; IndexedDB is async-only and does not back the synchronous Rust
disk/controller path. See:

- [`19-indexeddb-storage-story.md`](./19-indexeddb-storage-story.md)
- [`20-storage-trait-consolidation.md`](./20-storage-trait-consolidation.md)

We persist *derived artifacts* across browser sessions:

- **DXBC → WGSL output** (string)
- **Reflection metadata** required for bind-group/pipeline layout derivation

> We intentionally do **not** persist compiled WebGPU pipelines or `GPUShaderModule` objects. Those are backend/driver-managed and not stable across sessions.

#### Cache Key Scheme (Versioned)

All persisted artifacts are keyed by a stable, versioned identifier:

```
CacheKey = hash(content_bytes) + schema_version + backend_kind + device_fingerprint(optional)
```

Where:

- `content_bytes`: the *source* bytes that uniquely determine the derived artifact (e.g., raw DXBC bytecode).
- `hash`: a strong hash such as BLAKE3/SHA-256 encoded as hex/base64url.
- `schema_version`: explicit breaking-change counter.
- `backend_kind`: identifies the translator/reflection backend and configuration (e.g., `dxbc->wgsl@v1`, `naga@0.20`). Translator flags (e.g., half-pixel mode) must be included here.
- `device_fingerprint` (optional): include only when codegen depends on device limits/features (a capabilities hash).

Example key string (human-readable, safe for IndexedDB keys):

```
gpu-cache/v{CACHE_SCHEMA_VERSION}/{backend_kind}/{device_hash_or_none}/{content_hash}
```

`CACHE_SCHEMA_VERSION` **must** be incremented whenever we change:

- serialization format of the cached value
- reflection schema
- translation semantics (including dependency version bumps that change output)

#### Storage Backends

- **Primary: IndexedDB** for small blobs (WGSL + reflection metadata are typically small enough).
- **Optional: OPFS** for large blobs if we later cache bigger artifacts (e.g., preprocessed shader libraries, large debug maps).

In practice, the implementation can:

- store the entry inline in IndexedDB when `value_bytes <= INLINE_THRESHOLD` (e.g., 64–256 KiB)
- store larger values in OPFS (content-addressed file name = cache key hash) and keep a small IndexedDB record pointing to the OPFS path

#### Entry Format + Corruption Defense

Each record stores:

- `value_bytes` (WGSL + reflection, serialized)
- `size_bytes`
- `created_at_ms`
- `last_access_ms` (for LRU)

On **persistent cache hit**, we still treat data as untrusted:

1. Deserialize.
2. **Validate WGSL via Naga** (parse + validate).
3. If validation fails, treat as a miss and delete the corrupted entry.
4. If WGSL compilation fails (browser updates / WGSL validation changes), invalidate the entry and fall back to retranslation.

This prevents "poisoned" caches from permanently breaking startup.

#### Integration with `ShaderCache`

For the DXBC→WGSL/reflection phase:

1. Check **persistent** cache.
2. Check **in-memory** cache.
3. Translate + reflect + validate, then populate both caches.

The in-memory cache still holds session-only objects like `GPUShaderModule` and any compiled pipeline state.

#### Eviction + `clear_cache()`

Persisted caches must be bounded.

- Maintain a **maximum total byte budget** (e.g., 32–128 MiB configurable).
- Track `last_access_ms` per entry.
- When inserting, evict least-recently-used entries until under budget.

Expose a `clear_cache()` API that removes all persisted GPU cache state (IndexedDB + any OPFS files used).

#### Telemetry

Track at minimum:

- `persistent_hits`, `persistent_misses`
- `bytes_read`, `bytes_written`
- `entries_evicted`, `eviction_bytes`

Telemetry should be queryable from the UI/devtools overlay to confirm the persistent cache is working and to diagnose regressions.

See `web/gpu-cache/persistent_cache.ts` for a concrete implementation of the persistent cache layer.


### Graphics telemetry hooks (PF-007)

Graphics performance tuning needs visibility into two separate bottlenecks:

- **CPU-bound:** DirectX translation/state setup, command encoding, and resource uploads
- **GPU-bound:** shader execution and memory bandwidth

To diagnose this, route WebGPU calls through a thin instrumented wrapper that records:

- Draw calls / frame (`draw()` / `drawIndexed()` count)
- Render passes / frame (`beginRenderPass()` count)
- Pipeline switches / frame (`setPipeline()` churn)
- Bind group changes / frame (`setBindGroup()` churn)
- Bytes uploaded / frame (`queue.writeBuffer()` + `queue.writeTexture()` + staging copies)
- CPU time in translation + command encoding (timers around translator + encoder building)
- Best-effort GPU time when timestamp queries are available (`timestamp-query` feature)

These metrics feed:

1. The on-screen perf HUD (quick, human-readable diagnosis)
2. JSON telemetry export (regression tracking and automated analysis)

---

## Next Steps

- See [`docs/graphics/win7-wddm11-aerogpu-driver.md`](./graphics/win7-wddm11-aerogpu-driver.md) for the Windows 7 WDDM 1.1 (KMD+UMD) architecture and command transport boundary.
- For Windows 7 Aero bring-up, see [Win7 D3D9Ex UMD minimal surface](./graphics/win7-d3d9ex-umd-minimal.md)
- For Windows 7 D3D10/D3D11 UMD bring-up (DDI entrypoints, DXGI swapchain expectations), see [Win7 D3D10/11 UMD minimal surface](./graphics/win7-d3d10-11-umd-minimal.md)
- For a concrete Win7 D3D11 `d3d11umddi.h` function-table checklist (which entries must be non-null vs safely stubbable for FL10_0), see [Win7 D3D11 DDI function tables](./graphics/win7-d3d11ddi-function-tables.md)
- See [Direct3D 10/11 Translation](./16-d3d10-11-translation.md) for SM4/SM5 pipeline/resource details
- See [Guest GPU driver strategy](./graphics/guest-gpu-driver-strategy.md) for Windows guest driver options (virtio-gpu reuse vs custom WDDM)
- See [Win7 vblank/present timing requirements](./graphics/win7-vblank-present-requirements.md) for the minimal contract needed to keep DWM (Aero) composition stable.
- See [Audio Subsystem](./06-audio-subsystem.md) for sound emulation
- See [Performance Optimization](./10-performance-optimization.md) for GPU perf tips
- See [Task Breakdown](./15-agent-task-breakdown.md) for graphics tasks
