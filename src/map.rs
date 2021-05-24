//! Rust bindings for the support code in C that calls out to mmap.
//!
//! TODO: wrap strerror_r usefully.
use std::ffi::c_void;
use std::ptr::NonNull;

// These helpers are declared in `c/map.h`.
extern "C" {
    fn slitter__page_size() -> i64;
    fn slitter__reserve_region(size: usize, OUT_errno: *mut i32) -> Option<NonNull<c_void>>;
    fn slitter__release_region(base: NonNull<c_void>, size: usize) -> i32;
    fn slitter__allocate_region(base: NonNull<c_void>, size: usize) -> i32;
}

fn page_size_or_die() -> usize {
    let ret = unsafe { slitter__page_size() };

    if ret <= 0 {
        panic!("Unable to find page_size: errno={}", -ret);
    }

    ret as usize
}

lazy_static::lazy_static! {
    static ref PAGE_SIZE: usize = page_size_or_die();
}

/// Returns the system page size.
#[inline]
pub fn page_size() -> usize {
    *PAGE_SIZE
}

/// Attempts to reserve an *address space* region of `size` bytes.
///
/// The `size` argument must be a multiple of the page size.
pub fn reserve_region(size: usize) -> Result<NonNull<c_void>, i32> {
    let mut errno: i32 = 0;

    assert!(
        size > 0 && (size % page_size()) == 0,
        "Bad region size={} page_size={}",
        size,
        page_size()
    );

    if let Some(base) = unsafe { slitter__reserve_region(size, &mut errno) } {
        Ok(base)
    } else {
        Err(errno)
    }
}

/// Releases a region of `size` bytes starting at `base`.
///
/// The size argument must be a multiple of the page size.
pub fn release_region(base: NonNull<c_void>, size: usize) -> Result<(), i32> {
    if size == 0 {
        return Ok(());
    }

    assert!(
        (size % page_size()) == 0,
        "Bad region size={} page_size={}",
        size,
        page_size()
    );

    let ret = unsafe { slitter__release_region(base, size) };

    if ret == 0 {
        Ok(())
    } else {
        Err(-ret)
    }
}

/// Backs a region of `size` bytes starting at `base` with
/// (demand-faulted) memory.
///
/// The size argument must be a multiple of the page size.
pub fn allocate_region(base: NonNull<c_void>, size: usize) -> Result<(), i32> {
    if size == 0 {
        return Ok(());
    }

    assert!(
        (size % page_size()) == 0,
        "Bad region size={} page_size={}",
        size,
        page_size()
    );

    let ret = unsafe { slitter__allocate_region(base, size) };

    if ret == 0 {
        Ok(())
    } else {
        Err(-ret)
    }
}

#[test]
fn test_page_size() {
    assert_ne!(page_size(), 0);

    // We only develop on platforms with 4K pages.
    assert_eq!(page_size(), 4096);
}

// Simulate a data + metadata allocation workflow: overallocate, trim
// the slop, and ask for real memory in some of the remaining space.
#[test]
fn smoke_test() {
    let region_size = 1usize << 21;
    let mut base = reserve_region(3 * region_size).expect("reserve should succeed");

    assert!(region_size > 3 * page_size());

    // We overallocated `base` by 3x.  Drop the bottom and top
    // `region_size` bytes from the range.
    release_region(base, region_size).expect("should release the bottom slop");
    base = NonNull::new((base.as_ptr() as usize + region_size) as *mut c_void)
        .expect("Should be non-null");

    let top_slop = NonNull::new((base.as_ptr() as usize + region_size) as *mut c_void)
        .expect("Should be non-null");
    release_region(top_slop, region_size).expect("should release the top slop");

    // Conceptually split the region in three ranges: a one-page
    // region at the base, a guard page just after, and the rest.
    let bottom = base; // one page
    let _guard = NonNull::new((base.as_ptr() as usize + page_size()) as *mut c_void)
        .expect("Should be non-null");
    let remainder = NonNull::new((base.as_ptr() as usize + 2 * page_size()) as *mut c_void)
        .expect("Should be non-null");

    // Start by allocating the bottom and remainder regions.
    allocate_region(bottom, page_size()).expect("should allocate bottom");
    allocate_region(remainder, region_size - 2 * page_size()).expect("should allocate remainder");

    // And now release everything.
    release_region(base, region_size).expect("should release everything");
}
