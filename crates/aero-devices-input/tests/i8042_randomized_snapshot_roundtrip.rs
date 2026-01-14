use aero_devices_input::{I8042Controller, Ps2MouseButton};
use aero_io_snapshot::io::state::IoSnapshot;

/// A tiny deterministic PRNG (SplitMix64).
///
/// We keep this local so the test doesn't need any extra dependencies.
#[derive(Clone)]
struct Rng {
    state: u64,
}

impl Rng {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next_u64(&mut self) -> u64 {
        // splitmix64: https://prng.di.unimi.it/splitmix64.c
        self.state = self.state.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    }

    fn next_u8(&mut self) -> u8 {
        self.next_u64() as u8
    }

    fn gen_range_usize(&mut self, start: usize, end: usize) -> usize {
        assert!(start < end);
        start + (self.next_u64() as usize % (end - start))
    }

    fn gen_range_i32(&mut self, min: i32, max: i32) -> i32 {
        assert!(min <= max);
        let span = (max as i64 - min as i64 + 1) as u64;
        (min as i64 + (self.next_u64() % span) as i64) as i32
    }
}

#[derive(Debug, Clone)]
enum Op {
    WriteCmd(u8),
    WriteData(u8),
    ReadData,
    ReadStatus,
    InjectKey { bytes: Vec<u8> },
    InjectMouseMotion { dx: i32, dy: i32, wheel: i32 },
    SetMouseButtons { mask: u8 },
    Reset,
}

fn gen_controller_cmd(rng: &mut Rng) -> u8 {
    // Heavily bias toward "interesting" i8042 commands but keep some truly random coverage.
    const CMDS: &[u8] = &[
        0x20, // read command byte
        0x60, // write command byte (pending data)
        0xA7, // disable mouse
        0xA8, // enable mouse
        0xA9, // test mouse
        0xAA, // self-test
        0xAB, // test keyboard
        0xAD, // disable keyboard
        0xAE, // enable keyboard
        0xD0, // read output port
        0xD1, // write output port (pending data)
        0xD2, // write output buffer (kbd source) (pending data)
        0xD3, // write output buffer (mouse source) (pending data)
        0xD4, // write to mouse (pending data)
        0xDD, // disable A20 (non-standard)
        0xDF, // enable A20 (non-standard)
        0xFE, // pulse reset (to sysctrl, if attached)
    ];

    if rng.next_u8() < 230 {
        CMDS[rng.gen_range_usize(0, CMDS.len())]
    } else {
        rng.next_u8()
    }
}

fn gen_data_byte(rng: &mut Rng) -> u8 {
    // A mix of common keyboard + mouse commands and random bytes. Many of these are
    // multi-byte sequences that set "expecting data" state, which is valuable for
    // snapshot coverage.
    const DATA: &[u8] = &[
        // Keyboard commands.
        0xED, // set LEDs (expect data)
        0xEE, // echo
        0xF0, // set scancode set (expect data)
        0xF2, // identify
        0xF3, // set typematic rate/delay (expect data)
        0xF4, // enable scanning
        0xF5, // disable scanning
        0xF6, // set defaults
        0xFF, // reset
        // Mouse commands.
        0xE6, 0xE7, 0xE8, 0xE9, 0xEA, 0xEB, 0xF0, 0xF2, 0xF3, 0xF4, 0xF5, 0xF6, 0xFF,
        // Common data values used by those commands.
        0x00,
        0x01,
        0x02,
        0x03,
        0x04,
        0x20,
        0x40,
        0x80,
        0xA5,
        0xAA,
        0x55,
        0xED,
    ];

    if rng.next_u8() < 220 {
        DATA[rng.gen_range_usize(0, DATA.len())]
    } else {
        rng.next_u8()
    }
}

fn gen_scancode_bytes(rng: &mut Rng) -> Vec<u8> {
    // Common Set-2 make codes.
    const MAKE: &[u8] = &[
        0x1C, // A
        0x32, // B
        0x21, // C
        0x23, // D
        0x24, // E
        0x2B, // F
        0x34, // G
        0x33, // H
        0x43, // I
        0x3B, // J
        0x42, // K
        0x4B, // L
        0x3A, // M
        0x31, // N
        0x44, // O
        0x4D, // P
        0x15, // Q
        0x2D, // R
        0x1B, // S
        0x2C, // T
        0x3C, // U
        0x2A, // V
        0x1D, // W
        0x22, // X
        0x35, // Y
        0x1A, // Z
        0x29, // Space
        0x76, // Esc
        0x5A, // Enter
    ];
    const EXT: &[u8] = &[
        0x75, // Up
        0x72, // Down
        0x6B, // Left
        0x74, // Right
        0x70, // Insert
        0x71, // Delete
        0x6C, // Home
        0x69, // End
        0x7D, // PageUp
        0x7A, // PageDown
    ];

    // Prefer generating "interesting" sequences that can leave the Set2->Set1
    // translator in a non-default intermediate state (E0/F0 prefixes).
    match rng.next_u8() % 10 {
        0 => vec![MAKE[rng.gen_range_usize(0, MAKE.len())]],
        1 => vec![0xF0, MAKE[rng.gen_range_usize(0, MAKE.len())]],
        2 => vec![0xE0, EXT[rng.gen_range_usize(0, EXT.len())]],
        3 => vec![0xE0, 0xF0, EXT[rng.gen_range_usize(0, EXT.len())]],
        4 => vec![0xE0], // leave saw_e0=true
        5 => vec![0xF0], // leave saw_f0=true
        6 => vec![0xE0, 0xF0], // leave saw_e0 + saw_f0
        7 => vec![0xE1], // Pause/Break prefix
        8 => vec![rng.next_u8(), rng.next_u8()],
        _ => {
            let len = rng.gen_range_usize(1, 5);
            (0..len).map(|_| rng.next_u8()).collect()
        }
    }
}

fn apply_mouse_buttons(c: &mut I8042Controller, mask: u8) {
    let pressed = |bit: u8| (mask & bit) != 0;
    c.inject_mouse_button(Ps2MouseButton::Left, pressed(0x01));
    c.inject_mouse_button(Ps2MouseButton::Right, pressed(0x02));
    c.inject_mouse_button(Ps2MouseButton::Middle, pressed(0x04));
    c.inject_mouse_button(Ps2MouseButton::Side, pressed(0x08));
    c.inject_mouse_button(Ps2MouseButton::Extra, pressed(0x10));
}

fn gen_op(rng: &mut Rng) -> Op {
    match rng.next_u8() % 100 {
        0..=19 => Op::WriteCmd(gen_controller_cmd(rng)),
        20..=39 => Op::WriteData(gen_data_byte(rng)),
        40..=54 => Op::ReadData,
        55..=64 => Op::ReadStatus,
        65..=82 => Op::InjectKey {
            bytes: gen_scancode_bytes(rng),
        },
        83..=92 => Op::InjectMouseMotion {
            dx: rng.gen_range_i32(-200, 200),
            dy: rng.gen_range_i32(-200, 200),
            wheel: rng.gen_range_i32(-16, 16),
        },
        93..=96 => Op::SetMouseButtons {
            mask: rng.next_u8() & 0x1F,
        },
        _ => Op::Reset,
    }
}

fn apply_op(c: &mut I8042Controller, op: &Op) -> Option<u8> {
    match op {
        Op::WriteCmd(cmd) => {
            c.write_port(0x64, *cmd);
            None
        }
        Op::WriteData(value) => {
            c.write_port(0x60, *value);
            None
        }
        Op::ReadData => Some(c.read_port(0x60)),
        Op::ReadStatus => Some(c.read_port(0x64)),
        Op::InjectKey { bytes } => {
            c.inject_key_scancode_bytes(bytes);
            None
        }
        Op::InjectMouseMotion { dx, dy, wheel } => {
            c.inject_mouse_motion(*dx, *dy, *wheel);
            None
        }
        Op::SetMouseButtons { mask } => {
            apply_mouse_buttons(c, *mask);
            None
        }
        Op::Reset => {
            c.reset();
            None
        }
    }
}

#[test]
fn i8042_randomized_snapshot_restore_produces_equivalent_controller() {
    const SEED: u64 = 0x00C0_FFEE_8042_1234;
    const STEPS: usize = 10_000;
    const CHECKPOINTS: usize = 3;

    let mut rng = Rng::new(SEED);

    // Pick deterministic-but-random checkpoints across the run so each checkpoint has meaningful
    // "before" and "after" coverage.
    let mut checkpoint_steps = Vec::with_capacity(CHECKPOINTS);
    checkpoint_steps.push(rng.gen_range_usize(100, STEPS / 3));
    checkpoint_steps.push(rng.gen_range_usize(STEPS / 3, 2 * STEPS / 3));
    checkpoint_steps.push(rng.gen_range_usize(2 * STEPS / 3, STEPS - 1));
    checkpoint_steps.sort_unstable();
    checkpoint_steps.dedup();

    let mut next_checkpoint_idx = 0usize;

    let mut a = I8042Controller::new();
    let mut b: Option<I8042Controller> = None;

    for step in 0..STEPS {
        if next_checkpoint_idx < checkpoint_steps.len()
            && step == checkpoint_steps[next_checkpoint_idx]
        {
            if let Some(ref mut b_ref) = b {
                assert_eq!(
                    a.save_state(),
                    b_ref.save_state(),
                    "seed={SEED:#x} step={step} pre-checkpoint state mismatch"
                );
            }

            let snap = a.save_state();
            let mut restored = I8042Controller::new();
            restored
                .load_state(&snap)
                .expect("snapshot restore should succeed");
            assert_eq!(
                snap,
                restored.save_state(),
                "seed={SEED:#x} step={step} snapshot->restore->snapshot mismatch"
            );
            b = Some(restored);
            next_checkpoint_idx += 1;
        }

        let op = gen_op(&mut rng);

        if let Some(b_ref) = b.as_mut() {
            let ra = apply_op(&mut a, &op);
            let rb = apply_op(b_ref, &op);

            assert_eq!(
                ra, rb,
                "seed={SEED:#x} step={step} op={op:?} read mismatch"
            );
            assert_eq!(
                a.irq1_level(),
                b_ref.irq1_level(),
                "seed={SEED:#x} step={step} op={op:?} irq1_level mismatch"
            );
            assert_eq!(
                a.irq12_level(),
                b_ref.irq12_level(),
                "seed={SEED:#x} step={step} op={op:?} irq12_level mismatch"
            );
            assert_eq!(
                a.mouse_buttons_mask(),
                b_ref.mouse_buttons_mask(),
                "seed={SEED:#x} step={step} op={op:?} mouse_buttons_mask mismatch"
            );
            assert_eq!(
                a.read_port(0x64),
                b_ref.read_port(0x64),
                "seed={SEED:#x} step={step} op={op:?} status mismatch"
            );

            // Periodically compare snapshots so failures have a shorter distance-to-signal.
            if step % 1024 == 0 {
                assert_eq!(
                    a.save_state(),
                    b_ref.save_state(),
                    "seed={SEED:#x} step={step} op={op:?} periodic snapshot mismatch"
                );
            }
        } else {
            let _ = apply_op(&mut a, &op);
        }
    }

    let b_final = b.expect("test should have created at least one checkpoint controller");
    assert_eq!(
        a.save_state(),
        b_final.save_state(),
        "seed={SEED:#x} final snapshot mismatch"
    );
}
