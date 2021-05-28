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

use crate::class::ClassInfo;
use crate::linear_ref::LinearRef;
use crate::magazine::PopMagazine;
use crate::magazine::PushMagazine;
use crate::press;
use crate::Class;

/// Start with a fast path array of this many `Magazines`
/// structs, including one for the dummy 0 class.
const INITIAL_CACHE_SIZE: usize = 4;

#[derive(Default)]
#[repr(C)]
struct Magazines {
    /// The cache allocates from this magazine.
    alloc: PopMagazine,
    /// The cache releases into theis magazine.
    release: PushMagazine,
}

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
///
/// The cache consists of parallel vectors that are directly indexed
/// with the class id; the first element at index 0 is thus never
/// used, so we must increment SMALL_CACHE_SIZE by 1.
///
/// The source of truth on the number of allocation classes in the
/// Cache is the `per_class_info` vector; `per_class` may have extra
/// element but is never shorter than the `per_class_info`.
struct Cache {
    /// This array of magazines may be longer than necessary:
    /// zero-initialised magazines will correctly trigger a
    /// slow path.
    per_class: Box<[Magazines]>,
    /// This parallel vector holds a reference to ClassInfo; it is
    /// only `None` for the dummy entry we keep around for the invalid
    /// "0" class id.
    per_class_info: Vec<Option<&'static ClassInfo>>,
}

extern "C" {
    fn slitter__cache_register(region: *mut Magazines, count: usize);
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

/// C-accessible slow path for the allocation.  The slow-path code is
/// identical to regular non-C allocation, so it's always safe to call
/// this function.
#[ensures(ret.is_some() ->
          debug_allocation_map::can_be_allocated(class, ret.as_ref().unwrap().get()).is_ok(),
          "Successful allocations must be in the correct class and not double allocate")]
#[ensures(ret.is_some() ->
          debug_type_map::is_class(class, ret.as_ref().unwrap()).is_ok(),
          "Successful allocations must match the class of the address range.")]
#[ensures(ret.is_some() ->
          press::check_allocation(class, ret.as_ref().unwrap().get().as_ptr() as usize).is_ok(),
          "Sucessful allocations must have the allocation metadata set correctly.")]
#[no_mangle]
pub extern "C" fn slitter__allocate_slow(class: Class) -> Option<LinearRef> {
    let ret = CACHE
        .try_with(|cache| cache.borrow_mut().allocate_slow(class))
        .unwrap_or_else(|_| class.info().allocate_slow());
    assert!(ret.is_some(), "Allocation failed");

    ret
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

/// C-accessible slow path for the allocation.  The slow-path code is
/// identical to regular non-C release call, so it's always safe to
/// call this function.
#[requires(debug_allocation_map::has_been_released(class, block.get()).is_ok(),
           "Blocks passed to `release` must have already been marked as released.")]
#[requires(debug_type_map::is_class(class, &block).is_ok(),
           "Deallocated blocks must match the class of the address range.")]
#[requires(press::check_allocation(class, block.get().as_ptr() as usize).is_ok(),
          "Deallocated block must have the allocation metadata set correctly.")]
#[no_mangle]
pub extern "C" fn slitter__release_slow(class: Class, block: LinearRef) {
    let mut cell = Some(block);

    CACHE
        .try_with(|cache| cache.borrow_mut().release_slow(class, cell.take().unwrap()))
        .unwrap_or_else(|_| {
            if let Some(alloc) = cell {
                class.info().release_slow(alloc)
            }
        })
}

impl Drop for Cache {
    #[invariant(self.check_rep_or_err().is_ok(), "Internal invariants hold.")]
    #[ensures(self.per_class_info.is_empty(), "The cache must be empty before dropping.")]
    fn drop(&mut self) {
        unsafe {
            slitter__cache_register(std::ptr::null_mut(), 0);
        }

        while let Some(slot) = self.per_class_info.pop() {
            let index = self.per_class_info.len();
            let mut mags = Default::default();

            std::mem::swap(
                &mut mags,
                self.per_class
                    .get_mut(index)
                    .expect("per_class should be at least as long as per_class_info"),
            );

            if let Some(info) = slot {
                info.release_magazine(mags.alloc);
                info.release_magazine(mags.release);
            } else {
                // This must be the padding slot at index 0.
                assert!(self.per_class_info.is_empty());

                let default_rack = crate::rack::get_default_rack();
                default_rack.release_empty_magazine(mags.alloc);
                default_rack.release_empty_magazine(mags.release);
            }
        }
    }
}

impl Cache {
    fn new() -> Cache {
        let mags: [Magazines; INITIAL_CACHE_SIZE] = Default::default();

        Cache {
            per_class: Box::new(mags),
            per_class_info: Vec::new(),
        }
    }

    /// Returns `Err` when some of the Cache's invariants are violated.
    #[cfg(any(
        all(test, feature = "check_contracts_in_tests"),
        feature = "check_contracts"
    ))]
    fn check_rep_or_err(&self) -> Result<(), &'static str> {
        if self.per_class.len() < self.per_class_info.len() {
            return Err("Vector of magazines is shorter than vector of info.");
        }

        for mags in self.per_class.iter().skip(self.per_class_info.len()) {
            if !mags.alloc.is_empty() {
                return Err("Padding cache entry has a non-empty allocation magazine.");
            }

            if !mags.release.is_full() {
                return Err("Padding cache entry has a non-full release magazine.");
            }
        }

        if !self
            .per_class_info
            .iter()
            .enumerate()
            .all(|(i, x)| i == 0 || x.unwrap().id.id().get() as usize == i)
        {
            return Err("Some cache entries are missing their info.");
        }

        if let Some(_) = self.per_class_info.get(0) {
            if !self.per_class[0].alloc.is_empty() {
                return Err("Dummy cache entry has a non-empty allocation magazine.");
            }

            if !self.per_class[0].release.is_full() {
                return Err("Dummy cache entry has a non-full release magazine.");
            }
        }

        // All magazines must be in a good state, and only contain
        // *available* allocations for the correct class.
        for (mags, info) in self.per_class.iter().zip(&self.per_class_info) {
            mags.alloc.check_rep(info.map(|info| info.id))?;
            mags.release.check_rep(info.map(|info| info.id))?;
        }

        Ok(())
    }

    /// Ensure the cache's `per_class` array has at least `min_length`
    /// elements.
    #[invariant(self.check_rep_or_err().is_ok(), "Internal invariants hold.")]
    #[requires(min_length < usize::MAX / 2)]
    #[ensures(self.per_class.len() >= min_length)]
    #[ensures(self.per_class.len() >= old(self.per_class.len()))]
    fn ensure_per_class_length(&mut self, min_length: usize) {
        if self.per_class.len() >= min_length {
            return;
        }

        let new_length = min_length
            .checked_next_power_of_two()
            .expect("&CacheInfo are too big for len > usize::MAX / 2");
        let mut vec = Vec::with_capacity(new_length);
        vec.resize_with(new_length, Default::default);

        let mut new_slice = vec.into_boxed_slice();
        self.per_class
            .swap_with_slice(&mut new_slice[0..self.per_class.len()]);

        std::mem::swap(&mut new_slice, &mut self.per_class);
    }

    /// Ensures the cache's `per_class_info` array has one entry for every
    /// allocation class currently defined.
    #[invariant(self.check_rep_or_err().is_ok(), "Internal invariants hold.")]
    #[ensures(self.per_class_info.len() > old(crate::class::max_id()),
              "There exists an entry for the max class id when the function was called.")]
    #[cold]
    fn grow(&mut self) {
        let max_id = crate::class::max_id();
        if self.per_class_info.len() > max_id {
            return;
        }

        assert!(max_id <= u32::MAX as usize);
        self.ensure_per_class_length(max_id + 1);

        while self.per_class_info.len() <= max_id {
            let id = NonZeroU32::new(self.per_class_info.len() as u32);
            let info = id.and_then(|id| Class::from_id(id).map(|class| class.info()));

            self.per_class_info.push(info);
        }

        unsafe {
            // We want to pass `per_class.len()`, despite it being
            // longer than `per_class_info`: the extra elements will
            // correctly trigger a slow path, so this is safe, and we
            // want to concentrate all slow path conditionals to the
            // same branch, for predictability.  We can't get rid of
            // the "magazine is exhausted" condition, so let's make
            // the "array is too short" branch as unlikely as possible.
            slitter__cache_register(self.per_class.as_mut_ptr(), self.per_class.len());
        }
    }

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
    fn allocate_slow(&mut self, class: Class) -> Option<LinearRef> {
        let index = class.id().get() as usize;

        if self.per_class_info.len() <= index {
            self.grow();
        }

        // per_class.len() >= per_class_info.len()
        let mag = &mut self.per_class[index].alloc;
        if let Some(alloc) = mag.get() {
            return Some(alloc);
        }

        self.per_class_info[index]
            .expect("must have class info")
            .refill_magazine(mag)
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
        #[cfg(features = "c_fast_path")]
        const C_FAST_PATH: bool = true;
        #[cfg(not(features = "c_fast_path"))]
        const C_FAST_PATH: bool = false;

        if C_FAST_PATH {
            extern "C" {
                fn slitter_allocate(class: Class) -> Option<LinearRef>;
            }

            unsafe { slitter_allocate(class) }
        } else {
            self.allocate_slow(class)
        }
    }

    #[invariant(self.check_rep_or_err().is_ok(), "Internal invariants hold.")]
    #[requires(debug_allocation_map::has_been_released(class, block.get()).is_ok(),
               "A released block for `class` must have been marked as such.")]
    #[requires(debug_type_map::is_class(class, &block).is_ok(),
               "Deallocated blocks must match the class of the address range.")]
    #[requires(press::check_allocation(class, block.get().as_ptr() as usize).is_ok(),
               "Deallocated block must have the allocation metadata set correctly.")]
    #[inline(always)]
    fn release_slow(&mut self, class: Class, block: LinearRef) {
        press::check_allocation(class, block.get().as_ptr() as usize)
            .expect("deallocated address should match allocation class");

        let index = class.id().get() as usize;

        if self.per_class_info.len() <= index {
            assert!(index < u32::MAX as usize);
            self.grow();
        }

        // per_class.len() >= per_class_info.len()
        let mag = &mut self.per_class[index].release;
        // We prefer to cache freshly deallocated objects, for
        // temporal locality.
        if let Some(spill) = mag.put(block) {
            self.per_class_info[index]
                .expect("must have class info")
                .clear_magazine(mag, spill);
        }
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
        #[cfg(features = "c_fast_path")]
        const C_FAST_PATH: bool = true;
        #[cfg(not(features = "c_fast_path"))]
        const C_FAST_PATH: bool = false;

        if C_FAST_PATH {
            extern "C" {
                fn slitter_release(class: Class, block: LinearRef);
            }

            unsafe { slitter_release(class, block) }
        } else {
            self.release_slow(class, block)
        }
    }
}
