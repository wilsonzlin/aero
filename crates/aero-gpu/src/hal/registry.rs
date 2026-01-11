use core::marker::PhantomData;

use crate::hal::handles::Handle;
use crate::GpuError;

struct Slot<T> {
    generation: u32,
    value: Option<T>,
}

/// Handle-based storage with generation validation.
///
/// Backends should store all GPU resources in registries so that higher-level code can only refer
/// to resources using opaque IDs. When a resource is removed, the generation is incremented so
/// stale handles cannot be reused accidentally.
pub struct ResourceRegistry<Tag, T> {
    kind: &'static str,
    slots: Vec<Slot<T>>,
    free_list: Vec<u32>,
    _tag: PhantomData<Tag>,
}

impl<Tag, T> ResourceRegistry<Tag, T> {
    pub fn new(kind: &'static str) -> Self {
        Self {
            kind,
            slots: Vec::new(),
            free_list: Vec::new(),
            _tag: PhantomData,
        }
    }

    pub fn insert(&mut self, value: T) -> Handle<Tag> {
        if let Some(index) = self.free_list.pop() {
            let slot = &mut self.slots[index as usize];
            debug_assert!(slot.value.is_none(), "free_list points at a live slot");
            slot.value = Some(value);
            Handle::new(index, slot.generation)
        } else {
            let index = self.slots.len() as u32;
            self.slots.push(Slot {
                generation: 0,
                value: Some(value),
            });
            Handle::new(index, 0)
        }
    }

    pub fn get(&self, id: Handle<Tag>) -> Result<&T, GpuError> {
        let kind = self.kind;
        let index = id.index();
        let generation = id.generation();
        let slot = self.get_slot(id)?;
        slot.value.as_ref().ok_or(GpuError::InvalidHandle {
            kind,
            index,
            generation,
        })
    }

    pub fn get_mut(&mut self, id: Handle<Tag>) -> Result<&mut T, GpuError> {
        let kind = self.kind;
        let index = id.index();
        let generation = id.generation();
        let slot = self.get_slot_mut(id)?;
        slot.value.as_mut().ok_or(GpuError::InvalidHandle {
            kind,
            index,
            generation,
        })
    }

    pub fn remove(&mut self, id: Handle<Tag>) -> Result<T, GpuError> {
        let kind = self.kind;
        let index = id.index();
        let generation = id.generation();
        let slot = self.get_slot_mut(id)?;
        let value = slot.value.take().ok_or(GpuError::InvalidHandle {
            kind,
            index,
            generation,
        })?;
        slot.generation = slot.generation.wrapping_add(1);
        self.free_list.push(index);
        Ok(value)
    }

    pub fn len(&self) -> usize {
        self.slots.len() - self.free_list.len()
    }

    fn get_slot(&self, id: Handle<Tag>) -> Result<&Slot<T>, GpuError> {
        self.slots
            .get(id.index() as usize)
            .ok_or(GpuError::InvalidHandle {
                kind: self.kind,
                index: id.index(),
                generation: id.generation(),
            })
            .and_then(|slot| {
                if slot.generation != id.generation() {
                    return Err(GpuError::InvalidHandle {
                        kind: self.kind,
                        index: id.index(),
                        generation: id.generation(),
                    });
                }
                Ok(slot)
            })
    }

    fn get_slot_mut(&mut self, id: Handle<Tag>) -> Result<&mut Slot<T>, GpuError> {
        self.slots
            .get_mut(id.index() as usize)
            .ok_or(GpuError::InvalidHandle {
                kind: self.kind,
                index: id.index(),
                generation: id.generation(),
            })
            .and_then(|slot| {
                if slot.generation != id.generation() {
                    return Err(GpuError::InvalidHandle {
                        kind: self.kind,
                        index: id.index(),
                        generation: id.generation(),
                    });
                }
                Ok(slot)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::ResourceRegistry;

    #[derive(Debug)]
    enum FooTag {}

    #[test]
    fn remove_invalidates_old_handles() {
        let mut registry = ResourceRegistry::<FooTag, u32>::new("foo");

        let a = registry.insert(1);
        assert_eq!(*registry.get(a).unwrap(), 1);

        assert_eq!(registry.remove(a).unwrap(), 1);
        assert!(registry.get(a).is_err());

        let b = registry.insert(2);
        assert_eq!(a.index(), b.index(), "slot should be reused");
        assert_ne!(
            a.generation(),
            b.generation(),
            "generation must change when slot is reused"
        );

        assert!(registry.get(a).is_err());
        assert_eq!(*registry.get(b).unwrap(), 2);
    }

    #[test]
    fn out_of_bounds_is_invalid() {
        let registry = ResourceRegistry::<FooTag, u32>::new("foo");
        let bogus = crate::hal::handles::Handle::<FooTag>::new(999, 0);
        assert!(registry.get(bogus).is_err());
    }
}
