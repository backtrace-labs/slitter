//! A `Press` creates new allocations for a given `Class`'s data.
use crate::linear_ref::LinearRef;
use std::alloc::GlobalAlloc;
use std::alloc::Layout;
use std::alloc::System;
use std::ffi::c_void;
use std::ptr::NonNull;

pub struct Press {
    layout: Layout,
}

impl Press {
    pub fn new(layout: Layout) -> Self {
        Self { layout }
    }

    pub fn allocate_one_object(&self) -> Option<LinearRef> {
        let allocated = unsafe { System.alloc(self.layout) as *mut c_void };
        if allocated.is_null() {
            return None;
        }

        Some(LinearRef::new(NonNull::new(allocated)?))
    }
}
