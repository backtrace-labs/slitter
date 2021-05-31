//! This module services individual allocation and deallocation calls,
//! i.e., the majority of public calls into Slitter.
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

use crate::cache;
use crate::class::Class;
use crate::class::ClassInfo;
use crate::linear_ref::LinearRef;

impl Class {
    /// Attempts to return a newly allocated object for this `Class`.
    #[ensures(ret.is_some() ->
              debug_allocation_map::mark_allocated(self, ret.as_ref().unwrap()).is_ok(),
              "Successful allocations match the class and avoid double-allocation.")]
    #[ensures(ret.is_some() -> debug_type_map::ptr_is_class(self, ret.as_ref().unwrap()).is_ok(),
              "Successful allocations come from an address of the correct class.")]
    #[ensures(ret.is_some() -> press::check_allocation(self, ret.as_ref().unwrap().as_ptr() as usize).is_ok(),
              "Sucessful allocations must have the allocation metadata set correctly.")]
    #[inline(always)]
    pub fn allocate(self) -> Option<NonNull<c_void>> {
        cache::allocate(self).map(|x| x.convert_to_non_null())
    }

    /// Marks an object returned by `allocate` as ready for reuse.
    #[requires(debug_allocation_map::mark_released(self, &block).is_ok(),
               "Released blocks must match the class and not double-free.")]
    #[requires(debug_type_map::ptr_is_class(self, &block).is_ok(),
               "Released blocks come from an address of the correct class.")]
    #[inline(always)]
    pub fn release(self, block: NonNull<c_void>) {
        cache::release(self, LinearRef::new(block));
    }
}

impl ClassInfo {
    /// The `cache` calls into this slow path when its thread-local
    /// storage is being deinitialised.
    #[ensures(ret.is_some() ->
              debug_allocation_map::can_be_allocated(self.id, ret.as_ref().unwrap().get()).is_ok(),
              "Successful allocations are fresh, or match the class and avoid double-allocation.")]
    #[ensures(ret.is_some() ->
              debug_type_map::is_class(self.id, ret.as_ref().unwrap()).is_ok(),
              "Successful allocations come from an address of the correct class.")]
    #[ensures(ret.is_some() ->
              press::check_allocation(self.id, ret.as_ref().unwrap().get().as_ptr() as usize).is_ok(),
              "Sucessful allocations must have the allocation metadata set correctly.")]
    #[inline(never)]
    pub(crate) fn allocate_slow(&self) -> Option<LinearRef> {
        if let Some(mut mag) = self.get_cached_magazine() {
            let allocated = mag.get();
            assert!(allocated.is_some());

            self.release_magazine(mag);
            allocated
        } else {
            // We can assume the press always allocates zero-filled
            // objects: we require that the underlying mapper only
            // give us zero-filled memory.
            self.press.allocate_one_object()
        }
    }

    /// The `cache` calls into this slow path when its thread-local
    /// storage is being deinitialised.
    #[requires(debug_allocation_map::has_been_released(self.id, block.get()).is_ok(),
               "Slow-released blocks went through `Class::release`.")]
    #[requires(debug_type_map::is_class(self.id, &block).is_ok(),
               "Released blocks come from an address of the correct class.")]
    #[requires(press::check_allocation(self.id, block.get().as_ptr() as usize).is_ok(),
               "Deallocated block must have the allocation metadata set correctly.")]
    #[inline(never)]
    pub(crate) fn release_slow(&self, block: LinearRef) {
        let mut mag = self.allocate_non_full_magazine();

        // Deallocation must succeed.
        assert_eq!(mag.put(block), None);
        self.release_magazine(mag);
    }
}
