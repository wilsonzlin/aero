# 02 - CPU Emulation Engine

## Overview

The CPU emulation engine is the heart of Aero. It must accurately emulate the x86-64 architecture at sufficient speed to run Windows 7 smoothly. This document details the design of a tiered execution system combining interpretation and JIT compilation.

---

## x86-64 Architecture Requirements

### Processor Modes

Windows 7 uses all processor modes during boot and operation:

| Mode | When Used | Key Features |
|------|-----------|--------------|
| **Real Mode** | BIOS POST, early boot | 16-bit, 1MB address space, segmented |
| **Protected Mode** | Boot manager, early kernel | 32-bit, paging, privilege rings |
| **Long Mode (64-bit)** | Windows 7 kernel, apps | 64-bit, extended registers, RIP-relative |
| **Virtual 8086** | Legacy DOS apps (NTVDM) | Real mode emulation in protected mode |
| **System Management** | Power management, firmware | Hidden mode, entered via SMI |

### Register Set

```
┌─────────────────────────────────────────────────────────────────┐
│                    x86-64 Register File                          │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│  General Purpose (64-bit, can access 32/16/8-bit parts):        │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │ RAX RBX RCX RDX RSI RDI RBP RSP R8-R15                  │    │
│  └─────────────────────────────────────────────────────────┘    │
│                                                                  │
│  Instruction Pointer:                                            │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │ RIP (64-bit)                                             │    │
│  └─────────────────────────────────────────────────────────┘    │
│                                                                  │
│  Flags Register:                                                 │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │ RFLAGS (CF, PF, AF, ZF, SF, TF, IF, DF, OF, IOPL, etc.) │    │
│  └─────────────────────────────────────────────────────────┘    │
│                                                                  │
│  Segment Registers:                                              │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │ CS DS ES FS GS SS (+ hidden descriptor cache)           │    │
│  └─────────────────────────────────────────────────────────┘    │
│                                                                  │
│  Control Registers:                                              │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │ CR0 CR2 CR3 CR4 CR8 (paging, protection, features)      │    │
│  └─────────────────────────────────────────────────────────┘    │
│                                                                  │
│  Debug Registers:                                                │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │ DR0-DR3 (breakpoint addresses), DR6 DR7 (control)       │    │
│  └─────────────────────────────────────────────────────────┘    │
│                                                                  │
│  Model-Specific Registers (selected):                            │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │ EFER, STAR, LSTAR, CSTAR, FMASK, FS_BASE, GS_BASE,      │    │
│  │ KERNEL_GS_BASE, TSC, APIC_BASE, PAT, MTRR...            │    │
│  └─────────────────────────────────────────────────────────┘    │
│                                                                  │
│  FPU/SSE/AVX Registers:                                          │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │ x87: ST0-ST7 (80-bit)                                    │    │
│  │ SSE: XMM0-XMM15 (128-bit)                                │    │
│  │ AVX: YMM0-YMM15 (256-bit, if supported)                  │    │
│  │ MXCSR, FCW, FSW, FTW                                     │    │
│  └─────────────────────────────────────────────────────────┘    │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

### Instruction Set Categories

| Category | Examples | Priority | Notes |
|----------|----------|----------|-------|
| **Data Movement** | MOV, PUSH, POP, LEA, XCHG | Critical | Most common |
| **Arithmetic** | ADD, SUB, MUL, DIV, INC, DEC | Critical | Flag updates |
| **Logical** | AND, OR, XOR, NOT, TEST | Critical | Flag updates |
| **Control Flow** | JMP, CALL, RET, Jcc, LOOP | Critical | Block boundaries |
| **String** | MOVS, STOS, LODS, CMPS, SCAS | High | REP prefixes |
| **Bit Manipulation** | BT, BTS, BTR, BTC, BSF, BSR | High | Used heavily |
| **Shift/Rotate** | SHL, SHR, SAR, ROL, ROR, RCL, RCR | High | |
| **System** | INT, IRET, SYSENTER, SYSCALL | Critical | OS interface |
| **Privileged** | MOV CR, MOV DR, LGDT, LIDT | Critical | Ring 0 only |
| **x87 FPU** | FLD, FST, FADD, FMUL, etc. | Medium | Legacy float |
| **SSE/SSE2** | MOVAPS, ADDPS, MULPS, etc. | High | Modern float |
| **SSE3/SSSE3** | HADDPS, PSHUFB, etc. | Medium | Optimization |
| **SSE4.1/4.2** | PCMPESTRI, CRC32, POPCNT | Medium | Windows 7 uses |
| **AES-NI** | AESENC, AESDEC | Low | Crypto accel |
| **AVX** | VADDPS, VMOVAPS, etc. | Low | Optional |

---

## CPUID / Feature Model

Windows 7 boot and many drivers gate behavior based on CPUID leaves and feature bits (SSE2/PAE/NX/APIC/TSC, etc.). Aero must expose a *coherent* CPUID + MSR surface (e.g. advertising NX implies `EFER.NXE` exists and behaves).

See [`docs/cpu/README.md`](./cpu/README.md) for the current CPUID leaf coverage and feature profiles.

---

## Tiered Execution Architecture

### Tier 0: Interpreter

The interpreter executes instructions one at a time. It's slow but handles all edge cases correctly.

```rust
pub struct Interpreter {
    cpu: CpuState,
    memory: MemoryBus,
    decoder: InstructionDecoder,
}

impl Interpreter {
    pub fn step(&mut self) -> ExecutionResult {
        // 1. Fetch instruction bytes
        let ip = self.cpu.get_instruction_pointer();
        let bytes = self.memory.fetch_instruction(ip, MAX_INST_LEN);
        
        // 2. Decode instruction
        let inst = self.decoder.decode(&bytes)?;
        
        // 3. Execute instruction
        let result = self.execute_instruction(&inst)?;
        
        // 4. Update instruction pointer
        self.cpu.advance_ip(inst.length);
        
        // 5. Check for interrupts
        self.check_pending_interrupts()?;
        
        Ok(result)
    }
    
    fn execute_instruction(&mut self, inst: &Instruction) -> Result<()> {
        match inst.opcode {
            Opcode::MovRegReg => {
                let src = self.cpu.get_reg(inst.src);
                self.cpu.set_reg(inst.dst, src);
            }
            Opcode::MovRegMem => {
                let addr = self.compute_effective_address(inst)?;
                let val = self.memory.read(addr, inst.operand_size)?;
                self.cpu.set_reg(inst.dst, val);
            }
            Opcode::Add => {
                let a = self.cpu.get_reg(inst.dst);
                let b = self.get_operand_value(inst)?;
                let (result, flags) = alu_add(a, b, inst.operand_size);
                self.cpu.set_reg(inst.dst, result);
                self.cpu.update_flags(flags);
            }
            // ... hundreds more opcodes
        }
    }
}
```

### Tier 1: Baseline JIT

The baseline JIT compiles basic blocks to WASM quickly, without optimization.

```rust
pub struct BaselineJit {
    compiler: WasmCompiler,
    code_cache: CodeCache,
}

impl BaselineJit {
    pub fn compile_block(&mut self, address: u64) -> CompiledBlock {
        // 1. Find basic block boundaries
        let block = self.find_basic_block(address);
        
        // 2. Generate WASM without optimization
        let wasm_module = self.compiler.compile_basic(&block);
        
        // 3. Instantiate module
        let instance = WebAssembly::instantiate(&wasm_module);
        
        // 4. Cache and return
        self.code_cache.insert(address, instance.clone());
        instance
    }
    
    fn find_basic_block(&self, start: u64) -> BasicBlock {
        let mut instructions = Vec::new();
        let mut ip = start;
        
        loop {
            let inst = self.decode_at(ip);
            instructions.push(inst.clone());
            ip += inst.length as u64;
            
            // Block ends at control flow changes
            if inst.is_branch() || inst.is_call() || inst.is_ret() {
                break;
            }
            
            // Or if we hit a block size limit
            if instructions.len() >= MAX_BLOCK_SIZE {
                break;
            }
        }
        
        BasicBlock { start, instructions }
    }
}
```

### Tier 2: Optimizing JIT

The optimizing JIT performs advanced transformations for hot code paths.

```rust
pub struct OptimizingJit {
    ir_builder: IrBuilder,
    optimizer: IrOptimizer,
    wasm_backend: WasmBackend,
    profile_data: ProfileData,
}

impl OptimizingJit {
    pub fn compile_hot_region(&mut self, entry: u64) -> CompiledRegion {
        // 1. Build trace/region from profile data
        let trace = self.build_trace(entry);
        
        // 2. Convert to IR
        let mut ir = self.ir_builder.build_from_trace(&trace);
        
        // 3. Optimization passes
        self.optimizer.run_passes(&mut ir, &[
            Pass::DeadCodeElimination,
            Pass::ConstantFolding,
            Pass::CommonSubexpressionElimination,
            Pass::FlagElimination,      // x86-specific: remove unused flag computations
            Pass::AddressComputation,   // Optimize effective address calculations
            Pass::MemoryCoalescing,     // Combine adjacent memory accesses
            Pass::LoopInvariantCodeMotion,
            Pass::RegisterAllocation,
        ]);
        
        // 4. Generate optimized WASM
        let wasm = self.wasm_backend.generate(&ir);
        
        // 5. Add guards for deoptimization
        self.add_deopt_guards(&wasm, &trace);
        
        wasm
    }
}
```

---

## Instruction Decoder

### Decoding Pipeline

x86-64 instruction encoding is notoriously complex:

```
┌─────────────────────────────────────────────────────────────────┐
│                x86-64 Instruction Format                         │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│  ┌────────┬────────┬─────┬───────┬─────┬────────┬────────────┐  │
│  │Legacy  │ REX/   │Opcode│ModR/M │ SIB │Displace│ Immediate │  │
│  │Prefixes│VEX/EVEX│      │       │     │ment    │           │  │
│  ├────────┼────────┼─────┼───────┼─────┼────────┼────────────┤  │
│  │0-4     │0-4     │1-3  │0-1    │0-1  │0,1,2,4 │0,1,2,4,8  │  │
│  │bytes   │bytes   │bytes│byte   │byte │bytes   │bytes      │  │
│  └────────┴────────┴─────┴───────┴─────┴────────┴────────────┘  │
│                                                                  │
│  Total instruction length: 1-15 bytes                            │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

### Decoder Implementation

```rust
pub struct InstructionDecoder {
    mode: CpuMode,  // 16, 32, or 64-bit
}

impl InstructionDecoder {
    pub fn decode(&self, bytes: &[u8]) -> Result<DecodedInstruction> {
        let mut cursor = 0;
        
        // 1. Parse legacy prefixes (up to 4)
        let prefixes = self.parse_legacy_prefixes(bytes, &mut cursor);
        
        // 2. Parse REX/VEX/EVEX prefix (64-bit mode only)
        let rex = if self.mode == CpuMode::Long {
            self.parse_rex_prefix(bytes, &mut cursor)
        } else {
            None
        };
        
        // 3. Parse opcode (1-3 bytes)
        let opcode = self.parse_opcode(bytes, &mut cursor, &prefixes)?;
        
        // 4. Parse ModR/M if needed
        let modrm = if opcode.has_modrm() {
            Some(self.parse_modrm(bytes, &mut cursor, &rex))
        } else {
            None
        };
        
        // 5. Parse SIB if needed
        let sib = if modrm.as_ref().map_or(false, |m| m.needs_sib()) {
            Some(self.parse_sib(bytes, &mut cursor, &rex))
        } else {
            None
        };
        
        // 6. Parse displacement
        let displacement = self.parse_displacement(bytes, &mut cursor, &modrm);
        
        // 7. Parse immediate
        let immediate = self.parse_immediate(bytes, &mut cursor, &opcode, &prefixes);
        
        Ok(DecodedInstruction {
            prefixes,
            rex,
            opcode,
            modrm,
            sib,
            displacement,
            immediate,
            length: cursor,
        })
    }
}
```

### Opcode Tables

We use a multi-level table approach for fast opcode lookup:

```rust
// Primary opcode table (256 entries for 1-byte opcodes)
static OPCODE_TABLE_1BYTE: [OpcodeEntry; 256] = [
    /* 0x00 */ OpcodeEntry::new(Opcode::AddRmR8, ModRM::Required),
    /* 0x01 */ OpcodeEntry::new(Opcode::AddRmRv, ModRM::Required),
    /* 0x02 */ OpcodeEntry::new(Opcode::AddR8Rm, ModRM::Required),
    // ... all 256 entries
    /* 0x0F */ OpcodeEntry::escape(EscapeTable::TwoByte),
    // ...
];

// Two-byte opcode table (0F xx)
static OPCODE_TABLE_2BYTE: [OpcodeEntry; 256] = [
    /* 0F 00 */ OpcodeEntry::group(Group::Group6),
    /* 0F 01 */ OpcodeEntry::group(Group::Group7),
    // ... SSE instructions, etc.
    /* 0F 38 */ OpcodeEntry::escape(EscapeTable::ThreeByte38),
    /* 0F 3A */ OpcodeEntry::escape(EscapeTable::ThreeByte3A),
    // ...
];
```

---

## Flag Handling

x86 flags are expensive to emulate correctly. We use **lazy flag evaluation**:

```rust
pub struct LazyFlags {
    // Instead of computing all flags after every operation,
    // we store the operands and compute flags on demand
    result: u64,
    operand1: u64,
    operand2: u64,
    operation: FlagOperation,
    size: OperandSize,
}

impl LazyFlags {
    pub fn get_cf(&self) -> bool {
        match self.operation {
            FlagOperation::Add => {
                // Carry = result < operand1 (unsigned overflow)
                self.result < self.operand1
            }
            FlagOperation::Sub => {
                // Carry = operand1 < operand2 (borrow)
                self.operand1 < self.operand2
            }
            FlagOperation::Logic => false,  // Logic ops clear CF
            // ... etc
        }
    }
    
    pub fn get_zf(&self) -> bool {
        self.mask_result() == 0
    }
    
    pub fn get_sf(&self) -> bool {
        let sign_bit = match self.size {
            OperandSize::Byte => 7,
            OperandSize::Word => 15,
            OperandSize::Dword => 31,
            OperandSize::Qword => 63,
        };
        (self.result >> sign_bit) & 1 != 0
    }
    
    // More flag getters...
}
```

### Flag Elimination Optimization

In the optimizing JIT, we track which flags are actually used:

```rust
// Before optimization:
// ADD RAX, RBX      ; Sets CF, OF, SF, ZF, PF, AF
// ADD RCX, RDX      ; Also sets flags, clobbers previous
// JZ label          ; Only reads ZF

// After flag elimination:
// ADD RAX, RBX      ; No flag computation needed
// ADD RCX, RDX      ; Only compute ZF
// JZ label

impl FlagElimination {
    fn run(&mut self, ir: &mut IrGraph) {
        // Backward dataflow analysis
        let mut live_flags = FlagSet::empty();
        
        for block in ir.blocks_reverse_postorder() {
            for inst in block.instructions.iter().rev() {
                if inst.reads_flags() {
                    live_flags |= inst.flags_read();
                }
                if inst.writes_flags() {
                    // Only generate flag computation for live flags
                    inst.required_flags = live_flags & inst.flags_written();
                    live_flags -= inst.flags_written();
                }
            }
        }
    }
}
```

---

## Memory Access Translation

### Address Computation

```rust
impl CpuEmulator {
    fn compute_effective_address(&self, operand: &MemoryOperand) -> u64 {
        let mut addr = 0u64;
        
        // Base register
        if let Some(base) = operand.base {
            addr = addr.wrapping_add(self.get_reg(base));
        }
        
        // Index register with scale
        if let Some(index) = operand.index {
            let index_val = self.get_reg(index);
            let scaled = index_val.wrapping_mul(operand.scale as u64);
            addr = addr.wrapping_add(scaled);
        }
        
        // Displacement
        addr = addr.wrapping_add(operand.displacement as u64);
        
        // Segment override (protected/long mode)
        if self.mode != CpuMode::Real {
            addr = self.apply_segment(addr, operand.segment)?;
        }
        
        addr
    }
}
```

### Segmentation (Protected Mode)

```rust
pub struct SegmentDescriptor {
    base: u32,
    limit: u32,
    access: AccessRights,
    flags: SegmentFlags,
}

impl CpuEmulator {
    fn load_segment(&mut self, selector: u16, reg: SegmentRegister) -> Result<()> {
        if selector == 0 && reg != SegmentRegister::CS {
            // Null selector (except for CS)
            self.segments[reg].null = true;
            return Ok(());
        }
        
        // Get descriptor from GDT/LDT
        let table_base = if selector & 4 != 0 {
            self.ldtr.base
        } else {
            self.gdtr.base
        };
        
        let index = (selector >> 3) as u64;
        let desc_addr = table_base + index * 8;
        
        let desc_low = self.memory.read_u32(desc_addr);
        let desc_high = self.memory.read_u32(desc_addr + 4);
        
        let descriptor = SegmentDescriptor::from_raw(desc_low, desc_high);
        
        // Validate access rights
        self.validate_segment_access(&descriptor, reg)?;
        
        // Load into hidden cache
        self.segments[reg] = SegmentCache {
            selector,
            base: descriptor.base,
            limit: descriptor.effective_limit(),
            access: descriptor.access,
        };
        
        Ok(())
    }
}
```

---

## Interrupt and Exception Handling

### Exception Types

```rust
pub enum Exception {
    DivideError = 0,         // #DE
    Debug = 1,               // #DB
    Nmi = 2,                 // NMI
    Breakpoint = 3,          // #BP
    Overflow = 4,            // #OF
    BoundRange = 5,          // #BR
    InvalidOpcode = 6,       // #UD
    DeviceNotAvailable = 7,  // #NM
    DoubleFault = 8,         // #DF
    InvalidTss = 10,         // #TS
    SegmentNotPresent = 11,  // #NP
    StackFault = 12,         // #SS
    GeneralProtection = 13,  // #GP
    PageFault = 14,          // #PF
    X87Fpu = 16,             // #MF
    AlignmentCheck = 17,     // #AC
    MachineCheck = 18,       // #MC
    SimdFpu = 19,            // #XM
    Virtualization = 20,     // #VE
    ControlProtection = 21,  // #CP
}
```

### Interrupt Delivery

```rust
impl CpuEmulator {
    fn deliver_interrupt(&mut self, vector: u8, error_code: Option<u32>) -> Result<()> {
        match self.mode {
            CpuMode::Real => self.deliver_real_mode_interrupt(vector),
            CpuMode::Protected | CpuMode::Long => {
                let gate = self.read_idt_entry(vector)?;
                self.deliver_protected_interrupt(vector, gate, error_code)
            }
        }
    }
    
    fn deliver_protected_interrupt(
        &mut self,
        vector: u8,
        gate: IdtGate,
        error_code: Option<u32>,
    ) -> Result<()> {
        // Check gate type
        match gate.gate_type {
            GateType::Interrupt | GateType::Trap => {
                // Push state onto stack
                if self.mode == CpuMode::Long {
                    // 64-bit: Always push SS:RSP, even if no privilege change
                    self.push64(self.ss.selector as u64);
                    self.push64(self.rsp);
                    self.push64(self.rflags);
                    self.push64(self.cs.selector as u64);
                    self.push64(self.rip);
                    
                    if let Some(code) = error_code {
                        self.push64(code as u64);
                    }
                } else {
                    // 32-bit protected mode
                    if gate.dpl < self.cpl() {
                        // Stack switch needed
                        let (new_ss, new_esp) = self.get_stack_from_tss(gate.dpl)?;
                        // ... stack switch logic
                    }
                    self.push32(self.eflags);
                    self.push32(self.cs.selector as u32);
                    self.push32(self.eip);
                    
                    if let Some(code) = error_code {
                        self.push32(code);
                    }
                }
                
                // Clear IF for interrupt gates (not trap gates)
                if gate.gate_type == GateType::Interrupt {
                    self.rflags &= !FLAG_IF;
                }
                
                // Jump to handler
                self.cs.selector = gate.selector;
                self.load_segment(gate.selector, SegmentRegister::CS)?;
                self.rip = gate.offset;
            }
            GateType::Task => {
                // Task gate - perform task switch
                self.task_switch(gate.selector)?;
            }
        }
        
        Ok(())
    }
}
```

---

## Symmetric Multiprocessing (SMP) / Multi-vCPU

Windows 7 expects a real SMP platform:

- **Separate architectural state per CPU** (registers, MSRs, MMU/TLB, flags).
- **Shared physical memory and device models** (PCI, disks, timers, IOAPIC, etc.).
- **A per-CPU local APIC** for interrupt delivery and inter-processor interrupts (IPIs).

### Per-vCPU State

At minimum, each vCPU must have:

```rust
pub struct Vcpu {
    pub cpu: CpuState,     // GPRs, RIP/RFLAGS, segments, CR*, MSRs, MMU/TLB
    pub lapic: LocalApic,  // local APIC registers + pending vectors
}
```

### IPI Delivery via Local APIC (ICR)

SMP boot and common OS paths rely on APIC IPIs sent through the **Interrupt Command Register (ICR)**:

- **INIT IPI**: resets a target CPU and places it into a *wait-for-SIPI* state.
- **SIPI (Startup IPI)**: releases an AP from *wait-for-SIPI* and starts execution at `vector << 12`.
- **Fixed IPIs**: deliver a normal interrupt vector (e.g. TLB shootdowns, reschedule IPIs).

### Scheduling Model (Browser-Friendly)

Two practical approaches:

1. **One worker per vCPU** (when `SharedArrayBuffer` + threads are available).
   - vCPU workers execute in parallel.
   - Shared devices must define clear synchronization/ownership to avoid pervasive locking.
2. **Deterministic time-slicing in a single CPU worker** (recommended baseline).
   - Round-robin vCPUs with a fixed instruction/tick quantum.
   - Synchronization boundary at the end of each quantum:
     - Drain device events (MMIO completions, timers).
     - Deliver pending interrupts/IPIs.
   - Determinism is helpful for debugging and for reproducible tests.

## System Instructions

### SYSCALL/SYSRET (64-bit Fast System Call)

```rust
impl CpuEmulator {
    fn syscall(&mut self) -> Result<()> {
        // SYSCALL is only valid in 64-bit mode
        if self.mode != CpuMode::Long {
            return Err(Exception::InvalidOpcode);
        }
        
        // Save return address
        self.rcx = self.rip;
        
        // Save RFLAGS
        self.r11 = self.rflags;
        
        // Load new CS and SS from STAR MSR
        let star = self.msr_read(MSR_STAR);
        let syscall_cs = ((star >> 32) & 0xFFFF) as u16;
        self.cs.selector = syscall_cs;
        self.ss.selector = syscall_cs + 8;
        
        // Mask RFLAGS with FMASK MSR
        let fmask = self.msr_read(MSR_FMASK);
        self.rflags &= !fmask;
        
        // Jump to LSTAR
        self.rip = self.msr_read(MSR_LSTAR);
        
        // Set CPL to 0
        self.set_cpl(0);
        
        Ok(())
    }
    
    fn sysret(&mut self) -> Result<()> {
        // Must be in kernel mode
        if self.cpl() != 0 {
            return Err(Exception::GeneralProtection(0));
        }
        
        // Restore RFLAGS from R11
        self.rflags = self.r11;
        
        // Load user CS and SS from STAR MSR
        let star = self.msr_read(MSR_STAR);
        let sysret_cs = ((star >> 48) & 0xFFFF) as u16;
        self.cs.selector = sysret_cs | 3;  // Set RPL to 3
        self.ss.selector = sysret_cs + 8;
        
        // Return to user
        self.rip = self.rcx;
        self.set_cpl(3);
        
        Ok(())
    }
}
```

---

## WASM Code Generation

### Basic Block to WASM Translation

```rust
impl WasmCompiler {
    fn compile_basic_block(&mut self, block: &BasicBlock) -> WasmModule {
        let mut builder = WasmBuilder::new();
        
        // Function signature: (cpu_ptr: i32) -> i32 (next_rip)
        builder.begin_function(&[ValType::I32], &[ValType::I32]);
        
        // Load CPU state pointer into local
        let cpu_ptr = builder.get_param(0);
        
        for inst in &block.instructions {
            self.emit_instruction(&mut builder, cpu_ptr, inst);
        }
        
        // Return next RIP
        builder.i64_const(block.end_rip);
        builder.return_();
        
        builder.end_function();
        builder.build()
    }
    
    fn emit_instruction(&mut self, builder: &mut WasmBuilder, cpu: Local, inst: &Instruction) {
        match inst.opcode {
            Opcode::MovRegReg => {
                // dst = src
                let src_val = self.emit_read_reg(builder, cpu, inst.src);
                self.emit_write_reg(builder, cpu, inst.dst, src_val);
            }
            
            Opcode::Add => {
                // dst = dst + src, update flags
                let dst_val = self.emit_read_reg(builder, cpu, inst.dst);
                let src_val = self.emit_operand_value(builder, cpu, inst);
                
                builder.local_get(dst_val);
                builder.local_get(src_val);
                builder.i64_add();
                
                let result = builder.local_tee_new();
                
                self.emit_write_reg(builder, cpu, inst.dst, result);
                
                if inst.updates_flags {
                    self.emit_update_flags_add(builder, cpu, dst_val, src_val, result);
                }
            }
            
            Opcode::Jcc => {
                // Conditional branch - this ends the block
                let condition = self.emit_check_condition(builder, cpu, inst.condition);
                
                builder.local_get(condition);
                builder.if_(ValType::I64);
                    builder.i64_const(inst.target_address);
                builder.else_();
                    builder.i64_const(inst.next_address);
                builder.end();
            }
            
            // ... hundreds more opcodes
        }
    }
    
    fn emit_read_reg(&mut self, builder: &mut WasmBuilder, cpu: Local, reg: Register) -> Local {
        let offset = self.reg_offset(reg);
        builder.local_get(cpu);
        builder.i64_load(MemArg { offset, align: 3 });
        builder.local_tee_new()
    }
}
```

### Inlined Guest Memory Loads/Stores (TLB + RAM Fast Path)

Baseline JIT blocks must avoid an imported helper call per guest load/store. Instead, the code generator should inline address translation against a **JIT-visible TLB** (see [Memory Management](./03-memory-management.md#jit-visible-tlb-baseline-jit-memory-fast-path)) and directly `load/store` the guest RAM region in WASM linear memory.

The high-level strategy for each IR memory op is:

1. Compute effective `vaddr`
2. Attempt a direct-mapped TLB lookup (inline loads from the `JitTlb` struct)
3. On hit + `IS_RAM`: perform a direct WASM `load/store` at `ram_base + paddr`
4. On hit but non-RAM (MMIO/ROM/unmapped): exit the block via `jit_exit_mmio(...)`
5. On miss or permission mismatch: call `mmu_translate_slow(vaddr, access)` and continue (or raise `#PF`)

#### Codegen sketch

```rust
impl WasmCompiler {
    fn emit_load_u64(&mut self, b: &mut WasmBuilder, cpu: Local, vaddr: Local) -> Local {
        // (1) Optional cross-page guard. If the load crosses a 4KB boundary, go slow-path.
        // if ((vaddr & 0xFFF) > 0xFFF - 7) => slow
        self.emit_cross_page_guard(b, vaddr, 8);

        // (2) Fast-path translate: returns (phys_base_and_flags, hit?)
        let tlb_res = self.emit_tlb_lookup(b, cpu, vaddr, AccessType::Read);

        // (3) On RAM, do the direct memory load
        // paddr = (phys_base & !0xFFF) | (vaddr & 0xFFF)
        // wasm_addr = RAM_BASE + paddr
        let val = self.emit_direct_ram_load(b, tlb_res, vaddr, 8);

        // (4) Otherwise, `emit_direct_ram_load` will have emitted a `jit_exit_mmio`
        // or fallen back to `mmu_translate_slow`.
        val
    }

    fn emit_store_u64(&mut self, b: &mut WasmBuilder, cpu: Local, vaddr: Local, value: Local) {
        self.emit_cross_page_guard(b, vaddr, 8);

        let tlb_res = self.emit_tlb_lookup(b, cpu, vaddr, AccessType::Write);
        self.emit_direct_ram_store(b, tlb_res, vaddr, value, 8);

        // If the translation is non-RAM, the store exits to the runtime for MMIO.
        // If the store targets a CODE_WATCH page, the runtime invalidation hook runs
        // (either via an exit or a lightweight notification helper).
    }
}
```

The important point is that the **common case** (TLB hit + RAM) should compile down to:

- a handful of integer ops (`shr`, `and`, `xor`, `add/or`)
- two linear memory loads for the TLB entry
- the final linear memory `load/store` for guest RAM

…with no imported calls.

### SIMD Optimization

For SSE/AVX instructions, we use WASM SIMD:

```rust
fn emit_sse_addps(&mut self, builder: &mut WasmBuilder, dst: XmmReg, src: XmmReg) {
    // Load 128-bit registers as v128
    let dst_vec = self.emit_read_xmm(builder, dst);
    let src_vec = self.emit_read_xmm(builder, src);
    
    // Use WASM SIMD f32x4 addition
    builder.local_get(dst_vec);
    builder.local_get(src_vec);
    builder.f32x4_add();
    
    // Store result
    self.emit_write_xmm(builder, dst);
}
```

---

## Profiling and Hot Path Detection

```rust
pub struct ProfileCollector {
    execution_counts: HashMap<u64, u64>,  // address -> count
    branch_targets: HashMap<u64, Vec<u64>>,  // branch -> targets
    call_graph: HashMap<u64, Vec<u64>>,  // caller -> callees
}

impl ProfileCollector {
    pub fn record_execution(&mut self, address: u64) {
        *self.execution_counts.entry(address).or_default() += 1;
        
        // Trigger Tier 1 compilation at threshold
        if self.execution_counts[&address] == TIER1_THRESHOLD {
            self.request_tier1_compile(address);
        }
        
        // Trigger Tier 2 compilation at higher threshold
        if self.execution_counts[&address] == TIER2_THRESHOLD {
            self.request_tier2_compile(address);
        }
    }
    
    pub fn identify_hot_regions(&self) -> Vec<HotRegion> {
        // Use profile data to identify optimization opportunities
        // - Frequently executed loops
        // - Hot function traces
        // - Polymorphic call sites
        
        let mut regions = Vec::new();
        
        for (&addr, &count) in &self.execution_counts {
            if count >= TIER2_THRESHOLD {
                regions.push(HotRegion {
                    entry: addr,
                    execution_count: count,
                    trace: self.build_trace_from(addr),
                });
            }
        }
        
        regions.sort_by_key(|r| std::cmp::Reverse(r.execution_count));
        regions
    }
}
```

---

## Testing Strategy

### Instruction Tests

We use automated test generation from x86 reference:

```rust
#[test]
fn test_add_reg_reg() {
    let mut cpu = CpuEmulator::new();
    
    // ADD RAX, RBX
    cpu.set_reg(Register::RAX, 0x1234);
    cpu.set_reg(Register::RBX, 0x5678);
    cpu.execute_bytes(&[0x48, 0x01, 0xD8]);  // ADD RAX, RBX
    
    assert_eq!(cpu.get_reg(Register::RAX), 0x68AC);
    assert_eq!(cpu.get_flag(Flag::ZF), false);
    assert_eq!(cpu.get_flag(Flag::CF), false);
}

#[test]
fn test_add_overflow() {
    let mut cpu = CpuEmulator::new();
    
    cpu.set_reg(Register::RAX, 0x7FFFFFFFFFFFFFFF);
    cpu.set_reg(Register::RBX, 1);
    cpu.execute_bytes(&[0x48, 0x01, 0xD8]);
    
    assert_eq!(cpu.get_reg(Register::RAX), 0x8000000000000000);
    assert_eq!(cpu.get_flag(Flag::OF), true);  // Signed overflow
    assert_eq!(cpu.get_flag(Flag::SF), true);  // Sign flag set
}
```

### Conformance Testing

Compare against real hardware/QEMU:

```rust
#[test]
fn conformance_test_syscall() {
    let test_code = assemble("
        mov rax, 60     ; exit syscall
        mov rdi, 42     ; exit code
        syscall
    ");
    
    let aero_result = run_in_aero(&test_code);
    let qemu_result = run_in_qemu(&test_code);
    
    assert_eq!(aero_result.exit_code, qemu_result.exit_code);
    assert_eq!(aero_result.final_regs, qemu_result.final_regs);
}
```

---

## Performance Benchmarks

Target performance on modern hardware (2024 desktop):

| Benchmark | Target | Measurement |
|-----------|--------|-------------|
| Dhrystone | ≥ 500 DMIPS | Synthetic integer |
| Whetstone | ≥ 200 MWIPS | Synthetic floating point |
| Boot time | < 30s | Windows 7 to desktop |
| SPEC-like | ≥ 20% native | Mixed workloads |

For deterministic emulator-core throughput tracking (no OS images), use the guest CPU instruction throughput microbench suite (PF-008): [Guest CPU Instruction Throughput Benchmarks](./16-guest-cpu-benchmark-suite.md).

---

## Next Steps

- See [Memory Management](./03-memory-management.md) for paging/TLB details
- See [Performance Optimization](./10-performance-optimization.md) for advanced JIT techniques
- See [Task Breakdown](./15-agent-task-breakdown.md) for implementation details
