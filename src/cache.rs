//! Slitter stashes allocations for each size class in a thread-local
//! cache.
#[cfg(any(
    all(test, feature = "check_contracts_in_tests"),
    feature = "check_contracts"
))]
use contracts::*;
#[cfg(not(any(
    all(test, feature = "check_contracts_in_tests"),
    feature = "check_contracts"
)))]
use disabled_contracts::*;

use smallvec::SmallVec;
use std::cell::RefCell;
use std::num::NonZeroU32;

#[cfg(any(
    all(test, feature = "check_contracts_in_tests"),
    feature = "check_contracts"
))]
use crate::debug_allocation_map;
#[cfg(any(
    all(test, feature = "check_contracts_in_tests"),
    feature = "check_contracts"
))]
use crate::debug_type_map;
#[cfg(any(
    all(test, feature = "check_contracts_in_tests"),
    feature = "check_contracts"
))]
use crate::press;

use crate::class::ClassInfo;
use crate::linear_ref::LinearRef;
use crate::magazine::Magazine;
use crate::Class;

/// For each allocation class, we cache up to one magazine's worth of
/// allocations, and another magazine's worth of newly deallocated
/// objects.
///
/// Classically, we would use one magazine as a stack for both
/// allocations and deallocations, and a backup to avoid bouncing
/// nearly-full or nearly-empty magazines back to the `ClassInfo`.
/// We instead use two magazines to let us do smarter things on
/// deallocation (e.g., more easily check for double free or take
/// advantage of pre-zeroed out allocations).
struct ClassCache {
    /// The cache allocates from this magazine.
    allocation_mag: Magazine,
    /// The cache releases into this magazine.
    release_mag: Magazine,

    /// The `info` field is only `None` for the dummy entry we keep
    /// around for the invalid "0" class id.
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

/// Attempts to return an allocation for an object of this `class`.
#[ensures(ret.is_some() ->
          debug_allocation_map::can_be_allocated(class, ret.as_ref().unwrap().get()).is_ok(),
          "Successful allocations must be in the correct class and not double allocate")]
#[ensures(ret.is_some() ->
          debug_type_map::is_class(class, ret.as_ref().unwrap()).is_ok(),
          "Successful allocations must match the class of the address range.")]
#[ensures(ret.is_some() ->
          press::check_allocation(class, ret.as_ref().unwrap().get().as_ptr() as usize).is_ok(),
          "Sucessful allocations must have the allocation metadata set correctly.")]
#[inline(always)]
pub fn allocate(class: Class) -> Option<LinearRef> {
    CACHE
        .try_with(|cache| cache.borrow_mut().allocate(class))
        .unwrap_or_else(|_| class.info().allocate_slow())
}

/// Returns an allocation back to this `class`.
#[requires(debug_allocation_map::has_been_released(class, block.get()).is_ok(),
           "Blocks passed to `release` must have already been marked as released.")]
#[requires(debug_type_map::is_class(class, &block).is_ok(),
           "Deallocated blocks must match the class of the address range.")]
#[requires(press::check_allocation(class, block.get().as_ptr() as usize).is_ok(),
          "Deallocated block must have the allocation metadata set correctly.")]
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
    #[invariant(self.check_rep_or_err().is_ok(), "Internal invariants hold.")]
    #[ensures(self.per_class.is_empty(), "The cache must be empty before dropping.")]
    fn drop(&mut self) {
        while let Some(slot) = self.per_class.pop() {
            if let Some(info) = slot.info {
                info.release_magazine(slot.allocation_mag);
                info.release_magazine(slot.release_mag);
            } else {
                // This must be the padding slot at index 0.
                assert!(self.per_class.is_empty());

                let default_rack = crate::rack::get_default_rack();
                default_rack.release_empty_magazine(slot.allocation_mag);
                default_rack.release_empty_magazine(slot.release_mag);
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

    /// Returns `Err` when some of the Cache's invariants are violated.
    #[cfg(any(
        all(test, feature = "check_contracts_in_tests"),
        feature = "check_contracts"
    ))]
    fn check_rep_or_err(&self) -> Result<(), &'static str> {
        if !self
            .per_class
            .iter()
            .enumerate()
            .all(|(i, x)| i == 0 || x.info.unwrap().id.id().get() as usize == i)
        {
            return Err("Some cache entries are missing their info.");
        }

        if let Some(dummy) = self.per_class.get(0) {
            if !dummy.allocation_mag.is_empty() {
                return Err("Dummy cache entry has a non-empty allocation magazine.");
            }

            if !dummy.release_mag.is_empty() {
                return Err("Dummy cache entry has a non-empty allocation magazine.");
            }
        }

        // All magazines must be in a good state, and only contain
        // *available* allocations for the correct class.
        for class_cache in &self.per_class {
            class_cache
                .allocation_mag
                .check_rep(class_cache.info.map(|info| info.id))?;
            class_cache
                .release_mag
                .check_rep(class_cache.info.map(|info| info.id))?;
        }

        Ok(())
    }

    /// Ensures the cache's `per_class` array has one entry for every
    /// allocation class currently defined.
    #[invariant(self.check_rep_or_err().is_ok(), "Internal invariants hold.")]
    #[ensures(self.per_class.len() > old(crate::class::max_id()),
              "There exists an entry for the max class id when the function was called.")]
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

            let (allocation_mag, release_mag) = if let Some(i) = info {
                (i.allocate_magazine(), i.allocate_non_full_magazine())
            } else {
                (
                    crate::rack::get_default_rack().allocate_empty_magazine(),
                    crate::rack::get_default_rack().allocate_empty_magazine(),
                )
            };

            self.per_class.push(ClassCache {
                allocation_mag,
                release_mag,
                info,
            })
        }
    }

    /// Attempts to return an allocation for `class`.  Consumes from
    /// the cache if possible, and hits the Class(Info)'s slow path
    /// otherwise.
    #[invariant(self.check_rep_or_err().is_ok(), "Internal invariants hold.")]
    #[ensures(ret.is_some() ->
              debug_allocation_map::can_be_allocated(class, ret.as_ref().unwrap().get()).is_ok(),
              "Successful allocations must be from the correct class, and not double allocate.")]
    #[ensures(ret.is_some() -> debug_type_map::is_class(class, ret.as_ref().unwrap()).is_ok(),
              "Successful allocations must match the class of the address range.")]
    #[ensures(ret.is_some() ->
              press::check_allocation(class, ret.as_ref().unwrap().get().as_ptr() as usize).is_ok(),
              "Sucessful allocations must have the allocation metadata set correctly.")]
    #[inline(always)]
    fn allocate(&mut self, class: Class) -> Option<LinearRef> {
        let index = class.id().get() as usize;

        if self.per_class.len() <= index {
            self.grow();
        }

        let entry = &mut self.per_class[index];
        if let Some(alloc) = entry.allocation_mag.get() {
            return Some(alloc);
        }

        entry
            .info
            .expect("must have class info")
            .refill_magazine(&mut entry.allocation_mag)
    }

    /// Marks `block`, an allocation for `class`, ready for reuse.
    /// Pushes to the cache if possible, and hits the Class(Info)'s
    /// slow path otherwise.
    #[invariant(self.check_rep_or_err().is_ok(), "Internal invariants hold.")]
    #[requires(debug_allocation_map::has_been_released(class, block.get()).is_ok(),
               "A released block for `class` must have been marked as such.")]
    #[requires(debug_type_map::is_class(class, &block).is_ok(),
               "Deallocated blocks must match the class of the address range.")]
    #[requires(press::check_allocation(class, block.get().as_ptr() as usize).is_ok(),
               "Deallocated block must have the allocation metadata set correctly.")]
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
        if let Some(spill) = entry.release_mag.put(block) {
            entry
                .info
                .expect("must have class info")
                .clear_magazine(&mut entry.release_mag, spill);
        }
    }
}
