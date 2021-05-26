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
#[repr(transparent)]
pub struct Magazine(pub(crate) Box<MagazineImpl>);

/// A `MagazineStack` is a single-linked intrusive stack of magazines.
pub struct MagazineStack {
    inner: Mutex<Option<Box<MagazineImpl>>>,
}

/// A `Rack` allocates and recycles empty magazines.
pub struct Rack {
    // No state yet.
}

/// Returns a reference to the global default magazine rack.
pub fn get_default_rack() -> &'static Rack {
    lazy_static::lazy_static! { static ref RACK: Rack = Rack{}; };

    &RACK
}

impl Magazine {
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

        if self.0.link.is_some() {
            // Once wrapped in a public-facing `Magazine`, the link field
            // is always `None`: we only use it for an intrusive list in
            // `MagazineStack`.
            return Err("Magazine has linkage.");
        }

        // If we have an allocation class, the types must match.
        if let Some(class) = maybe_class {
            for i in 0..self.0.num_allocated {
                let alloc = unsafe { &*self.0.allocations[i as usize].as_ptr() };

                debug_allocation_map::can_be_allocated(class, alloc.get())?;
                debug_type_map::is_class(class, alloc)?;
                press::check_allocation(class, alloc.get().as_ptr() as usize)?;
            }
        }

        Ok(())
    }

    /// Attempts to get an unused block from the magazine.
    #[inline(always)]
    pub fn get(&mut self) -> Option<LinearRef> {
        self.0.get()
    }

    /// Attempts to put an unused block back in the magazine.
    ///
    /// Returns that unused block on failure.
    #[inline(always)]
    pub fn put(&mut self, freed: LinearRef) -> Option<LinearRef> {
        self.0.put(freed)
    }

    /// Fills `self` with allocations returned by `allocator`
    #[inline(always)]
    pub fn populate(&mut self, allocator: impl FnMut() -> Option<LinearRef>) {
        self.0.populate(allocator)
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
impl MagazineStack {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(None),
        }
    }

    #[requires(mag.check_rep(None).is_ok(),
               "Magazine must make sense.")]
    fn push(&self, mut mag: Magazine) {
        assert!(mag.0.link.is_none());
        let mut stack = self.inner.lock().unwrap();

        mag.0.link = stack.take();
        *stack = Some(mag.0)
    }

    #[ensures(ret.is_some() ->
              ret.as_ref().unwrap().check_rep(None).is_ok(),
              "Magazine should make sense.")]
    fn pop(&self) -> Option<Magazine> {
        let mut stack = self.inner.lock().unwrap();

        if let Some(mut mag) = stack.take() {
            std::mem::swap(&mut mag.link, &mut *stack);
            assert!(mag.link.is_none());
            Some(Magazine(mag))
        } else {
            None
        }
    }
}

impl Rack {
    #[ensures(ret.is_empty(), "Newly allocated magazines are empty.")]
    pub fn allocate_empty_magazine(&self) -> Magazine {
        Magazine(Box::new(Default::default()))
    }

    #[requires(mag.is_empty(), "Only empty magazines are released to the Rack.")]
    pub fn release_empty_magazine(&self, mag: Magazine) {
        // We can only release empty magazines.
        assert_eq!(mag.0.num_allocated, 0);
        // And now drop it.
    }
}

impl crate::class::ClassInfo {
    /// Returns a magazine; it may be empty, full, or partially
    /// populated.
    #[ensures(ret.check_rep(Some(self.id)).is_ok(),
              "Returned magazine makes sense for class.")]
    #[inline(never)]
    pub(crate) fn allocate_magazine(&self) -> Magazine {
        self.partial_mags
            .pop()
            .or_else(|| self.full_mags.pop())
            .unwrap_or_else(|| self.rack.allocate_empty_magazine())
    }

    /// Returns a cached magazine; it is never empty.
    #[ensures(ret.is_some() -> !ret.as_ref().unwrap().is_empty(),
              "On success, the magazine is non-empty.")]
    #[ensures(ret.is_some() ->
              ret.as_ref().unwrap().check_rep(Some(self.id)).is_ok(),
              "Returned magazine makes sense for class.")]
    #[inline(never)]
    pub(crate) fn get_cached_magazine(&self) -> Option<Magazine> {
        self.partial_mags.pop().or_else(|| self.full_mags.pop())
    }

    /// Returns a magazine; it may be partially populated or empty.
    #[ensures(!ret.is_full(), "The returned magazine is never empty.")]
    #[ensures(ret.check_rep(Some(self.id)).is_ok(),
              "Returned magazine makes sense for class.")]
    #[inline(never)]
    pub(crate) fn allocate_non_full_magazine(&self) -> Magazine {
        self.partial_mags
            .pop()
            .unwrap_or_else(|| self.rack.allocate_empty_magazine())
    }

    /// Attempts to return one allocation and to refill `mag`.
    ///
    /// When the return value is not `None` (i.e., not an OOM), `mag`
    /// is usually non-empty on exit; in the common case, `mag` is
    /// one allocation (the return value) short of full.
    #[invariant(mag.check_rep(Some(self.id)).is_ok(),
               "Magazine must match `self`.")]
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
    pub(crate) fn refill_magazine(&self, mag: &mut Magazine) -> Option<LinearRef> {
        // Try to get a new non-empty magazine; prefer partial mags
        // because we prefer to have 0 partial mags.
        if let Some(mut new_mag) = self.partial_mags.pop().or_else(|| self.full_mags.pop()) {
            assert!(!new_mag.is_empty());

            let allocated = new_mag.get();
            std::mem::swap(&mut new_mag, mag);
            self.release_magazine(new_mag);

            return allocated;
        }

        let allocated = self.press.allocate_one_object()?;
        mag.populate(|| self.press.allocate_one_object());
        Some(allocated)
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
    pub(crate) fn clear_magazine(&self, mag: &mut Magazine, spilled: LinearRef) {
        // Get a new non-full magazine.
        let mut new_mag = self
            .partial_mags
            .pop()
            .unwrap_or_else(|| self.rack.allocate_empty_magazine());

        assert!(!new_mag.is_full());
        assert_eq!(new_mag.put(spilled), None);

        std::mem::swap(&mut new_mag, mag);
        self.release_magazine(new_mag);
    }

    /// Acquires ownership of `mag` and its cached allocations.
    #[requires(mag.check_rep(Some(self.id)).is_ok(),
               "Magazine must match `self`.")]
    #[inline(never)]
    pub(crate) fn release_magazine(&self, mag: Magazine) {
        assert!(mag.0.link.is_none());

        if mag.is_empty() {
            self.rack.release_empty_magazine(mag);
        } else if mag.is_full() {
            self.full_mags.push(mag);
        } else {
            self.partial_mags.push(mag);
        }
    }
}

#[test]
fn smoke_test_rack() {
    let rack = get_default_rack();
    let mag = rack.allocate_empty_magazine();

    rack.release_empty_magazine(mag);
}

#[test]
fn magazine_stack_smoke_test() {
    let rack = get_default_rack();
    let stack = MagazineStack::new();

    stack.push(rack.allocate_empty_magazine());
    stack.push(rack.allocate_empty_magazine());

    assert!(stack.pop().is_some());

    stack.push(rack.allocate_empty_magazine());
    assert!(stack.pop().is_some());
    assert!(stack.pop().is_some());

    assert!(stack.pop().is_none());
}
