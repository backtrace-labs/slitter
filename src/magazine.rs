//! The cache layer always allocates from and releases into small
//! arrays of pointers to pre-allocated block.  These small arrays are
//! "magazines," and are themselves allocated and released by a
//! "rack."
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

use std::mem::MaybeUninit;

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
#[cfg(any(
    all(test, feature = "check_contracts_in_tests"),
    feature = "check_contracts"
))]
use crate::Class;

use crate::linear_ref::LinearRef;
use crate::magazine_impl::MagazineImpl;

/// A Magazine is a thin wrapper around MagazineImpl: the wrapping
/// lets us impose a tighter contract on the interface used in the
/// allocator, while keeping the internal implementation testable.
///
/// A `PUSH_MAG: true` magazine can only grow, and a `PUSH_MAG: false`
/// one can only shrink.
///
/// The default for Push magazines is always full, and the default Pop
/// magazine is always empty.
#[derive(Default)]
#[repr(transparent)]
pub struct Magazine<const PUSH_MAG: bool>(pub(crate) MagazineImpl<PUSH_MAG>);

pub type PushMagazine = Magazine<true>;
pub type PopMagazine = Magazine<false>;

/// Thread-local allocation caches also cache magazines locally.
/// Buffering one magazine before pushing it to global freelists,
/// helps avoid contention for common patterns like back-to-back
/// allocation and deallocation.
pub enum LocalMagazineCache {
    Nothing,
    Empty(PopMagazine), // Always an empty magazine with storage.
    Full(PushMagazine), // Always a full magazine with storage.
}

impl LocalMagazineCache {
    /// Stores `mag` in the cache, and returns the previously-cached
    /// magazine, if any.
    ///
    /// If `mag` cannot be cached, returns `mag`.
    pub fn populate<const PUSH_MAG: bool>(
        &mut self,
        mag: Magazine<PUSH_MAG>,
    ) -> Option<Magazine<PUSH_MAG>> {
        use LocalMagazineCache::*;

        if !mag.has_storage() {
            return Some(mag);
        }

        let mut local;

        if mag.is_full() {
            let storage = mag.0.storage();
            assert!(storage.is_some(), "Checked on entry");
            local = LocalMagazineCache::Full(Magazine(MagazineImpl::new(storage)));
        } else if mag.is_empty() {
            let storage = mag.0.storage();
            assert!(storage.is_some(), "Checked on entry");
            local = LocalMagazineCache::Empty(Magazine(MagazineImpl::new(storage)));
        } else {
            return Some(mag);
        }

        std::mem::swap(self, &mut local);

        match local {
            Nothing => None,
            Empty(cached) => Some(Magazine(MagazineImpl::new(cached.0.storage()))),
            Full(cached) => Some(Magazine(MagazineImpl::new(cached.0.storage()))),
        }
    }

    /// Returns a full `PopMagazine` if one is cached.
    pub fn steal_full(&mut self) -> Option<PopMagazine> {
        use LocalMagazineCache::*;

        match self {
            Nothing => None,
            Empty(_) => None,
            Full(_) => {
                let mut private = LocalMagazineCache::Nothing;

                std::mem::swap(&mut private, self);
                let storage = if let Full(mag) = private {
                    mag.0.storage()?
                } else {
                    panic!("std::mem::swap changed enum");
                };

                assert_eq!(
                    storage.num_allocated_slow,
                    crate::magazine_impl::MAGAZINE_SIZE
                );
                Some(Magazine(MagazineImpl::new(Some(storage))))
            }
        }
    }

    /// Returns an empty `PushMagazine` if one is cached.
    pub fn steal_empty(&mut self) -> Option<PushMagazine> {
        use LocalMagazineCache::*;

        match self {
            Nothing => None,
            Full(_) => None,
            Empty(_) => {
                let mut private = LocalMagazineCache::Nothing;

                std::mem::swap(&mut private, self);
                let storage = if let Empty(mag) = private {
                    mag.0.storage()?
                } else {
                    panic!("std::mem::swap changed enum");
                };

                assert_eq!(storage.num_allocated_slow, 0);
                Some(Magazine(MagazineImpl::new(Some(storage))))
            }
        }
    }
}

impl<const PUSH_MAG: bool> Magazine<PUSH_MAG> {
    /// Checks that current object's state is valid.
    ///
    /// If a class is provided, all allocations must match it.
    #[cfg(any(
        all(test, feature = "check_contracts_in_tests"),
        feature = "check_contracts"
    ))]
    pub fn check_rep(&self, maybe_class: Option<Class>) -> Result<(), &'static str> {
        if !self.0.check_rep() {
            return Err("MagazineImpl fails check_rep");
        }

        // If we have an allocation class, the types must match.
        if let Some(class) = maybe_class {
            let info = class.info();
            let zeroed_out = |alloc: &LinearRef| {
                let ptr = alloc.get().as_ptr() as *const u8;

                (0..info.layout.size()).all(|i| unsafe { std::ptr::read(ptr.add(i)) } == 0)
            };

            for i in 0..self.0.len() {
                if let Some(alloc) = self.0.nth(i) {
                    debug_allocation_map::can_be_allocated(class, alloc.get())?;
                    debug_type_map::is_class(class, alloc)?;
                    press::check_allocation(class, alloc.get().as_ptr() as usize)?;

                    // If allocations are supposed to be zero-initialised,
                    // everything in a pop mag should be zeroed out.
                    if !PUSH_MAG && info.zero_init {
                        if !zeroed_out(alloc) {
                            return Err("Non-zero-initialised cached allocation");
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Returns whether this magazine is backed by real storage, and
    /// thus has capacity.
    #[inline(always)]
    pub fn has_storage(&self) -> bool {
        self.0.has_storage()
    }

    #[inline(always)]
    pub fn is_full(&self) -> bool {
        self.0.is_full()
    }

    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl Magazine</*PUSH_MAG=*/ true> {
    /// Attempts to put an unused block back in the magazine.
    ///
    /// Returns that unused block on failure.
    #[invariant(self.check_rep(None).is_ok())]
    #[inline(always)]
    pub fn put(&mut self, freed: LinearRef) -> Option<LinearRef> {
        self.0.put(freed)
    }
}

impl Magazine</*PUSH_MAG=*/ false> {
    /// Attempts to get an unused block from the magazine.
    #[invariant(self.check_rep(None).is_ok())]
    #[inline(always)]
    pub fn get(&mut self) -> Option<LinearRef> {
        self.0.get()
    }

    /// Returns a slice for the used slots in the magazine
    #[inline(always)]
    fn get_populated(&self) -> &[MaybeUninit<LinearRef>] {
        self.0.get_populated()
    }

    /// Returns a slice for the unused slots in the magazine
    #[inline(always)]
    fn get_unpopulated(&mut self) -> &mut [MaybeUninit<LinearRef>] {
        self.0.get_unpopulated()
    }

    /// Marks the first `count` unused slots in the magazine as now populated.
    #[invariant(self.check_rep(None).is_ok())]
    #[inline(always)]
    fn commit_populated(&mut self, count: usize) {
        self.0.commit_populated(count)
    }
}

impl crate::class::ClassInfo {
    /// Returns a cached magazine; it is never empty.
    #[ensures(ret.is_some() -> !ret.as_ref().unwrap().is_empty(),
              "On success, the magazine is non-empty.")]
    #[ensures(ret.is_some() ->
              ret.as_ref().unwrap().check_rep(Some(self.id)).is_ok(),
              "Returned magazine makes sense for class.")]
    #[inline]
    pub(crate) fn get_cached_magazine(
        &self,
        cache: &mut LocalMagazineCache,
    ) -> Option<PopMagazine> {
        // Pop from partial magazines first, because we'd prefer to
        // have 0 partial mag.
        let ret = cache
            .steal_full()
            .or_else(|| self.partial_mags.try_pop())
            .or_else(|| self.full_mags.pop())?;

        if self.zero_init {
            for allocation in ret.get_populated() {
                unsafe {
                    let alloc = &*allocation.as_ptr();
                    std::ptr::write_bytes(alloc.get().as_ptr() as *mut u8, 0, self.layout.size());
                }
            }
        }

        Some(ret)
    }

    /// Returns a magazine; it may be partially populated or empty.
    #[ensures(!ret.is_full(), "The returned magazine is never empty.")]
    #[ensures(ret.check_rep(Some(self.id)).is_ok(),
              "Returned magazine makes sense for class.")]
    #[inline]
    pub(crate) fn allocate_non_full_magazine(
        &self,
        cache: &mut LocalMagazineCache,
    ) -> PushMagazine {
        cache
            .steal_empty()
            .or_else(|| self.partial_mags.try_pop())
            .unwrap_or_else(|| self.rack.allocate_empty_magazine())
    }

    /// Attempts to return one allocation and to refill `mag`.
    ///
    /// When the return value is not `None` (i.e., not an OOM), `mag`
    /// is usually non-empty on exit; in the common case, `mag` is
    /// one allocation (the return value) short of full.
    #[invariant(mag.check_rep(Some(self.id)).is_ok(),
               "Magazine must match `self`.")]
    #[requires(mag.is_empty(),
               "Magazine must be empty on entry.")]
    #[ensures(ret.is_none() -> mag.is_empty(),
              "Allocation never fails when the magazine is non-empty.")]
    #[ensures(ret.is_some() ->
              debug_allocation_map::can_be_allocated(self.id, ret.as_ref().unwrap().get()).is_ok(),
              "Successful allocations are not in use.")]
    #[ensures(ret.is_some() ->
              debug_type_map::is_class(self.id, ret.as_ref().unwrap()).is_ok(),
              "Successful allocations come from an address of the correct class.")]
    #[ensures(ret.is_some() ->
              press::check_allocation(self.id, ret.as_ref().unwrap().get().as_ptr() as usize).is_ok(),
              "Sucessful allocations must have the allocation metadata set correctly.")]
    #[inline(never)]
    pub(crate) fn refill_magazine(
        &self,
        mag: &mut PopMagazine,
        cache: &mut LocalMagazineCache,
    ) -> Option<LinearRef> {
        if let Some(mut new_mag) = self.get_cached_magazine(cache) {
            assert!(!new_mag.is_empty());

            let allocated = new_mag.get();
            std::mem::swap(&mut new_mag, mag);
            self.release_magazine(new_mag, Some(cache));

            return allocated;
        }

        // Make sure we have capacity for `allocate_many_objects()` to
        // do something useful.
        if !mag.has_storage() {
            // We only enter this branch at most once per thread per
            // allocation class: the thread cache starts with a dummy
            // magazine, and we upgrade to a real one here.
            let mut new_mag = self.rack.allocate_empty_magazine();
            std::mem::swap(&mut new_mag, mag);

            self.release_magazine(new_mag, Some(cache));
        }

        let (count, allocated) = self.press.allocate_many_objects(mag.get_unpopulated());
        mag.commit_populated(count);
        allocated
    }

    /// Acquires ownership of `spilled` and all cached allocations from
    /// the magazine, and removes some allocations from `mag`.
    ///
    /// On exit, `spilled` is in a magazine, and `mag` is usually not
    /// full; in the common case, `mag` only contains `spilled`.
    #[invariant(mag.check_rep(Some(self.id)).is_ok(),
               "Magazine must match `self`.")]
    #[requires(debug_allocation_map::has_been_released(self.id, spilled.get()).is_ok(),
               "A released block for `class` must have been marked as such.")]
    #[requires(debug_type_map::is_class(self.id, &spilled).is_ok(),
               "Deallocated blocks must match the class of the address range.")]
    #[requires(press::check_allocation(self.id, spilled.get().as_ptr() as usize).is_ok(),
               "Deallocated block must have the allocation metadata set correctly.")]
    #[inline(never)]
    pub(crate) fn clear_magazine(
        &self,
        mag: &mut PushMagazine,
        cache: &mut LocalMagazineCache,
        spilled: LinearRef,
    ) {
        let mut new_mag = self.allocate_non_full_magazine(cache);

        assert!(!new_mag.is_full());
        assert_eq!(new_mag.put(spilled), None);

        std::mem::swap(&mut new_mag, mag);
        self.release_magazine(new_mag, Some(cache));
    }

    /// Acquires ownership of `mag` and its cached allocations.
    #[requires(mag.check_rep(Some(self.id)).is_ok(),
               "Magazine must match `self`.")]
    #[inline(never)]
    pub(crate) fn release_magazine<const PUSH_MAG: bool>(
        &self,
        mut mag: Magazine<PUSH_MAG>,
        maybe_cache: Option<&mut LocalMagazineCache>,
    ) {
        if let Some(cache) = maybe_cache {
            match cache.populate(mag) {
                Some(new_mag) => mag = new_mag,
                None => return,
            }
        }

        if mag.is_empty() {
            self.rack.release_empty_magazine(mag);
        } else if mag.is_full() {
            self.full_mags.push(mag);
        } else {
            self.partial_mags.push(mag);
        }
    }
}
