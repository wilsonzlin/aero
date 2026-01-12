use std::sync::Mutex;

/// A simple thread-safe queue with drain semantics.
///
/// This is kept dependency-free (std-only) so it can be unit tested on the host
/// and used by the wasm32 build for GPU diagnostics event forwarding.
#[derive(Debug)]
pub struct DrainQueue<T> {
    inner: Mutex<Vec<T>>,
}

impl<T> Default for DrainQueue<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> DrainQueue<T> {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Vec::new()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<T>> {
        match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    pub fn push(&self, item: T) {
        self.lock().push(item);
    }

    /// Push an item, truncating the queue to at most `max_len` by dropping the
    /// oldest items.
    ///
    /// If `max_len` is 0, the item is dropped.
    pub fn push_bounded(&self, item: T, max_len: usize) {
        if max_len == 0 {
            return;
        }
        let mut guard = self.lock();
        if guard.len() >= max_len {
            // Keep the most recent items. This is O(n) but expected to be cheap
            // given the small bounded size and low event frequency.
            let drop_count = guard.len() + 1 - max_len;
            guard.drain(0..drop_count);
        }
        guard.push(item);
    }

    pub fn drain(&self) -> Vec<T> {
        std::mem::take(&mut *self.lock())
    }
}

#[cfg(test)]
mod tests {
    use super::DrainQueue;
    use std::sync::Arc;

    #[test]
    fn drain_queue_drains_and_clears() {
        let q = DrainQueue::new();
        q.push(1);
        q.push(2);
        assert_eq!(q.drain(), vec![1, 2]);
        assert!(q.drain().is_empty());
        q.push(3);
        assert_eq!(q.drain(), vec![3]);
    }

    #[test]
    fn drain_queue_is_thread_safe() {
        let q = Arc::new(DrainQueue::new());
        let mut threads = Vec::new();
        for t in 0..4u32 {
            let q = Arc::clone(&q);
            threads.push(std::thread::spawn(move || {
                for i in 0..100u32 {
                    q.push((t * 1000) + i);
                }
            }));
        }

        for th in threads {
            th.join().expect("thread join");
        }

        let mut items = q.drain();
        items.sort_unstable();
        assert_eq!(items.len(), 400);
        // Spot check some expected elements.
        assert_eq!(items[0], 0);
        assert_eq!(items[1], 1);
        assert_eq!(items.last().copied(), Some(3000 + 99));
        assert!(q.drain().is_empty());
    }

    #[test]
    fn push_bounded_drops_oldest() {
        let q = DrainQueue::new();
        q.push_bounded(1, 2);
        q.push_bounded(2, 2);
        q.push_bounded(3, 2);
        assert_eq!(q.drain(), vec![2, 3]);
    }
}
