//! A `Press` creates new allocations for a given `Class`.  The
//! allocations must be such that the `Press` can also map valid
//! addresses back to their `Class`.
//!
//! While each class gets its own press, the latter requirement means
//! that the presses must all implement compatible metadata stashing
//! schemes.  This works because `Mill`s all use the same scheme.
//!
//! We enable mostly lock-free operations by guaranteeing that each
//! span and corresponding metadata is immortal once allocated.
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

use std::alloc::Layout;
use std::ffi::c_void;
use std::mem::MaybeUninit;
use std::num::NonZeroUsize;
use std::ptr::NonNull;
use std::sync::atomic::AtomicPtr;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::Mutex;

#[cfg(any(
    all(test, feature = "check_contracts_in_tests"),
    feature = "check_contracts"
))]
use crate::debug_allocation_map;
#[cfg(any(
    all(test, feature = "check_contracts_in_tests"),
    feature = "check_contracts"
))]
use crate::debug_arange_map;
#[cfg(any(
    all(test, feature = "check_contracts_in_tests"),
    feature = "check_contracts"
))]
use crate::debug_type_map;

use crate::linear_ref::LinearRef;
use crate::mill;
use crate::mill::Mill;
use crate::mill::SpanMetadata;
use crate::mill::MAX_SPAN_SIZE;
use crate::Class;

/// We batch-allocate at most this many elements at once.  This limit
/// makes it clear that a 64-bit counter will not wraparound.
///
/// In practice, callers ask for one more than the magazine size, at
/// most, and that's less than this limit.
const MAX_ALLOCATION_BATCH: usize = 100;

static_assertions::const_assert!(
    (crate::magazine_impl::MAGAZINE_SIZE as usize) < MAX_ALLOCATION_BATCH
);

/// We don't guarantee alignment greater than this value.
pub const MAX_OBJECT_ALIGNMENT: usize = 4096;

static_assertions::const_assert!(MAX_OBJECT_ALIGNMENT <= mill::MAX_SPAN_SIZE);

#[derive(Debug)]
pub struct Press {
    /// The current span that services bump pointer allocation.
    bump: AtomicPtr<SpanMetadata>,

    /// Writes to the bump itself (i.e., updating the `AtomicPtr`
    /// itself) go through this lock.
    mill: Mutex<&'static Mill>,
    layout: Layout,
    class: Class,
}

/// Returns Ok if the allocation `address` might have come from a `Press` for `class`.
///
/// # Errors
///
/// Returns Err if the address definitely did not come from that `class`.
#[inline]
pub fn check_allocation(class: Class, address: usize) -> Result<(), &'static str> {
    let meta_ptr = SpanMetadata::from_allocation_address(address);

    let meta = unsafe { meta_ptr.as_mut() }.ok_or("Derived a bad metadata address")?;
    if meta.class_id != Some(class.id()) {
        Err("Incorrect class id")
    } else {
        Ok(())
    }
}

impl Press {
    /// Returns a fresh `Press` for an object `class` with that object
    /// `layout`, and the underlying mapper `mapper_name` (`None` for
    /// the default `Mapper` / `Mill`).
    ///
    /// All presses with the same `mapper_name` share the same `Mill`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the layout violates the allocator's constraints,
    /// or no mapper can be found for `mapper_name`.
    pub fn new(
        class: Class,
        mut layout: Layout,
        mapper_name: Option<&str>,
    ) -> Result<Self, &'static str> {
        if layout.align() > MAX_OBJECT_ALIGNMENT {
            return Err("slitter only supports alignment up to 4 KB");
        }

        layout = layout.pad_to_align();
        assert_eq!(layout.size() % layout.align(), 0);

        if layout.size() > MAX_SPAN_SIZE / 2 {
            Err("Class elements too large (after alignment)")
        } else {
            Ok(Self {
                bump: Default::default(),
                mill: Mutex::new(mill::get_mill(mapper_name)?),
                layout,
                class,
            })
        }
    }

    /// Associates the `count` allocations starting at `begin` with `self.class`.
    #[cfg(any(
        all(test, feature = "check_contracts_in_tests"),
        feature = "check_contracts"
    ))]
    fn associate_range(&self, begin: usize, count: usize) -> Result<(), &'static str> {
        for i in 0..count {
            debug_type_map::associate_class(self.class, begin + i * self.layout.size())?;
        }

        Ok(())
    }

    /// Checks if the `count` allocations starting at `begin` are associated with `self.class`.
    #[cfg(any(
        all(test, feature = "check_contracts_in_tests"),
        feature = "check_contracts"
    ))]
    fn is_range_associated_and_free(&self, begin: usize, count: usize) -> Result<(), &'static str> {
        for i in 0..count {
            let address = NonNull::new((begin + i * self.layout.size()) as *mut c_void)
                .ok_or("allocated NULL pointer")?;
            debug_type_map::ptr_is_class(self.class, &address)?;
            debug_allocation_map::can_be_allocated(self.class, &address)?;
        }

        Ok(())
    }

    /// Checks that all `count` allocations starting at `begin` are associated with `self.class`.
    #[cfg(any(
        all(test, feature = "check_contracts_in_tests"),
        feature = "check_contracts"
    ))]
    fn check_allocation_range(&self, begin: usize, count: usize) -> Result<(), &'static str> {
        for i in 0..count {
            check_allocation(self.class, begin + i * self.layout.size())?;
        }

        Ok(())
    }

    /// Attempts to allocate up to `max_count` consecutive object by
    /// bumping the metadata pointer.
    ///
    /// Returns the address of the first object and the number of
    /// allocations on success.
    #[requires(debug_arange_map::is_metadata(meta as * mut SpanMetadata as usize,
                                             std::mem::size_of::<SpanMetadata>()).is_ok(),
               "The `meta` reference must come from a metadata range.")]
    #[ensures(ret.is_some() -> ret.unwrap().1.get() <= max_count.get(),
              "We never return more than `max_count` allocations.")]
    #[ensures(ret.is_some() -> ret.unwrap().0.get() as usize % self.layout.align() == 0,
              "The base address is correctly aligned.")]
    #[ensures(ret.is_some() -> self.associate_range(ret.unwrap().0.get(), ret.unwrap().1.get()).is_ok(),
              "On success, it must be possible to associate the returned address with `self.class`.")]
    #[ensures(ret.is_some() ->
              debug_arange_map::is_data(ret.unwrap().0.get(), self.layout.size() * ret.unwrap().1.get()).is_ok(),
              "On success, the returned data must come from a data range.")]
    #[ensures(ret.is_some() -> self.check_allocation_range(ret.unwrap().0.get(), ret.unwrap().1.get()).is_ok(),
              "On success, the allocations must all have the class metadata set up.")]
    fn try_allocate_from_span(
        &self,
        meta: &mut SpanMetadata,
        max_count: NonZeroUsize,
    ) -> Option<(NonZeroUsize, NonZeroUsize)> {
        let desired = max_count.get().clamp(0, MAX_ALLOCATION_BATCH);
        let limit = meta.bump_limit as usize;

        let allocated_id = meta.bump_ptr.fetch_add(desired, Ordering::Relaxed);
        if allocated_id >= limit {
            return None;
        }

        // This is our actual allocation count: our allocated range
        // starts at `allocated_id`, and stops at `allocated_id +
        // desired` (that's how much we acquired), or at `limit`
        // if we acquired more than the bump limit.
        let actual = (limit - allocated_id).clamp(0, desired);

        // `meta.bump_ptr` is incremented atomically, so
        // we always return fresh addresses.
        //
        // XXX: This expression has to satisfy the `ensures`
        // postconditions; they're checked in
        // `assert_new_bump_is_safe`, including the alignment
        // of `span_begin`.
        Some((
            NonZeroUsize::new(meta.span_begin + allocated_id * self.layout.size())?,
            NonZeroUsize::new(actual)?,
        ))
    }

    /// Asserts that every allocation in `bump` is valid for the
    /// allocation.
    #[cfg(any(
        all(test, feature = "check_contracts_in_tests"),
        feature = "check_contracts"
    ))]
    fn assert_new_bump_is_safe(&self, bump: *mut SpanMetadata) {
        assert!(
            debug_arange_map::is_metadata(bump as usize, std::mem::size_of::<SpanMetadata>())
                .is_ok()
        );

        let meta = unsafe { bump.as_mut() }.expect("must be valid");

        assert_eq!(meta.span_begin % self.layout.align(), 0);

        for i in 0..meta.bump_limit as usize {
            let address = meta.span_begin + i * self.layout.size();
            assert!(debug_arange_map::is_data(address, self.layout.size()).is_ok());
            assert!(check_allocation(self.class, address).is_ok());
        }
    }

    #[cfg(not(any(
        all(test, feature = "check_contracts_in_tests"),
        feature = "check_contracts"
    )))]
    #[inline]
    fn assert_new_bump_is_safe(&self, _bump: *mut SpanMetadata) {}

    /// Attempts to replace our bump pointer with a new one.
    #[ensures(ret.is_ok() ->
              self.bump.load(Ordering::Relaxed) != old(self.bump.load(Ordering::Relaxed)),
              "On success, the bump Span has been updated.")]
    #[ensures(debug_arange_map::is_metadata(self.bump.load(Ordering::Relaxed) as usize,
                                            std::mem::size_of::<SpanMetadata>()).is_ok(),
              "The bump struct must point to a valid metadata range.")]
    fn try_replace_span(&self, expected: *mut SpanMetadata) -> Result<(), i32> {
        if self.bump.load(Ordering::Relaxed) != expected {
            // Someone else made progress.

            return Ok(());
        }

        let mill = self.mill.lock().unwrap();
        // Check again with the lock held, before allocating a new span.
        if self.bump.load(Ordering::Relaxed) != expected {
            return Ok(());
        }

        // Get a new span.  It must have enough bytes for one
        // allocation, but will usually have more (the default desired
        // size, nearly 1 MB).
        let range = mill.get_span(self.layout.size(), None)?;
        let meta: &mut _ = range.meta;

        // We should have a fresh Metadata struct before claiming it as ours.
        assert_eq!(meta.class_id, None);
        meta.class_id = Some(self.class.id());
        meta.bump_limit = (range.data_size / self.layout.size()) as u32;
        assert!(
            meta.bump_limit > 0,
            "layout.size > MAX_SPAN_SIZE, but we check for that in the constructor."
        );
        meta.bump_ptr = AtomicUsize::new(0);
        meta.span_begin = range.data as usize;

        // Make sure allocations in the trail are properly marked as being ours.
        for trailing_meta in range.trail {
            // This Metadata struct must not already be allocated.
            assert_eq!(trailing_meta.class_id, None);
            trailing_meta.class_id = Some(self.class.id());
        }

        // Publish the metadata for our fresh span.
        assert_eq!(self.bump.load(Ordering::Relaxed), expected);
        self.assert_new_bump_is_safe(meta);
        self.bump.store(meta, Ordering::Release);
        Ok(())
    }

    /// Attempts to allocate up to `max_count` objects.  Returns Ok()
    /// if we tried to allocate from the current bump region.
    ///
    /// On allocation success, returns Ok(Some(base_address, object_count))
    ///
    /// # Errors
    ///
    /// Returns `Err` if we failed to grab a new bump region.
    #[ensures(ret.is_ok() && ret.unwrap().is_some() ->
              ret.unwrap().unwrap().1.get() <= max_count.get(),
              "We never overallocate.")]
    #[ensures(ret.is_ok() && ret.unwrap().is_some() ->
              self.is_range_associated_and_free(ret.unwrap().unwrap().0.get(), ret.unwrap().unwrap().1.get()).is_ok(),
              "Successful allocations are fresh, or match the class and avoid double-allocation.")]
    #[ensures(ret.is_ok() && ret.unwrap().is_some() ->
              self.check_allocation_range(ret.unwrap().unwrap().0.get(), ret.unwrap().unwrap().1.get()).is_ok(),
              "Sucessful allocations must have the allocation metadata set correctly.")]
    fn try_allocate_once(
        &self,
        max_count: NonZeroUsize,
    ) -> Result<Option<(NonZeroUsize, NonZeroUsize)>, i32> {
        let meta_ptr: *mut SpanMetadata = self.bump.load(Ordering::Acquire);

        if let Some(meta) = unsafe { meta_ptr.as_mut() } {
            if let Some(result) = self.try_allocate_from_span(meta, max_count) {
                return Ok(Some(result));
            }
        }

        // Either we didn't find any span metadata, or bump
        // allocation failed.  Either way, let's try to put
        // a new span in.
        self.try_replace_span(meta_ptr).map(|_| None)
    }

    /// Tries to allocate up to `max_count` objects.  Only fails on OOM.
    #[ensures(ret.is_some() ->
              ret.unwrap().1.get() <= max_count.get(),
              "We never overallocate.")]
    #[ensures(ret.is_some() ->
              self.is_range_associated_and_free(ret.unwrap().0.get(), ret.unwrap().1.get()).is_ok(),
              "Successful allocations are fresh, or match the class and avoid double-allocation.")]
    #[ensures(ret.is_some() ->
              self.check_allocation_range(ret.unwrap().0.get(), ret.unwrap().1.get()).is_ok(),
              "Sucessful allocations must have the allocation metadata set correctly.")]
    fn try_allocate(&self, max_count: NonZeroUsize) -> Option<(NonZeroUsize, NonZeroUsize)> {
        loop {
            match self.try_allocate_once(max_count) {
                Err(_) => return None, // TODO: log
                Ok(Some(result)) => return Some(result),
                _ => continue,
            }
        }
    }

    #[ensures(ret.is_some() ->
              debug_allocation_map::can_be_allocated(self.class, ret.as_ref().unwrap().get()).is_ok(),
              "Successful allocations are fresh, or match the class and avoid double-allocation.")]
    #[ensures(ret.is_some() ->
              debug_type_map::is_class(self.class, ret.as_ref().unwrap()).is_ok(),
              "On success, the new allocation has the correct type.")]
    #[ensures(ret.is_some() ->
              check_allocation(self.class, ret.as_ref().unwrap().get().as_ptr() as usize).is_ok(),
              "Sucessful allocations must have the allocation metadata set correctly.")]
    pub fn allocate_one_object(&self) -> Option<LinearRef> {
        let (address, _count) = self.try_allocate(NonZeroUsize::new(1).unwrap())?;

        debug_assert_eq!(_count.get(), 1);
        Some(LinearRef::new(unsafe {
            NonNull::new_unchecked(address.get() as *mut c_void)
        }))
    }

    /// Attempts to allocate multiple objects: first the second return
    /// value, and then as many elements in `dst` as possible.
    ///
    /// Returns the number of elements populated in `dst` (starting
    /// at low indices), and an allocated object if possible.
    #[ensures(ret.1.is_some() ->
              debug_allocation_map::can_be_allocated(self.class, ret.1.as_ref().unwrap().get()).is_ok(),
              "Successful allocations are fresh, or match the class and avoid double-allocation.")]
    #[ensures(ret.1.is_some() ->
              debug_type_map::is_class(self.class, ret.1.as_ref().unwrap()).is_ok(),
              "On success, the new allocation has the correct type.")]
    #[ensures(ret.1.is_some() ->
              check_allocation(self.class, ret.1.as_ref().unwrap().get().as_ptr() as usize).is_ok(),
              "Sucessful allocations must have the allocation metadata set correctly.")]
    #[ensures(ret.1.is_none() -> ret.0 == 0,
              "We always try to satisfy the return value first.")]
    // We don't check `dst` because the contract expression would be
    // unsafe, but it's the same as `ret.1.is_some()` for all
    // populated elements.
    //
    // We do check the same invariants in the target `Magazine` via
    // `ClassInfo::refill_magazine`.
    pub fn allocate_many_objects(
        &self,
        dst: &mut [MaybeUninit<LinearRef>],
    ) -> (usize, Option<LinearRef>) {
        let elsize = self.layout.size();

        match self.try_allocate(NonZeroUsize::new(dst.len() + 1).expect("Should not overflow")) {
            Some((base, count)) => {
                let mut address = base.get();

                // Acquires the next element from `base[0..count]`.
                let mut get_ref = || {
                    let ret =
                        LinearRef::new(unsafe { NonNull::new_unchecked(address as *mut c_void) });

                    address += elsize;
                    ret
                };

                let ret = Some(get_ref());

                let mut populated = 0;
                for uninit in dst.iter_mut().take(count.get() - 1) {
                    unsafe { uninit.as_mut_ptr().write(get_ref()) };
                    populated += 1;
                }

                debug_assert!(populated <= count.get());
                (populated, ret)
            }
            None => (0, None),
        }
    }
}
