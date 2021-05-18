//! Slitter stashes allocations for each size class in a thread-local
//! cache.
use smallvec::SmallVec;
use std::cell::RefCell;
use std::num::NonZeroU32;

use crate::class::ClassInfo;
use crate::linear_ref::LinearRef;
use crate::magazine::Magazine;
use crate::Class;

/// For each allocation class, we cache up to one magazine's worth of
/// allocations.
struct ClassCache {
    mag: Box<Magazine>,

    // The `info` field is only `None` for the dummy entry we keep
    // around for the invalid "0" class id.
    info: Option<&'static ClassInfo>,
}

/// Inline the cache array for up to this many allocation classes.
const SMALL_CACHE_SIZE: usize = 3;

struct Cache {
    // This vector is directly indexed with the class id; the first
    // element at index 0 is thus never used, so we must increment
    // SMALL_CACHE_SIZE by 1.
    per_class: SmallVec<[ClassCache; SMALL_CACHE_SIZE + 1]>,
}

// TODO: keyed thread-local is slow.  We should `#![feature(thread_local)]`
// and `#[thread_local] static mut CACHE: ...` in nightly builds.  We'll
// still want a `thread_local!` to trigger cleanup...
thread_local!(static CACHE: RefCell<Cache> = RefCell::new(Cache::new()));

#[inline(always)]
pub fn allocate(class: Class) -> Option<LinearRef> {
    CACHE
        .try_with(|cache| cache.borrow_mut().allocate(class))
        .unwrap_or_else(|_| class.info().allocate_slow())
}

#[inline(always)]
pub fn release(class: Class, block: LinearRef) {
    let mut cell = Some(block);

    CACHE
        .try_with(|cache| cache.borrow_mut().release(class, cell.take().unwrap()))
        .unwrap_or_else(|_| {
            if let Some(alloc) = cell {
                class.info().release_slow(alloc)
            }
        })
}

impl Drop for Cache {
    fn drop(&mut self) {
        while let Some(slot) = self.per_class.pop() {
            let mag = slot.mag;

            if let Some(info) = slot.info {
                info.release_magazine(mag);
            } else {
                // This must be the padding slot at index 0.
                assert!(self.per_class.is_empty());
                crate::magazine::get_default_rack().release_empty_magazine(mag);
            }
        }
    }
}

impl Cache {
    fn new() -> Cache {
        Cache {
            per_class: SmallVec::new(),
        }
    }

    /// Ensures the cache's `per_class` array has one entry for every
    /// allocation class currently defined.
    #[cold]
    fn grow(&mut self) {
        let max_id = crate::class::max_id();
        if self.per_class.len() > max_id {
            return;
        }

        assert!(max_id <= u32::MAX as usize);
        while self.per_class.len() <= max_id {
            let id = NonZeroU32::new(self.per_class.len() as u32);
            let info = id.and_then(|id| Class::from_id(id).map(|class| class.info()));

            let mag = if let Some(i) = info {
                i.allocate_magazine()
            } else {
                crate::magazine::get_default_rack().allocate_empty_magazine()
            };

            let slot = ClassCache { mag, info };

            self.per_class.push(slot)
        }
    }

    /// Attempts to return an allocation for `class`.  Consumes from
    /// the cache if possible, and hits the Class(Info)'s slow path
    /// otherwise.
    #[inline(always)]
    fn allocate(&mut self, class: Class) -> Option<LinearRef> {
        let index = class.id().get() as usize;

        if self.per_class.len() <= index {
            self.grow();
        }

        let entry = &mut self.per_class[index];
        if let Some(alloc) = entry.mag.get() {
            return Some(alloc);
        }

        entry
            .info
            .expect("must have class info")
            .refill_magazine(&mut entry.mag)
    }

    /// Marks `block`, an allocation for `class`, ready for reuse.
    /// Pushes to the cache if possible, and hits the Class(Info)'s
    /// slow path otherwise.
    #[inline(always)]
    fn release(&mut self, class: Class, block: LinearRef) {
        let index = class.id().get() as usize;

        if self.per_class.len() <= index {
            assert!(index < u32::MAX as usize);
            self.grow();
        }

        let entry = &mut self.per_class[index];
        // We prefer to cache freshly deallocated objects, for
        // temporal locality.
        if let Some(spill) = entry.mag.put(block) {
            entry
                .info
                .expect("must have class info")
                .clear_magazine(&mut entry.mag, spill);
        }
    }
}
