use aero_ipc::ring::{record_size, PopError, PushError, RingBuffer};
use std::collections::VecDeque;
use std::sync::Arc;

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next_u32(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        ((x.wrapping_mul(0x2545F4914F6CDD1D)) >> 32) as u32
    }

    fn gen_range(&mut self, max_exclusive: u32) -> u32 {
        if max_exclusive == 0 {
            return 0;
        }
        self.next_u32() % max_exclusive
    }

    fn fill_bytes(&mut self, buf: &mut [u8]) {
        for b in buf {
            *b = (self.next_u32() & 0xFF) as u8;
        }
    }
}

#[test]
fn ring_buffer_single_thread_fuzz() {
    // Tiny capacity to force wraparound and full-buffer behaviour.
    let rb = RingBuffer::new(256);
    let mut model: VecDeque<Vec<u8>> = VecDeque::new();

    let mut rng = Rng::new(0x1234_5678_9ABC_DEF0);
    for _ in 0..50_000 {
        let op = rng.gen_range(3);
        match op {
            0 => {
                // push
                let len = rng.gen_range(64) as usize;
                let mut msg = vec![0u8; len];
                rng.fill_bytes(&mut msg);
                match rb.try_push(&msg) {
                    Ok(()) => model.push_back(msg),
                    Err(PushError::Full) => {}
                    Err(PushError::TooLarge) => panic!("unexpected TooLarge"),
                }
            }
            1 => {
                // pop
                match rb.try_pop() {
                    Ok(v) => {
                        let expected = model.pop_front().expect("model has data");
                        assert_eq!(v, expected);
                    }
                    Err(PopError::Empty) => assert!(model.is_empty()),
                    Err(PopError::Corrupt) => panic!("corrupt"),
                }
            }
            _ => {
                // mixed: push then pop (helps exercise immediate wrap marker cases)
                let len = rng.gen_range(32) as usize;
                let mut msg = vec![0u8; len];
                rng.fill_bytes(&mut msg);
                if rb.try_push(&msg).is_ok() {
                    model.push_back(msg);
                }
                if let Ok(v) = rb.try_pop() {
                    let expected = model.pop_front().expect("model has data");
                    assert_eq!(v, expected);
                }
            }
        }
    }

    while let Ok(v) = rb.try_pop() {
        let expected = model.pop_front().expect("model has data");
        assert_eq!(v, expected);
    }
    assert!(model.is_empty());
}

#[test]
fn ring_buffer_spsc_concurrent() {
    let rb = Arc::new(RingBuffer::new(512));
    let producer = rb.clone();
    let consumer = rb.clone();

    const N: u32 = 100_000;

    let t_prod = std::thread::spawn(move || {
        let mut buf = [0u8; 4];
        for i in 0..N {
            buf.copy_from_slice(&i.to_le_bytes());
            producer.push_spinning(&buf);
        }
    });

    let t_cons = std::thread::spawn(move || {
        for i in 0..N {
            let msg = consumer.pop_spinning();
            let got = u32::from_le_bytes(msg[..4].try_into().unwrap());
            assert_eq!(got, i);
        }
    });

    t_prod.join().unwrap();
    t_cons.join().unwrap();
    assert!(rb.is_empty());
}

#[test]
fn ring_buffer_mpsc_concurrent() {
    let rb = Arc::new(RingBuffer::new(1024));

    // Avoid spawning excessive OS threads in constrained CI environments. Two producers are still
    // sufficient to exercise the multi-producer code paths.
    const PRODUCERS: usize = 2;
    const PER_PRODUCER: u32 = 50_000;

    let mut handles = Vec::new();
    for pid in 0..PRODUCERS {
        let rb = rb.clone();
        handles.push(std::thread::spawn(move || {
            let mut buf = [0u8; 8];
            for seq in 0..PER_PRODUCER {
                buf[..4].copy_from_slice(&(pid as u32).to_le_bytes());
                buf[4..].copy_from_slice(&seq.to_le_bytes());
                rb.push_spinning(&buf);
            }
        }));
    }

    let total = PRODUCERS as u32 * PER_PRODUCER;
    let mut seen = vec![vec![false; PER_PRODUCER as usize]; PRODUCERS];

    for _ in 0..total {
        let msg = rb.pop_spinning();
        let pid = u32::from_le_bytes(msg[..4].try_into().unwrap()) as usize;
        let seq = u32::from_le_bytes(msg[4..8].try_into().unwrap()) as usize;
        assert!(pid < PRODUCERS);
        assert!(seq < PER_PRODUCER as usize);
        assert!(!seen[pid][seq], "duplicate delivery");
        seen[pid][seq] = true;
    }

    for h in handles {
        h.join().unwrap();
    }

    for (pid, rows) in seen.iter().enumerate() {
        for (seq, v) in rows.iter().enumerate() {
            assert!(*v, "missing pid={pid} seq={seq}");
        }
    }
    assert!(rb.is_empty());
}

#[test]
fn ring_buffer_rejects_too_large_record() {
    let rb = RingBuffer::new(64);
    // Guaranteed too large because even the header cannot fit for a big payload.
    let payload_len = 10_000;
    let payload = vec![0u8; payload_len];
    assert_eq!(rb.try_push(&payload), Err(PushError::TooLarge));
    assert!(record_size(payload_len) > rb.capacity_bytes());
}
