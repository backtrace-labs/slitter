//! This module tracks the type of allocated addresses in debug builds.
use std::collections::BTreeMap;
use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::Mutex;

use crate::linear_ref::LinearRef;
use crate::Class;

struct TypeInfo {
    begin: usize,
    size: usize,
    class: Class,
}

lazy_static::lazy_static! {
    static ref ALLOCATION_TYPE_MAP: Mutex<BTreeMap<usize, TypeInfo>> = Default::default();
}

/// Ensures this allocation is fresh and associates it with `class`.
pub fn associate_class(class: Class, alloc: &LinearRef) -> Result<(), &'static str> {
    let info = class.info();
    let begin = alloc.get().as_ptr() as usize;
    let size = info.layout.size();

    if usize::MAX - begin < size {
        return Err("Address is too high.");
    }

    let mut map = ALLOCATION_TYPE_MAP.lock().unwrap();

    // Make sure nothing overlaps with the allocation.
    for (_, info) in map.range(0..=(begin + size)).rev() {
        // We're walking allocations from the top down.  If the
        // current allocation is too high, keep looking.
        if info.begin >= begin + size {
            continue;
        }

        // If the current allocation is too low, stop.
        if begin >= info.begin + info.size {
            break;
        }

        return Err("Found overlapping allocation");
    }

    map.insert(begin, TypeInfo { begin, size, class });
    Ok(())
}

/// Checks whether the `alloc`ation is of type `class`.
pub fn ptr_is_class(class: Class, alloc: &NonNull<c_void>) -> Result<(), &'static str> {
    let begin = alloc.as_ptr() as usize;
    let map = ALLOCATION_TYPE_MAP.lock().unwrap();

    let entry = map.get(&begin).ok_or("Allocation not found")?;
    if entry.class != class {
        return Err("Allocation class mismatch");
    }

    Ok(())
}

/// Checks whether the `alloc`ation is of type `class`.
pub fn is_class(class: Class, alloc: &LinearRef) -> Result<(), &'static str> {
    ptr_is_class(class, alloc.get())
}
