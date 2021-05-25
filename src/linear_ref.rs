//! A `LinearRef` is a `NonNull<c_void>` that can't be copied or
//! cloned.  We use it internally in Slitter to make it harder to
//! accidentally duplicate allocations.
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

use std::ffi::c_void;
use std::ptr::NonNull;

#[derive(Debug)]
#[repr(transparent)]
pub struct LinearRef {
    inner: NonNull<c_void>,
}

/// `LinearRef` wrap allocations to help Slitter avoid duplicating
/// allocations.
impl LinearRef {
    /// Creates a new `LinearRef` from a `NonNull`.  That `inner`
    /// `NonNull` must be the unique reference to that address.
    ///
    /// This function should only be used when directly interacting
    /// with external code (e.g., callers, the system allocator, or
    /// newly mapped pages).
    #[allow(clippy::assertions_on_constants)]
    #[requires(true, "`inner` must be unique (check manually)")]
    #[inline(always)]
    pub fn new(inner: NonNull<c_void>) -> Self {
        Self { inner }
    }

    /// Converts a `LinearRef` to a `NonNull`.
    ///
    /// This function should only be used when directly interacting
    /// with external code (e.g., when returning an allocation to a
    /// caller).
    #[inline(always)]
    pub fn convert_to_non_null(self) -> NonNull<c_void> {
        #[allow(clippy::let_and_return)]
        let ret = self.inner;

        #[cfg(any(
            all(test, feature = "check_contracts_in_tests"),
            feature = "check_contracts"
        ))]
        std::mem::forget(self);
        ret
    }

    /// Returns a `LinearRef` for an arbitrary non-zero integer
    #[cfg(test)]
    pub fn from_address(address: usize) -> Self {
        Self::new(NonNull::new(address as *mut c_void).expect("should not be zero"))
    }

    /// Only used for test and contracts: returns a reference to the
    /// underlying `NonNull`.
    #[cfg(any(test, feature = "check_contracts"))]
    pub(crate) fn get(&self) -> &NonNull<c_void> {
        &self.inner
    }
}

#[cfg(any(
    all(test, feature = "check_contracts_in_tests"),
    feature = "check_contracts"
))]
impl Drop for LinearRef {
    #[allow(clippy::assertions_on_constants)]
    #[requires(false, "LinearRef should never be dropped.")]
    fn drop(&mut self) {}
}

impl PartialEq for LinearRef {
    fn eq(&self, other: &Self) -> bool {
        self.inner.as_ptr() == other.inner.as_ptr()
    }
}

impl Eq for LinearRef {}

// It's safe to send LinearRef, because linearity means there's only
// one reference to the underlying address, and thus only one thread
// at a time has access to the data.
unsafe impl Send for LinearRef {}
