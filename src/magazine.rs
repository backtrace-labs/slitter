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

const MAGAZINE_SIZE: u32 = 30;

/// A Magazine is a thin wrapper around MagazineImpl: the wrapping
/// lets us impose a tighter contract on the interface used in the
/// allocator, while keeping the internal implementation testable.
#[repr(transparent)]
pub struct Magazine(Box<MagazineImpl>);

#[repr(C)] // We play fun tricks with zero-initialisation.
struct MagazineImpl {
    /// The `allocations` array is populated from the bottom up;
    /// the first `num_allocated` indices have NonNull values,
    /// and the remainder are undefined.
    num_allocated: u32,
    allocations: [Option<LinearRef>; MAGAZINE_SIZE as usize],

    /// Single linked list linkage.
    link: Option<Box<MagazineImpl>>,
}

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
            for maybe_alloc in &self.0.allocations {
                if let Some(alloc) = maybe_alloc {
                    debug_allocation_map::can_be_allocated(class, alloc.get())?;
                    debug_type_map::is_class(class, alloc)?;
                    press::check_allocation(class, alloc.get().as_ptr() as usize)?;
                }
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

impl MagazineImpl {
    /// Checks that the current object's state is valid.
    #[cfg(any(
        all(test, feature = "check_contracts_in_tests"),
        feature = "check_contracts"
    ))]
    fn check_rep(&self) -> bool {
        let allocated = self.num_allocated as usize;

        // Everything before `allocated` must be populated,
        // and everything else empty.
        self.allocations.iter().enumerate().all(|(i, entry)| {
            if i < allocated {
                entry.is_some()
            } else {
                entry.is_none()
            }
        })
    }

    /// Attempts to get an unused block from the magazine.
    #[invariant(self.check_rep(), "Representation makes sense.")]
    #[ensures(old(self.is_empty()) == ret.is_none(),
              "We only fail to pop from empty magazines.")]
    pub fn get(&mut self) -> Option<LinearRef> {
        if self.num_allocated == 0 {
            return None;
        }

        self.num_allocated -= 1;
        self.allocations[self.num_allocated as usize].take()
    }

    /// Attempts to put an unused block back in the magazine.
    ///
    /// Returns that unused block on failure.
    #[invariant(self.check_rep())]
    #[ensures(ret.is_some() -> old(freed.get().as_ptr()) == ret.as_ref().unwrap().get().as_ptr(),
              "On failure, we return `freed`.")]
    #[ensures(ret.is_none() -> old(freed.get().as_ptr()) == self.allocations[self.num_allocated as usize - 1].as_ref().unwrap().get().as_ptr(),
              "On success, `freed` is in the magazine.")]
    #[ensures(old(self.is_full()) == ret.is_some(),
              "We only fail to push to full magazines.")]
    pub fn put(&mut self, freed: LinearRef) -> Option<LinearRef> {
        let index = self.num_allocated;
        if index >= MAGAZINE_SIZE {
            return Some(freed);
        }

        self.num_allocated += 1;
        self.allocations[index as usize] = Some(freed);
        None
    }

    /// Fills `self` with allocations returned by `allocator`
    #[invariant(self.check_rep())]
    #[ensures(self.num_allocated >= old(self.num_allocated),
              "We should never lose allocations.")]
    pub fn populate(&mut self, mut allocator: impl FnMut() -> Option<LinearRef>) {
        let mut count = self.num_allocated as usize;

        while count < MAGAZINE_SIZE as usize {
            match allocator() {
                Some(block) => self.allocations[count] = Some(block),
                None => break,
            }

            count += 1;
        }

        self.num_allocated = count as u32;
    }

    #[invariant(self.check_rep())]
    #[inline]
    pub fn is_full(&self) -> bool {
        self.num_allocated == MAGAZINE_SIZE
    }

    #[invariant(self.check_rep())]
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.num_allocated == 0
    }
}

impl Default for MagazineImpl {
    fn default() -> Self {
        // Proof that MagazineImpl its constituents are FFI-safe.
        #[allow(dead_code)]
        extern "C" fn unused(
            _mag: MagazineImpl,
            _ref: Option<LinearRef>,
            _link: Option<Box<MagazineImpl>>,
        ) {
        }

        Self {
            num_allocated: 0,
            allocations: Default::default(),
            link: None,
        }
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
fn smoke_test_magazine() {
    let rack = get_default_rack();
    let mut mag = rack.allocate_empty_magazine().0;

    // Getting an empty magazine should return None
    assert_eq!(mag.get(), None); // mag: []

    // And getting it again should still return None.
    assert_eq!(mag.get(), None); // mag: []

    assert_eq!(mag.put(LinearRef::from_address(1)), None); // mag: [1]
    assert_eq!(mag.put(LinearRef::from_address(2)), None); // mag: [1, 2]

    {
        let popped = mag.get().expect("should have a value"); // mag: [1]

        assert_eq!(popped.get().as_ptr() as usize, 2);
        std::mem::forget(popped);
    }

    assert_eq!(mag.put(LinearRef::from_address(3)), None); // mag: [1, 3]

    {
        let popped = mag.get().expect("should have a value");

        assert_eq!(popped.get().as_ptr() as usize, 3); // mag: [1]
        std::mem::forget(popped);
    }

    {
        let popped = mag.get().expect("should have a value");

        assert_eq!(popped.get().as_ptr() as usize, 1); // mag: []
        std::mem::forget(popped);
    }

    rack.release_empty_magazine(Magazine(mag));
}

#[test]
fn magazine_fill_up() {
    let rack = get_default_rack();
    let mut mag = rack.allocate_empty_magazine().0;

    // Fill up the magazine.
    for i in 1..=MAGAZINE_SIZE {
        assert_eq!(mag.num_allocated, i - 1);
        assert_eq!(mag.put(LinearRef::from_address(i as usize)), None);
        assert_eq!(mag.num_allocated, i);
    }

    // This insert should fail
    let failed_insert = mag
        .put(LinearRef::from_address(usize::MAX))
        .expect("should fail");
    assert_eq!(failed_insert.get().as_ptr() as usize, usize::MAX);
    std::mem::forget(failed_insert);

    assert_eq!(mag.num_allocated, MAGAZINE_SIZE);

    // We should pop in LIFO order.
    for i in (1..=MAGAZINE_SIZE).rev() {
        assert_eq!(mag.num_allocated, i);
        let popped = mag.get().expect("has value");
        assert_eq!(popped.get().as_ptr() as usize, i as usize);
        std::mem::forget(popped);

        assert_eq!(mag.num_allocated, i - 1);
    }

    // And now the magazine should be empty.
    assert_eq!(mag.num_allocated, 0);
    // So all subsequent `get()` calls will return None.
    assert_eq!(mag.get(), None);
    assert_eq!(mag.get(), None);
    assert_eq!(mag.num_allocated, 0);

    rack.release_empty_magazine(Magazine(mag));
}

#[test]
fn magazine_populate() {
    let rack = get_default_rack();
    let mut mag = rack.allocate_empty_magazine().0;

    // Fill up the magazine.
    let mut count = 0usize;
    mag.populate(|| {
        count += 1;
        Some(LinearRef::from_address(count))
    });

    assert_eq!(mag.num_allocated, MAGAZINE_SIZE);

    // We should pop in LIFO order.
    for i in (1..=MAGAZINE_SIZE).rev() {
        assert_eq!(mag.num_allocated, i);
        let popped = mag.get().expect("has value");
        assert_eq!(popped.get().as_ptr() as usize, i as usize);
        std::mem::forget(popped);

        assert_eq!(mag.num_allocated, i - 1);
    }

    // And now the magazine should be empty.
    assert_eq!(mag.num_allocated, 0);

    rack.release_empty_magazine(Magazine(mag));
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
