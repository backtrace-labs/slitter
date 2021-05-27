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

use std::mem::MaybeUninit;
use std::ptr::NonNull;

use crate::linear_ref::LinearRef;

#[cfg(not(feature = "test_only_small_constants"))]
pub const MAGAZINE_SIZE: u32 = 30;

#[cfg(feature = "test_only_small_constants")]
pub const MAGAZINE_SIZE: u32 = 6;

/// The `MagazineStorage` is the heap-allocated storage for a
/// magazine.
///
/// The same struct is available in C as `struct magazine_storage`, in
/// `mag.h`.
#[repr(C)]
pub struct MagazineStorage {
    /// The `allocations` array is populated from the bottom up; the
    /// first `num_allocated_slow` indices have values, and the
    /// remainder are uninitialised.
    ///
    /// This field may not be accurate when wrapped in a `MagazineImpl`.
    num_allocated_slow: u32,
    allocations: [MaybeUninit<LinearRef>; MAGAZINE_SIZE as usize],

    /// Single linked list linkage.
    pub(crate) link: Option<NonNull<MagazineStorage>>,
}

/// The `MagazineImpl` is the actual implementation for the storage.
/// This split lets us cache information inline.
///
/// The same struct is available in C as `struct magazine`, in
/// `mag.h`.
///
/// If `PUSH_MAG` is true, we have a push-only magazine.  If it's
/// false, we have a pop-only magazine.
#[repr(C)]
pub struct MagazineImpl<const PUSH_MAG: bool> {
    /// "Pop" (PUSH_MAG = false) magazines decrement the `top_of_stack`
    /// from MAGAZINE_SIZE down to 0.
    ///
    /// "Push" (PUSH_MAG = true) magazines increment the `top_of_stack`
    /// from `-MAGAZINE_SIZE` up to 0.
    ///
    /// The backing storate expects the opposite direction than the
    /// Push strategy, so we must convert when going in and out of the
    /// raw `MagazineStorage` representation.
    top_of_stack: isize,

    /// Always populated when `top_of_stack != 0`.
    inner: Option<&'static mut MagazineStorage>,
}

impl<const PUSH_MAG: bool> MagazineImpl<PUSH_MAG> {
    /// Wraps `maybe_inner` in an impl.  If `maybe_inner` is None, the
    /// a push magazine is initialised to full, and a pop to empty.
    // Disabled precondition: lifetimes are too hard for contracts.
    // #[requires(inner.link.is_none())]
    #[inline(always)]
    pub fn new(maybe_inner: Option<&'static mut MagazineStorage>) -> Self {
        if let Some(inner) = maybe_inner {
            #[cfg(any(
                all(test, feature = "check_contracts_in_tests"),
                feature = "check_contracts"
            ))]
            assert!(inner.link.is_none());

            if PUSH_MAG {
                Self {
                    top_of_stack: inner.num_allocated_slow as isize - MAGAZINE_SIZE as isize,
                    inner: Some(inner),
                }
            } else {
                Self {
                    top_of_stack: inner.num_allocated_slow as isize,
                    inner: Some(inner),
                }
            }
        } else {
            Self {
                top_of_stack: 0,
                inner: None,
            }
        }
    }

    /// Returns whether this magazine is backed by real storage, and
    /// thus has capacity.
    #[inline(always)]
    pub fn has_storage(&self) -> bool {
        self.inner.is_some()
    }

    // Disabled postcondition: lifetimes are too hard for contracts.
    // #[requires(self.check_rep())]
    // #[ensures(ret.link.is_none())]
    #[inline(always)]
    pub fn storage(self) -> Option<&'static mut MagazineStorage> {
        #[cfg(any(
            all(test, feature = "check_contracts_in_tests"),
            feature = "check_contracts"
        ))]
        assert!(self.check_rep());

        let inner = self.inner?;
        if PUSH_MAG {
            inner.num_allocated_slow = (MAGAZINE_SIZE as isize + self.top_of_stack) as u32;
        } else {
            inner.num_allocated_slow = self.top_of_stack as u32;
        }

        #[cfg(any(
            all(test, feature = "check_contracts_in_tests"),
            feature = "check_contracts"
        ))]
        assert!(inner.link.is_none());
        Some(inner)
    }

    #[invariant(self.check_rep())]
    #[inline]
    pub fn is_full(&self) -> bool {
        if PUSH_MAG {
            self.top_of_stack == 0
        } else {
            self.top_of_stack == MAGAZINE_SIZE as isize
        }
    }

    #[invariant(self.check_rep())]
    #[inline]
    pub fn is_empty(&self) -> bool {
        if PUSH_MAG {
            self.top_of_stack == -(MAGAZINE_SIZE as isize)
        } else {
            self.top_of_stack == 0
        }
    }

    /// Returns the number of elements in the magazine.
    #[cfg(any(test, feature = "check_contracts"))]
    pub fn len(&self) -> usize {
        if PUSH_MAG {
            (self.top_of_stack + MAGAZINE_SIZE as isize) as usize
        } else {
            self.top_of_stack as usize
        }
    }

    /// Returns a reference to the element at `index`.
    ///
    /// Calling this with an index that has no valid value
    /// will cause undefined behafiour.
    #[cfg(any(
        all(test, feature = "check_contracts_in_tests"),
        feature = "check_contracts"
    ))]
    pub fn nth(&self, index: usize) -> Option<&LinearRef> {
        Some(unsafe { &*self.inner.as_ref()?.allocations[index].as_ptr() })
    }

    /// Checks that the current object's state is valid.
    #[cfg(any(
        all(test, feature = "check_contracts_in_tests"),
        feature = "check_contracts"
    ))]
    pub(crate) fn check_rep(&self) -> bool {
        let inner;

        if let Some(storage) = &self.inner {
            inner = storage;
        } else {
            // Missing storage is only allowed when `top_of_stack ==
            // 0`.
            return self.top_of_stack == 0;
        };

        // The storage should never be in a linked stack.
        if inner.link.is_some() {
            return false;
        }

        if PUSH_MAG {
            // The top of stack index should be in [-MAGAZINE_SIZE, 0]
            // for Push magazine.
            if self.top_of_stack < -(MAGAZINE_SIZE as isize) || self.top_of_stack > 0 {
                return false;
            }
        } else {
            // The top of stack index should be in [0, MAGAZINE_SIZE]
            // for Pop magazines.
            if self.top_of_stack < 0 || self.top_of_stack > MAGAZINE_SIZE as isize {
                return false;
            }
        }

        // Everything before `allocated` must be populated, and thus
        // non-NULL.  Everything at or after `allocated` is garbage
        // and must not be read.
        inner
            .allocations
            .iter()
            .take(self.len())
            .all(|entry| !entry.as_ptr().is_null())
    }
}

impl<const PUSH_MAG: bool> Default for MagazineImpl<PUSH_MAG> {
    #[inline(always)]
    fn default() -> Self {
        Self::new(None)
    }
}

// Logic for "push" magazines.
impl MagazineImpl<true> {
    /// Converts to a Pop magazine
    #[cfg(test)]
    pub fn into_pop(self) -> MagazineImpl<false> {
        MagazineImpl::new(self.storage())
    }

    /// Attempts to put an unused block back in the magazine.
    ///
    /// Returns that unused block on failure.
    #[invariant(self.check_rep())]
    #[ensures(ret.is_none() -> self.top_of_stack == old(self.top_of_stack) + 1,
              "We add one element on success.")]
    #[ensures(ret.is_some() -> self.top_of_stack == old(self.top_of_stack),
              "We don't change the stack on failure.")]
    #[ensures(ret.is_some() -> old(freed.get().as_ptr()) == ret.as_ref().unwrap().get().as_ptr(),
              "On failure, we return `freed`.")]
    #[ensures(ret.is_none() -> old(freed.get().as_ptr()) == self.peek(),
              "On success, `freed` is in the magazine.")]
    #[ensures(old(self.is_full()) == ret.is_some(),
              "We only fail to push to full magazines.")]
    #[inline(always)]
    pub fn put(&mut self, freed: LinearRef) -> Option<LinearRef> {
        let index = self.top_of_stack;
        if index == 0 {
            return Some(freed);
        }

        self.top_of_stack += 1;
        unsafe {
            self.inner
                .as_mut()
                .expect("non-zero top_of_stack must have a storage")
                .allocations[(MAGAZINE_SIZE as isize + index) as usize]
                .as_mut_ptr()
                .write(freed);
        }
        None
    }

    /// Contract-only: returns the pointer at the top of the stack, of NULL if none.
    #[cfg(any(
        all(test, feature = "check_contracts_in_tests"),
        feature = "check_contracts"
    ))]
    fn peek(&self) -> *mut c_void {
        if self.top_of_stack == -(MAGAZINE_SIZE as isize) {
            std::ptr::null::<c_void>() as *mut _
        } else {
            unsafe {
                self.inner
                    .as_ref()
                    .expect("non-zero top_of_stack must have a storage")
                    .allocations[(MAGAZINE_SIZE as isize + self.top_of_stack) as usize - 1]
                    .as_ptr()
                    .as_ref()
            }
            .unwrap()
            .get()
            .as_ptr()
        }
    }
}

// Functions that only exist on "pop" magazines.
impl MagazineImpl<false> {
    /// Converts to a Push magazine
    #[cfg(test)]
    pub fn into_push(self) -> MagazineImpl<true> {
        MagazineImpl::new(self.storage())
    }

    /// Attempts to get an unused block from the magazine.
    #[invariant(self.check_rep(), "Representation makes sense.")]
    #[ensures(old(self.is_empty()) == ret.is_none(),
              "We only fail to pop from empty magazines.")]
    #[ensures(ret.is_none() -> self.top_of_stack == old(self.top_of_stack),
              "We don't change the stack size on failure.")]
    #[ensures(ret.is_some() -> self.top_of_stack == old(self.top_of_stack) - 1,
              "Must remove one element on success.")]
    #[ensures(ret.is_some() -> ret.as_ref().unwrap().get().as_ptr() == old(self.peek()),
              "Must return the top of stack on success.")]
    #[inline(always)]
    pub fn get(&mut self) -> Option<LinearRef> {
        if self.top_of_stack == 0 {
            return None;
        }

        self.top_of_stack -= 1;
        let mut old = MaybeUninit::uninit();
        std::mem::swap(
            &mut old,
            &mut self
                .inner
                .as_mut()
                .expect("non-zero top_of_stack must have a storage")
                .allocations[self.top_of_stack as usize],
        );
        Some(unsafe { old.assume_init() })
    }

    /// Returns a slice for the unused slots in the magazine
    // No invariant: they confuse the borrow checker.
    #[inline(always)]
    pub fn get_unpopulated(&mut self) -> &mut [MaybeUninit<LinearRef>] {
        if let Some(inner) = &mut self.inner {
            &mut inner.allocations[self.top_of_stack as usize..]
        } else {
            &mut []
        }
    }

    /// Marks the first `count` unused slots in the magazine as now populated.
    #[invariant(self.check_rep())]
    #[requires(count <= MAGAZINE_SIZE as usize - self.top_of_stack as usize)]
    #[inline(always)]
    pub fn commit_populated(&mut self, count: usize) {
        self.top_of_stack += count as isize;
    }

    /// Contract-only: returns the pointer at the top of the stack, of NULL if none.
    #[cfg(any(
        all(test, feature = "check_contracts_in_tests"),
        feature = "check_contracts"
    ))]
    fn peek(&self) -> *mut c_void {
        if self.top_of_stack == 0 {
            std::ptr::null::<c_void>() as *mut _
        } else {
            unsafe {
                self.inner
                    .as_ref()
                    .expect("non-zero top_of_stack must have a storage")
                    .allocations[self.top_of_stack as usize - 1]
                    .as_ptr()
                    .as_ref()
            }
            .unwrap()
            .get()
            .as_ptr()
        }
    }
}

impl Default for MagazineStorage {
    fn default() -> Self {
        // Proof that MagazineImpl its constituents are FFI-safe.
        #[allow(dead_code)]
        extern "C" fn unused(
            _mag: MagazineStorage,
            _ref: Option<LinearRef>,
            _link: Option<Box<MagazineStorage>>,
        ) {
        }

        Self {
            num_allocated_slow: 0,
            // Safe to leave this as garbage: we never read past
            // `num_allocated_slow`.
            allocations: unsafe { MaybeUninit::uninit().assume_init() },
            link: None,
        }
    }
}

/// We should only drop empty magazines.
impl Drop for MagazineStorage {
    #[requires(self.num_allocated_slow == 0,
               "Only empty magazines can be dropped.")]
    fn drop(&mut self) {}
}

#[test]
fn smoke_test_magazine() {
    let rack = crate::rack::get_default_rack();
    let mut mag = rack.allocate_empty_magazine::</*PUSH_MAG=*/false>().0;

    // Getting an empty magazine should return None
    assert_eq!(mag.get(), None); // mag: []

    // And getting it again should still return None.
    assert_eq!(mag.get(), None); // mag: []

    let mut mag2 = mag.into_push();

    assert_eq!(mag2.put(LinearRef::from_address(1)), None); // mag: [1]
    assert_eq!(mag2.put(LinearRef::from_address(2)), None); // mag: [1, 2]

    let mut mag3 = mag2.into_pop();
    {
        let popped = mag3.get().expect("should have a value"); // mag: [1]

        assert_eq!(popped.get().as_ptr() as usize, 2);
        std::mem::forget(popped);
    }

    let mut mag4 = mag3.into_push();
    assert_eq!(mag4.put(LinearRef::from_address(3)), None); // mag: [1, 3]

    let mut mag5 = mag4.into_pop();
    {
        let popped = mag5.get().expect("should have a value");

        assert_eq!(popped.get().as_ptr() as usize, 3); // mag: [1]
        std::mem::forget(popped);
    }

    {
        let popped = mag5.get().expect("should have a value");

        assert_eq!(popped.get().as_ptr() as usize, 1); // mag: []
        std::mem::forget(popped);
    }

    rack.release_empty_magazine(crate::magazine::Magazine(mag5));
}

#[test]
fn magazine_fill_up() {
    let rack = crate::rack::get_default_rack();
    let mut mag = rack.allocate_empty_magazine::</*PUSH_MAG=*/true>().0;

    // Fill up the magazine.
    for i in 1..=MAGAZINE_SIZE as usize {
        assert_eq!(mag.len(), i - 1);
        assert_eq!(mag.put(LinearRef::from_address(i)), None);
        assert_eq!(mag.len(), i);
    }

    // This insert should fail
    let failed_insert = mag
        .put(LinearRef::from_address(usize::MAX))
        .expect("should fail");
    assert_eq!(failed_insert.get().as_ptr() as usize, usize::MAX);
    std::mem::forget(failed_insert);

    assert_eq!(mag.len(), MAGAZINE_SIZE as usize);

    let mut pop_mag = mag.into_pop();

    // We should pop in LIFO order.
    for i in (1..=MAGAZINE_SIZE as usize).rev() {
        assert_eq!(pop_mag.len(), i);
        let popped = pop_mag.get().expect("has value");
        assert_eq!(popped.get().as_ptr() as usize, i as usize);
        std::mem::forget(popped);

        assert_eq!(pop_mag.len(), i - 1);
    }

    // And now the magazine should be empty.
    assert_eq!(pop_mag.len(), 0);
    // So all subsequent `get()` calls will return None.
    assert_eq!(pop_mag.get(), None);
    assert_eq!(pop_mag.get(), None);
    assert_eq!(pop_mag.len(), 0);

    rack.release_empty_magazine(crate::magazine::Magazine(pop_mag));
}
