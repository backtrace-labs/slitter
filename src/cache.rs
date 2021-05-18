//! Slitter stashes allocations for each size class in a thread-local
//! cache.
use smallvec::SmallVec;
use std::cell::RefCell;
use std::ffi::c_void;
use std::num::NonZeroU32;
use std::ptr::NonNull;

use crate::Class;

/// For each allocation class, we cache up to one allocation.
struct ClassCache {
    slot: Option<NonNull<c_void>>,
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
pub fn allocate(class: Class) -> Option<NonNull<c_void>> {
    CACHE
        .try_with(|cache| cache.borrow_mut().allocate(class))
        .unwrap_or_else(|_| class.allocate_slow())
}

#[inline(always)]
pub fn release(class: Class, block: NonNull<c_void>) {
    CACHE
        .try_with(|cache| cache.borrow_mut().release(class, block))
        .unwrap_or_else(|_| class.release_slow(block))
}

impl Drop for Cache {
    fn drop(&mut self) {
        for (index, cache) in self.per_class.iter_mut().enumerate() {
            if let Some(alloc) = cache.slot.take() {
                Class::from_id(
                    NonZeroU32::new(index as u32).expect("populated index must be positive"),
                )
                .expect("class must exist")
                .release_slow(alloc);
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
        self.per_class
            .resize_with(max_id + 1, || ClassCache { slot: None });
    }

    /// Attempts to return an allocation for `class`.  Consumes from
    /// the cache if possible, and hits the class's slow path otherwise.
    #[inline(always)]
    fn allocate(&mut self, class: Class) -> Option<NonNull<c_void>> {
        let index = class.id().get() as usize;

        if self.per_class.len() <= index {
            self.grow();
        }

        self.per_class[index]
            .slot
            .take()
            .or_else(|| class.allocate_slow())
    }

    /// Marks `block`, an allocation for `class`, ready for reuse.
    /// Pushes to the cache if possible, and hits the class's slow
    /// path otherwise.
    #[inline(always)]
    fn release(&mut self, class: Class, block: NonNull<c_void>) {
        let index = class.id().get() as usize;

        if self.per_class.len() <= index {
            assert!(index < u32::MAX as usize);
            self.grow();
        }

        // We prefer to cache freshly deallocated objects, for
        // temporal locality.
        if let Some(old) = self.per_class[index].slot.replace(block) {
            class.release_slow(old);
        }
    }
}
