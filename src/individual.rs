//! This module services individual allocation and deallocation calls,
//! i.e., the majority of public calls into Slitter.
use std::alloc::GlobalAlloc;
use std::alloc::System;
use std::ffi::c_void;
use std::ptr::NonNull;

use crate::class::Class;

impl Class {
    #[inline(always)]
    pub fn allocate(self) -> Option<NonNull<c_void>> {
        let layout = self.info().layout;

        NonNull::new(unsafe { System.alloc(layout) } as *mut c_void)
    }

    #[inline(always)]
    pub fn release(self, block: NonNull<c_void>) {
        let ptr = block.as_ptr() as *mut u8;
        let layout = self.info().layout;

        unsafe {
            System.dealloc(ptr, layout);
        }
    }
}
