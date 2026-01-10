#[derive(Debug, Clone)]
pub struct AudioWorkletRingBuffer {
    buf: Vec<f32>,
    read_pos: usize,
    write_pos: usize,
    len: usize,
}

impl AudioWorkletRingBuffer {
    pub fn new(capacity_samples: usize) -> Self {
        assert!(capacity_samples > 0);
        Self {
            buf: vec![0.0; capacity_samples],
            read_pos: 0,
            write_pos: 0,
            len: 0,
        }
    }

    pub fn capacity_samples(&self) -> usize {
        self.buf.len()
    }

    pub fn len_samples(&self) -> usize {
        self.len
    }

    pub fn free_samples(&self) -> usize {
        self.capacity_samples() - self.len
    }

    pub fn push_samples(&mut self, samples: &[f32]) -> usize {
        let write_len = samples.len().min(self.free_samples());
        if write_len == 0 {
            return 0;
        }

        let first_chunk = write_len.min(self.capacity_samples() - self.write_pos);
        self.buf[self.write_pos..self.write_pos + first_chunk]
            .copy_from_slice(&samples[..first_chunk]);
        self.write_pos = (self.write_pos + first_chunk) % self.capacity_samples();

        let second_chunk = write_len - first_chunk;
        if second_chunk > 0 {
            self.buf[self.write_pos..self.write_pos + second_chunk]
                .copy_from_slice(&samples[first_chunk..first_chunk + second_chunk]);
            self.write_pos = (self.write_pos + second_chunk) % self.capacity_samples();
        }

        self.len += write_len;
        write_len
    }

    pub fn pop_samples(&mut self, out: &mut [f32]) -> usize {
        let read_len = out.len().min(self.len);
        if read_len == 0 {
            return 0;
        }

        let first_chunk = read_len.min(self.capacity_samples() - self.read_pos);
        out[..first_chunk].copy_from_slice(&self.buf[self.read_pos..self.read_pos + first_chunk]);
        self.read_pos = (self.read_pos + first_chunk) % self.capacity_samples();

        let second_chunk = read_len - first_chunk;
        if second_chunk > 0 {
            out[first_chunk..first_chunk + second_chunk]
                .copy_from_slice(&self.buf[self.read_pos..self.read_pos + second_chunk]);
            self.read_pos = (self.read_pos + second_chunk) % self.capacity_samples();
        }

        self.len -= read_len;
        read_len
    }
}

pub trait AudioSink {
    fn push_interleaved_f32(&mut self, samples: &[f32]) -> usize;
}

impl AudioSink for AudioWorkletRingBuffer {
    fn push_interleaved_f32(&mut self, samples: &[f32]) -> usize {
        self.push_samples(samples)
    }
}

#[cfg(test)]
mod tests {
    use super::AudioWorkletRingBuffer;

    #[test]
    fn ring_buffer_wraps_and_tracks_length() {
        let mut rb = AudioWorkletRingBuffer::new(4);
        assert_eq!(rb.push_samples(&[1.0, 2.0, 3.0]), 3);
        assert_eq!(rb.len_samples(), 3);

        let mut out = [0.0; 2];
        assert_eq!(rb.pop_samples(&mut out), 2);
        assert_eq!(out, [1.0, 2.0]);
        assert_eq!(rb.len_samples(), 1);

        assert_eq!(rb.push_samples(&[4.0, 5.0, 6.0]), 3);
        assert_eq!(rb.len_samples(), 4);

        let mut out = [0.0; 4];
        assert_eq!(rb.pop_samples(&mut out), 4);
        assert_eq!(out, [3.0, 4.0, 5.0, 6.0]);
        assert_eq!(rb.len_samples(), 0);
    }
}
