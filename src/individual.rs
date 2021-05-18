//! This module services individual allocation and deallocation calls,
//! i.e., the majority of public calls into Slitter.
use std::alloc::GlobalAlloc;
use std::alloc::System;
use std::ffi::c_void;
use std::ptr::NonNull;

use crate::cache;
use crate::class::Class;
use crate::class::ClassInfo;
use crate::linear_ref::LinearRef;

impl Class {
    #[inline(always)]
    pub fn allocate(self) -> Option<NonNull<c_void>> {
        cache::allocate(self).map(|x| x.convert_to_non_null())
    }

    #[inline(always)]
    pub fn release(self, block: NonNull<c_void>) {
        cache::release(self, LinearRef::new(block));
    }
}

impl ClassInfo {
    #[inline(never)]
    pub(crate) fn allocate_slow(&self) -> Option<LinearRef> {
        let layout = self.layout;
        let offset = self.offset;

        let allocated = unsafe { System.alloc(layout) as *mut u8 };
        if allocated.is_null() {
            return None;
        }

        Some(LinearRef::new(NonNull::new(
            allocated.wrapping_add(offset) as *mut c_void,
        )?))
    }

    #[inline(never)]
    pub(crate) fn release_slow(&self, block: LinearRef) {
        let layout = self.layout;
        let offset = self.offset;
        let ptr = (block.convert_to_non_null().as_ptr() as *mut u8).wrapping_sub(offset);

        unsafe {
            System.dealloc(ptr, layout);
        }
    }
}
