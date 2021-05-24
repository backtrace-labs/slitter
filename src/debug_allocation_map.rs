//! This module tracks the internal status of allocated objects in
//! debug builds.
use std::collections::HashMap;
use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::Mutex;

use crate::Class;

struct AllocationInfo {
    class: Class,
    live: bool, // True if owned by the mutator
}

lazy_static::lazy_static! {
    static ref ALLOCATION_STATE_MAP: Mutex<HashMap<usize, AllocationInfo>> = Default::default();
}

/// Confirms that it makes sense to return this allocation to the mutator.
pub fn can_be_allocated(class: Class, alloc: &NonNull<c_void>) -> Result<(), &'static str> {
    let map = ALLOCATION_STATE_MAP.lock().unwrap();

    if let Some(info) = map.get(&(alloc.as_ptr() as usize)) {
        if info.class != class {
            return Err("class mismatch");
        }

        if info.live {
            return Err("double allocation");
        }
    }

    Ok(())
}

/// Marks this allocation as returned to the mutator.
pub fn mark_allocated(class: Class, alloc: &NonNull<c_void>) -> Result<(), &'static str> {
    let mut map = ALLOCATION_STATE_MAP.lock().unwrap();
    let mut info = map
        .entry(alloc.as_ptr() as usize)
        .or_insert(AllocationInfo { class, live: false });

    if info.class != class {
        return Err("class mismatch");
    }

    if info.live {
        return Err("double allocation");
    }

    info.live = true;
    Ok(())
}

/// Marks this allocation as released by the mutator.
pub fn mark_released(class: Class, alloc: &NonNull<c_void>) -> Result<(), &'static str> {
    let mut map = ALLOCATION_STATE_MAP.lock().unwrap();
    let mut info = map
        .get_mut(&(alloc.as_ptr() as usize))
        .ok_or("Released unknown address")?;

    if info.class != class {
        return Err("class mismatch");
    }

    if !info.live {
        return Err("double free");
    }

    info.live = false;
    Ok(())
}

/// Confirms that the allocation has been released by the mutator
pub fn has_been_released(class: Class, alloc: &NonNull<c_void>) -> Result<(), &'static str> {
    let map = ALLOCATION_STATE_MAP.lock().unwrap();
    let info = map
        .get(&(alloc.as_ptr() as usize))
        .ok_or("Released unknown address")?;

    if info.class != class {
        return Err("class mismatch");
    }

    if info.live {
        return Err("released a live allocation");
    }

    Ok(())
}
