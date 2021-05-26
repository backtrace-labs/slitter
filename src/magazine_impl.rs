//! The base `MagazineImpl` handles the pushing and popping of
//! arbitrary pointers to inline uninit storage.  It does not impose
//! strong contracts; that's the responsibility of its `Magazine`
//! wrapper struct.
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

#[cfg(any(
    all(test, feature = "check_contracts_in_tests"),
    feature = "check_contracts"
))]
use std::ffi::c_void;

use crate::linear_ref::LinearRef;
use std::mem::MaybeUninit;

#[cfg(not(feature = "test_only_small_constants"))]
const MAGAZINE_SIZE: u32 = 30;

#[cfg(feature = "test_only_small_constants")]
const MAGAZINE_SIZE: u32 = 6;

#[repr(C)] // We access this struct from C.
pub struct MagazineImpl {
    /// The `allocations` array is populated from the bottom up;
    /// the first `num_allocated` indices have NonNull values,
    /// and the remainder are undefined.
    pub(crate) num_allocated: u32,
    pub(crate) allocations: [MaybeUninit<LinearRef>; MAGAZINE_SIZE as usize],

    /// Single linked list linkage.
    pub(crate) link: Option<Box<MagazineImpl>>,
}

impl MagazineImpl {
    /// Checks that the current object's state is valid.
    #[cfg(any(
        all(test, feature = "check_contracts_in_tests"),
        feature = "check_contracts"
    ))]
    pub(crate) fn check_rep(&self) -> bool {
        // Everything before `allocated` must be populated, and thus
        // non-NULL.  Everything at or after `allocated` is garbage
        // and must not be read.
        self.allocations
            .iter()
            .take(self.num_allocated as usize)
            .all(|entry| !entry.as_ptr().is_null())
    }

    /// Contract-only: returns the pointer at the top of the stack, of NULL if none.
    #[cfg(any(
        all(test, feature = "check_contracts_in_tests"),
        feature = "check_contracts"
    ))]
    fn peek(&self) -> *mut c_void {
        if self.num_allocated == 0 {
            std::ptr::null::<c_void>() as *mut _
        } else {
            unsafe {
                self.allocations[self.num_allocated as usize - 1]
                    .as_ptr()
                    .as_ref()
            }
            .unwrap()
            .get()
            .as_ptr()
        }
    }

    /// Attempts to get an unused block from the magazine.
    #[invariant(self.check_rep(), "Representation makes sense.")]
    #[ensures(old(self.is_empty()) == ret.is_none(),
              "We only fail to pop from empty magazines.")]
    #[ensures(ret.is_none() -> self.num_allocated == old(self.num_allocated),
              "We don't change the stack size on failure.")]
    #[ensures(ret.is_some() -> self.num_allocated == old(self.num_allocated) - 1,
              "Must remove one element on success.")]
    #[ensures(ret.is_some() -> ret.as_ref().unwrap().get().as_ptr() == old(self.peek()),
              "Must return the top of stack on success.")]
    #[inline(always)]
    pub fn get(&mut self) -> Option<LinearRef> {
        if self.num_allocated == 0 {
            return None;
        }

        self.num_allocated -= 1;
        let mut old = MaybeUninit::uninit();
        std::mem::swap(&mut old, &mut self.allocations[self.num_allocated as usize]);
        Some(unsafe { old.assume_init() })
    }

    /// Attempts to put an unused block back in the magazine.
    ///
    /// Returns that unused block on failure.
    #[invariant(self.check_rep())]
    #[ensures(ret.is_none() -> self.num_allocated == old(self.num_allocated) + 1,
              "We add one element on success.")]
    #[ensures(ret.is_some() -> self.num_allocated == old(self.num_allocated),
              "We don't change the stack on failure.")]
    #[ensures(ret.is_some() -> old(freed.get().as_ptr()) == ret.as_ref().unwrap().get().as_ptr(),
              "On failure, we return `freed`.")]
    #[ensures(ret.is_none() -> old(freed.get().as_ptr()) == self.peek(),
              "On success, `freed` is in the magazine.")]
    #[ensures(old(self.is_full()) == ret.is_some(),
              "We only fail to push to full magazines.")]
    #[inline(always)]
    pub fn put(&mut self, freed: LinearRef) -> Option<LinearRef> {
        let index = self.num_allocated;
        if index >= MAGAZINE_SIZE {
            return Some(freed);
        }

        self.num_allocated += 1;
        unsafe {
            self.allocations[index as usize].as_mut_ptr().write(freed);
        }
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
                Some(block) => unsafe { self.allocations[count].as_mut_ptr().write(block) },
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
            // Safe to leave this as garbage: we never read past
            // `num_allocated`.
            allocations: unsafe { MaybeUninit::uninit().assume_init() },
            link: None,
        }
    }
}

/// We should only drop empty magazines.
impl Drop for MagazineImpl {
    #[requires(self.num_allocated == 0,
               "Only empty magazines can be dropped.")]
    fn drop(&mut self) {}
}

#[test]
fn smoke_test_magazine() {
    let rack = crate::magazine::get_default_rack();
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

    rack.release_empty_magazine(crate::magazine::Magazine(mag));
}

#[test]
fn magazine_fill_up() {
    let rack = crate::magazine::get_default_rack();
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

    rack.release_empty_magazine(crate::magazine::Magazine(mag));
}

#[test]
fn magazine_populate() {
    let rack = crate::magazine::get_default_rack();
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

    rack.release_empty_magazine(crate::magazine::Magazine(mag));
}
