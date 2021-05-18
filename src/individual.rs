//! This module services individual allocation and deallocation calls,
//! i.e., the majority of public calls into Slitter.
use std::alloc::GlobalAlloc;
use std::alloc::System;
use std::ffi::c_void;
use std::ptr::NonNull;

use crate::cache;
use crate::class::Class;
use crate::class::ClassInfo;

impl Class {
    #[inline(always)]
    pub fn allocate(self) -> Option<NonNull<c_void>> {
        cache::allocate(self)
    }

    #[inline(always)]
    pub fn release(self, block: NonNull<c_void>) {
        cache::release(self, block);
    }
}

impl ClassInfo {
    #[inline(never)]
    pub(crate) fn allocate_slow(&self) -> Option<NonNull<c_void>> {
        let layout = self.layout;
        let offset = self.offset;

        NonNull::new(unsafe { System.alloc(layout) as *mut u8 }.wrapping_add(offset) as *mut c_void)
    }

    #[inline(never)]
    pub(crate) fn release_slow(&self, block: NonNull<c_void>) {
        let layout = self.layout;
        let offset = self.offset;
        let ptr = (block.as_ptr() as *mut u8).wrapping_sub(offset);

        unsafe {
            System.dealloc(ptr, layout);
        }
    }
}
