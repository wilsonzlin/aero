use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use aero_devices_input::i8042::IrqSink;
use aero_devices_input::I8042Controller;

#[derive(Clone)]
struct QueueIrqSink(Rc<RefCell<VecDeque<u8>>>);

impl IrqSink for QueueIrqSink {
    fn raise_irq(&mut self, irq: u8) {
        self.0.borrow_mut().push_back(irq);
    }
}

#[derive(Default)]
struct BiosKeyboard {
    shift: bool,
    queue: VecDeque<(u8, u8)>, // (ascii, scancode)
    saw_e0: bool,
}

impl BiosKeyboard {
    fn handle_irq1(&mut self, i8042: &mut I8042Controller) {
        // Real BIOS reads port 0x60 from the IRQ1 handler.
        let byte = i8042.read_port(0x60);

        if byte == 0xE0 {
            self.saw_e0 = true;
            return;
        }

        // Ignore extended codes for this minimal INT16 test.
        if self.saw_e0 {
            self.saw_e0 = false;
            return;
        }

        let is_break = byte & 0x80 != 0;
        let sc = byte & 0x7F;

        match sc {
            0x2A | 0x36 => {
                // Shift.
                self.shift = !is_break;
            }
            _ => {
                if is_break {
                    return;
                }
                if let Some(ascii) = set1_to_ascii(sc, self.shift) {
                    self.queue.push_back((ascii, sc));
                }
            }
        }
    }

    fn int16_read(&mut self) -> Option<(u8, u8)> {
        self.queue.pop_front()
    }
}

fn set1_to_ascii(sc: u8, shift: bool) -> Option<u8> {
    let c = match (sc, shift) {
        (0x1E, false) => b'a',
        (0x1E, true) => b'A',
        (0x30, false) => b'b',
        (0x30, true) => b'B',
        (0x39, _) => b' ',
        (0x1C, _) => b'\r',
        _ => return None,
    };
    Some(c)
}

#[test]
fn bios_int16_reads_keystrokes_from_irq1_handler() {
    let irqs = Rc::new(RefCell::new(VecDeque::new()));
    let mut i8042 = I8042Controller::new();
    i8042.set_irq_sink(Box::new(QueueIrqSink(irqs.clone())));

    let mut bios = BiosKeyboard::default();

    // Press and release 'a'.
    i8042.inject_browser_key("KeyA", true);
    i8042.inject_browser_key("KeyA", false);

    // Drain IRQ queue and run the IRQ1 handler.
    loop {
        let irq = { irqs.borrow_mut().pop_front() };
        let Some(irq) = irq else {
            break;
        };
        if irq == 1 {
            bios.handle_irq1(&mut i8042);
        }
    }

    assert_eq!(bios.int16_read(), Some((b'a', 0x1E)));
    assert_eq!(bios.int16_read(), None);
}

#[test]
fn bios_int16_honors_shift_state() {
    let irqs = Rc::new(RefCell::new(VecDeque::new()));
    let mut i8042 = I8042Controller::new();
    i8042.set_irq_sink(Box::new(QueueIrqSink(irqs.clone())));

    let mut bios = BiosKeyboard::default();

    i8042.inject_browser_key("ShiftLeft", true);
    i8042.inject_browser_key("KeyA", true);
    i8042.inject_browser_key("KeyA", false);
    i8042.inject_browser_key("ShiftLeft", false);

    loop {
        let irq = { irqs.borrow_mut().pop_front() };
        let Some(irq) = irq else {
            break;
        };
        if irq == 1 {
            bios.handle_irq1(&mut i8042);
        }
    }

    assert_eq!(bios.int16_read(), Some((b'A', 0x1E)));
}
