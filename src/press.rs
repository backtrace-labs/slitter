//! A `Press` creates new allocations for a given `Class`.  The
//! allocations must be such that the `Press` can also map valid
//! addresses back to their `Class`.
//!
//! While each class gets its own press, the latter requirement means
//! that the presses must all implement compatible metadata stashing
//! schemes.
//!
//! For now, we assume that each `Press` allocates data linearly (with
//! a bump pointer) from 2 MB-aligned spans of 2 MB, and hides the
//! corresponding metadata 8 KB *before* that span, with a guard page
//! between the span's metadata and the actual span data, and more
//! guard pages before the metadata and after the span itself.
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
    pub fn new(class: Class, mut layout: Layout) -> Result<Self, &'static str> {
        if layout.align() > MAX_OBJECT_ALIGNMENT {
            return Err("slitter only supports alignment up to 4 KB");
        }

        layout = layout.pad_to_align();

        if layout.size() > MAX_SPAN_SIZE / 2 {
            Err("Class elements too large (after alignment)")
        } else {
            Ok(Self {
                bump: Default::default(),
                mill: Mutex::new(mill::get_default_mill()),
                layout,
                class,
            })
        }
    }

    /// Attempts to allocate one object by bumping the metadata
    /// pointer.
    #[requires(debug_arange_map::is_metadata(meta as * mut SpanMetadata as usize,
                                             std::mem::size_of::<SpanMetadata>()).is_ok(),
               "The `meta` reference must come from a metadata range.")]
    #[ensures(ret.is_some() ->
              debug_type_map::associate_class(self.class,
                                              ret.as_ref().unwrap()).is_ok(),
              "On success, it must be possible to associate the returned address with `self.class`.")]
    #[ensures(ret.is_some() ->
              debug_arange_map::is_data(ret.as_ref().unwrap().get().as_ptr() as usize,
                                        self.layout.size()).is_ok(),
              "On success, the returned data must come from a data range.")]
    #[ensures(ret.is_some() ->
              check_allocation(self.class,
                               ret.as_ref().unwrap().get().as_ptr() as usize).is_ok(),
              "On success, the allocation must have the class metadata set up.")]
    fn try_allocate_from_span(&self, meta: &mut SpanMetadata) -> Option<LinearRef> {
        let allocated_id = meta.bump_ptr.fetch_add(1, Ordering::Relaxed);

        if allocated_id >= meta.bump_limit as usize {
            return None;
        }

        let address = meta.span_begin + allocated_id * self.layout.size();
        // `meta.bump_ptr` is incremented atomically, so
        // `LinearRef::new` always receives a unique address.
        //
        // XXX: This expression has to satisfy the `ensures`
        // postconditions, checked in `assert_new_bump_is_safe`.
        Some(LinearRef::new(NonNull::new(address as *mut c_void)?))
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

    /// Attempts to allocate one object.  Returns Ok(_) if we tried to
    /// allocate from the current bump region.
    ///
    /// # Errors
    ///
    /// Returns `Err` if we failed to grab a new bump region.
    #[ensures(ret.is_ok() && ret.as_ref().unwrap().is_some() ->
              debug_allocation_map::can_be_allocated(self.class, ret.as_ref().unwrap().as_ref().unwrap().get()).is_ok(),
              "Successful allocations are fresh, or match the class and avoid double-allocation.")]
    #[ensures(ret.is_ok() && ret.as_ref().unwrap().is_some() ->
              debug_type_map::is_class(self.class, ret.as_ref().unwrap().as_ref().unwrap()).is_ok(),
              "On success, the new allocation has the correct type.")]
    #[ensures(ret.is_ok() && ret.as_ref().unwrap().is_some() ->
              check_allocation(self.class, ret.as_ref().unwrap().as_ref().unwrap().get().as_ptr() as usize).is_ok(),
              "Sucessful allocations must have the allocation metadata set correctly.")]
    fn try_allocate_once(&self) -> Result<Option<LinearRef>, i32> {
        let meta_ptr: *mut SpanMetadata = self.bump.load(Ordering::Acquire);

        if let Some(meta) = unsafe { meta_ptr.as_mut() } {
            if let Some(ret) = self.try_allocate_from_span(meta) {
                return Ok(Some(ret));
            }
        }

        // Either we didn't find any span metadata, or bump
        // allocation failed.  Either way, let's try to put
        // a new span in.
        self.try_replace_span(meta_ptr).map(|_| None)
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
        loop {
            match self.try_allocate_once() {
                Err(_) => return None, // TODO: log
                Ok(Some(ret)) => return Some(ret),
                _ => continue,
            }
        }
    }
}
