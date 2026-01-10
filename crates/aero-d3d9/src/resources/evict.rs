use hashbrown::HashMap;
use std::collections::VecDeque;
use std::hash::Hash;

/// Small LRU helper used for budget-based resource eviction.
///
/// This is not currently wired into every resource type, but provides a shared primitive for
/// places where we need a deterministic "least recently used" ordering.
#[derive(Debug)]
pub struct Lru<K> {
    order: VecDeque<K>,
    index: HashMap<K, usize>,
}

impl<K> Lru<K>
where
    K: Clone + Eq + Hash,
{
    pub fn new() -> Self {
        Self {
            order: VecDeque::new(),
            index: HashMap::new(),
        }
    }

    pub fn touch(&mut self, key: &K) {
        if let Some(&pos) = self.index.get(key) {
            self.order.remove(pos);
            self.rebuild_index();
        }
        self.order.push_back(key.clone());
        self.rebuild_index();
    }

    pub fn pop_lru(&mut self) -> Option<K> {
        let key = self.order.pop_front()?;
        self.rebuild_index();
        Some(key)
    }

    pub fn remove(&mut self, key: &K) -> bool {
        let Some(&pos) = self.index.get(key) else {
            return false;
        };
        self.order.remove(pos);
        self.rebuild_index();
        true
    }

    fn rebuild_index(&mut self) {
        self.index.clear();
        for (i, k) in self.order.iter().enumerate() {
            self.index.insert(k.clone(), i);
        }
    }
}

impl<K> Default for Lru<K>
where
    K: Clone + Eq + Hash,
{
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::Lru;

    #[test]
    fn lru_orders_keys() {
        let mut lru = Lru::new();
        lru.touch(&1);
        lru.touch(&2);
        lru.touch(&3);
        lru.touch(&2);

        assert_eq!(lru.pop_lru(), Some(1));
        assert_eq!(lru.pop_lru(), Some(3));
        assert_eq!(lru.pop_lru(), Some(2));
        assert_eq!(lru.pop_lru(), None);
    }
}
