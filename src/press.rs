//! A `Press` creates new allocations for a given `Class`.  The
//! allocations must be such that the `Press` can also map valid
//! addresses back to their `Class`.
//!
//! While each class gets its own press, the latter requirement means
//! that the presses must all implement compatible metadata stashing
//! schemes.
//!
//! For now, we assume that each `Press` allocates data linearly (with
//! a bump pointer) from 2 MB-aligned chunks of 2 MB, and hides the
//! corresponding metadata 8 KB *before* that chunk, with a guard page
//! between the chunk's metadata and the actual chunk data, and more
//! guard pages before the metadata and after the chunk itself.
//!
//! We enable mostly lock-free operations by guaranteeing that each
//! chunk and corresponding metadata is immortal once allocated.
use crate::linear_ref::LinearRef;
use crate::map;
use crate::Class;
use std::alloc::Layout;
use std::ffi::c_void;
use std::num::NonZeroU32;
use std::num::NonZeroUsize;
use std::ptr::NonNull;
use std::sync::atomic::AtomicPtr;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::Mutex;

/// Data chunks are naturally aligned to their chunk size.
const DATA_ALIGNMENT: usize = 2 << 20;
const GUARD_PAGE_SIZE: usize = 4096;
const METADATA_PAGE_SIZE: usize = 4096;

/// Grabbing an address space of at least this many bytes should be
/// enough to find a spot for the chunk + its metadata.
///
/// If the first aligned region has enough of a prefix for 2 guard
/// pages and the metadata, we're good.
///
/// Otherwise, the next aligned region is at most `DATA_ALIGNMENT + 2 *
/// GUARD_PAGE_SIZE + METADATA_PAGE_SIZE` in, which must leave at least
/// the suffix `GUARD_PAGE` before the end of the region.
///
/// The code in this module, including this constant, assumes that
/// `DATA_ALIGNMENT` is greater than the total amount of bytes
/// we wish to keep around the data chunk.
const MAPPED_REGION_SIZE: usize = 2 * DATA_ALIGNMENT + 3 * GUARD_PAGE_SIZE + METADATA_PAGE_SIZE;

#[derive(Debug)]
#[repr(C)]
struct ChunkMetadata {
    /// The id of the class for all allocations in this chunk.  We
    /// don't use a `Class` because 0-initialised data must be valid.
    class_id: Option<NonZeroU32>,
    /// Number of elements available in the chunk
    bump_limit: u32,
    /// Number of elements allocated in the chunk
    bump_ptr: AtomicUsize,
    /// Start address for the chunk.
    chunk_begin: usize,
}

#[allow(unused)]
extern "C" {
    fn unused_chunk_metadata_is_zero_safe() -> ChunkMetadata;
}

/// We track the exact way we want to partition a range of address
/// space in an `AllocatedChunk`.
///
/// The space looks like:
///
/// - slop:  [base, bottom_slop_end)
/// - guard: [bottom_stop_end, meta)
/// - meta:  [meta, meta + 4K)
/// - guard: [meta + 4K, data)
/// - data:  [data, top_slop_begin - 4K)
/// - guard: [top_slop_begin - 4K, top_slop_begin),
/// - slop:  [top_slop_begin, top)
///
/// Once the `meta` and `data` regions are successfully allocated, we
/// want to get rid of the bottom and top "slop" regions, then
/// everything remains immortal.
///
/// On failure, we must instead release everything from `base` to `top`.
#[derive(Debug)]
struct AllocatedChunk {
    // All these `usize` are addresses.
    base: NonZeroUsize,     // page-aligned
    top: NonZeroUsize,      // page-aligned
    bottom_slop_end: usize, // page-aligned
    pub meta: *mut ChunkMetadata,
    pub data: *mut c_void,
    pub data_end: usize,
    top_slop_begin: usize, // page-aligned
}

#[derive(Debug)]
pub struct Press {
    /// The current chunk that services bump pointer allocation.
    bump: AtomicPtr<ChunkMetadata>,

    /// Writes to the bump itself (i.e., updating the `AtomicPtr` itself) go
    /// through the `bump_replace_lock`.
    bump_replace_lock: Mutex<()>,
    layout: Layout,
    class: Class,
}

impl ChunkMetadata {
    /// Maps a Press-allocated address to its metadata.
    pub fn from_allocation_address(address: usize) -> *mut ChunkMetadata {
        let base = address - (address % DATA_ALIGNMENT);
        let meta = base - GUARD_PAGE_SIZE - METADATA_PAGE_SIZE;

        meta as *mut ChunkMetadata
    }
}

impl AllocatedChunk {
    /// Attempts to carve out a chunk + metadata in `[base, base + size)`.
    pub fn new(base: NonZeroUsize, size: usize) -> Result<AllocatedChunk, &'static str> {
        // Try to find the data address.  It must be aligned to 2MB,
        // and have room for the metadata + guard pages before/after
        // the data chunk.
        const PREFIX_SIZE: usize = GUARD_PAGE_SIZE + METADATA_PAGE_SIZE + GUARD_PAGE_SIZE;
        const SUFFIX_SIZE: usize = GUARD_PAGE_SIZE;

        let page_size = map::page_size();

        if (base.get() % page_size) != 0 {
            return Err("base is incorrectly aligned");
        }

        if (size % page_size) != 0 {
            return Err("size is incorrectly aligned");
        }

        // Since we track the exclusive end of the region, this will
        // fail if someone manages to map a region that contains the
        // last page of the address space.  That doesn't happen for
        // userspace on platforms we care about.
        let top = NonZeroUsize::new(
            base.get()
                .checked_add(size)
                .ok_or("input region wraps around")?,
        )
        .expect("must be non-zero");
        let mut data = DATA_ALIGNMENT
            .checked_mul((base.get() / DATA_ALIGNMENT) + 1)
            .ok_or("overflow in alignment")?;

        assert!(data >= base.get());
        // If there's not enough room for the prefix, go forward by
        // another chunk size.
        if (data - base.get()) < PREFIX_SIZE {
            data = data.checked_add(DATA_ALIGNMENT).ok_or("overflow in bump")?;
        }

        // This subtraction is safe before data >= base + PREFIX_SIZE.
        let mut bottom_slop_end = data.checked_sub(PREFIX_SIZE).unwrap();
        // Make sure bottom_slop_end is aligned to a page.  It's always OK to remove *less*.
        bottom_slop_end -= bottom_slop_end % page_size;

        // This addition is safe for the same reason.
        let meta = bottom_slop_end.checked_add(GUARD_PAGE_SIZE).unwrap() as *mut ChunkMetadata;

        let data_end = data
            .checked_add(DATA_ALIGNMENT)
            .ok_or("overflow in data_end")?;
        let mut suffix_end = data_end
            .checked_add(SUFFIX_SIZE)
            .ok_or("overflow in suffix_end")?;
        // Align suffix_end to a page.  It's always OK to remove less,
        // so we bump that up.
        if (suffix_end % page_size) > 0 {
            suffix_end = page_size * (1 + (suffix_end / page_size));
        }

        if suffix_end > top.get() {
            return Err("region too small");
        }

        Ok(AllocatedChunk {
            base,
            top,
            bottom_slop_end,
            meta,
            data: data as *mut c_void,
            data_end,
            top_slop_begin: suffix_end,
        })
    }

    /// Asserts against internal invariants.
    pub fn check_rep(&self) {
        let page_size = map::page_size();

        // Check that [base, top) makes sense.
        assert_eq!(self.base.get() % page_size, 0, "self: {:?}", self);
        assert_eq!(self.top.get() % page_size, 0, "self: {:?}", self);
        assert!(self.base.get() <= self.top.get(), "self: {:?}", self);

        // Now the slop regions.
        assert_eq!(self.bottom_slop_end % page_size, 0, "self: {:?}", self);
        assert_eq!(self.top_slop_begin % page_size, 0, "self: {:?}", self);

        assert!(self.bottom_slop_end >= self.base.get(), "self: {:?}", self);
        assert!(self.top_slop_begin <= self.top.get(), "self: {:?}", self);
        assert!(
            self.bottom_slop_end <= self.top_slop_begin,
            "self: {:?}",
            self
        );

        // The meta region must be between the slops, and must be
        // correctly offset from the data.
        assert!(
            self.meta as usize >= self.bottom_slop_end + GUARD_PAGE_SIZE,
            "self: {:?}",
            self
        );
        assert!(
            self.meta as usize + METADATA_PAGE_SIZE <= self.top_slop_begin,
            "self: {:?}",
            self
        );

        assert_eq!(
            self.meta as usize + METADATA_PAGE_SIZE + GUARD_PAGE_SIZE,
            self.data as usize,
            "self: {:?}",
            self
        );

        // Now do the data.
        assert_eq!(self.data as usize % DATA_ALIGNMENT, 0, "self: {:?}", self);
        assert!(
            self.data as usize >= self.bottom_slop_end,
            "self: {:?}",
            self
        );
        assert!(
            self.data as usize + DATA_ALIGNMENT + GUARD_PAGE_SIZE <= self.top_slop_begin,
            "self: {:?}",
            self
        );

        assert_eq!(
            ChunkMetadata::from_allocation_address(self.data as usize),
            self.meta
        );
        assert_eq!(
            ChunkMetadata::from_allocation_address(self.data as usize + (DATA_ALIGNMENT - 1)),
            self.meta
        );
    }

    /// Attempts to allocate the chunk, and calls `f` if that succeeds.
    ///
    /// If `f` fails, releases the chunk; if `f` succeeds, commits it.
    pub fn call_with_chunk<T>(self, f: impl FnOnce(&Self) -> Result<T, i32>) -> Result<T, i32> {
        self.check_rep();
        self.allocate()?;

        let ret = f(&self);
        if ret.is_err() {
            self.release_all()?;
        } else {
            // TODO: figure out failure logging.
            let _ = self.commit();
        }

        ret
    }

    /// Releases the region covered by the AllocatedChunk.
    fn release_all(self) -> Result<(), i32> {
        map::release_region(
            NonNull::new(self.base.get() as *mut c_void).expect("must be valid"),
            self.top.get() - self.base.get(),
        )
    }

    /// Backs the data and metadata region with memory.
    fn allocate(&self) -> Result<(), i32> {
        // Ensures the region containing [begin, begin + size) is
        // backed by memory, by rounding outward.
        fn rounded_allocate(mut begin: usize, size: usize) -> Result<(), i32> {
            let mut top = begin + size;

            let page_size = map::page_size();
            begin -= begin % page_size;
            if (top % page_size) > 0 {
                top = page_size * (1 + (top / page_size));
            }

            map::allocate_region(
                NonNull::new(begin as *mut c_void).expect("must be valid"),
                top - begin,
            )
        }

        rounded_allocate(self.meta as usize, METADATA_PAGE_SIZE)?;
        rounded_allocate(self.data as usize, DATA_ALIGNMENT)
    }

    /// Releases any slop around the allocated memory.
    fn commit(self) -> Result<(), i32> {
        fn release(begin: usize, end: usize) -> Result<(), i32> {
            let page_size = map::page_size();

            assert!(begin <= end);
            if begin == end {
                return Ok(());
            }

            assert_eq!(begin % page_size, 0);
            assert_eq!(end % page_size, 0);
            map::release_region(
                NonNull::new(begin as *mut c_void).expect("must be valid"),
                end - begin,
            )
        }

        release(self.base.get(), self.bottom_slop_end)?;
        release(self.top_slop_begin, self.top.get())
    }
}

/// Returns Ok if the allocation `address` might have come from a `Press` for `class`.
///
/// # Errors
///
/// Returns Err if the address definitely did not come from that `class`.
#[inline]
pub fn check_allocation(class: Class, address: usize) -> Result<(), &'static str> {
    let meta_ptr = ChunkMetadata::from_allocation_address(address);

    let meta = unsafe { meta_ptr.as_mut() }.ok_or("Derived a bad metadata address")?;
    if meta.class_id != Some(class.id()) {
        Err("Incorrect class id")
    } else {
        Ok(())
    }
}

impl Press {
    pub fn new(class: Class, mut layout: Layout) -> Result<Self, &'static str> {
        layout = layout.pad_to_align();

        if layout.size() > DATA_ALIGNMENT / 2 {
            Err("Class elements too large (after alignment)")
        } else {
            Ok(Self {
                bump: Default::default(),
                bump_replace_lock: Default::default(),
                layout,
                class,
            })
        }
    }

    /// Attempts to allocate one object by bumping the metadata
    /// pointer.
    fn try_allocate_from_chunk(&self, meta: &mut ChunkMetadata) -> Option<LinearRef> {
        let allocated_id = meta.bump_ptr.fetch_add(1, Ordering::Relaxed);

        if allocated_id >= meta.bump_limit as usize {
            return None;
        }

        let address = meta.chunk_begin + allocated_id * self.layout.size();
        Some(LinearRef::new(NonNull::new(address as *mut c_void)?))
    }

    /// Attempts to replace our bump pointer with a new one.
    fn try_replace_chunk(&self, expected: *mut ChunkMetadata) -> Result<(), i32> {
        let page_size = map::page_size();
        let mut size = MAPPED_REGION_SIZE;
        if (size % page_size) > 0 {
            size = page_size * (1 + (size / page_size));
        }

        if self.bump.load(Ordering::Relaxed) != expected {
            // Someone else made progress.
            return Ok(());
        }

        let _guard = self.bump_replace_lock.lock().unwrap();
        // Check again with the lock held, before allocating a new chunk.
        if self.bump.load(Ordering::Relaxed) != expected {
            return Ok(());
        }

        let region: NonNull<c_void> = map::reserve_region(size)?;
        let chunk = AllocatedChunk::new(NonZeroUsize::new(region.as_ptr() as usize).unwrap(), size)
            .expect("Must be able to partition");

        chunk.call_with_chunk(|chunk| {
            let meta = unsafe { chunk.meta.as_mut() }.expect("must be valid");
            meta.class_id = Some(self.class.id());
            meta.bump_limit = (DATA_ALIGNMENT / self.layout.size()) as u32;
            assert!(
                meta.bump_limit > 0,
                "layout.size > DATA_ALIGNMENT, but we check for that in the constructor."
            );
            meta.bump_ptr = AtomicUsize::new(0);
            meta.chunk_begin = chunk.data as usize;

            // Publish the metadata for our fresh chunk.
            assert_eq!(self.bump.load(Ordering::Relaxed), expected);
            self.bump.store(chunk.meta, Ordering::Release);
            Ok(())
        })
    }

    /// Attempts to allocate one object.  Returns Ok(_) if we tried to
    /// allocate from the current bump region.
    ///
    /// # Errors
    ///
    /// Returns `Err` if we failed to grab a new bump region.
    fn try_allocate_once(&self) -> Result<Option<LinearRef>, i32> {
        let meta_ptr: *mut ChunkMetadata = self.bump.load(Ordering::Acquire);

        if let Some(meta) = unsafe { meta_ptr.as_mut() } {
            if let Some(ret) = self.try_allocate_from_chunk(meta) {
                return Ok(Some(ret));
            }
        }

        // Either we didn't find any chunk metadata, and bump
        // allocation failed.  Either way, let's try to put
        // a new chunk in.
        self.try_replace_chunk(meta_ptr).map(|_| None)
    }

    pub fn allocate_one_object(&self) -> Option<LinearRef> {
        loop {
            match self.try_allocate_once() {
                Err(_) => return None, // TODO: log
                Ok(Some(ret)) => return Some(ret),
                _ => continue,
            }
        }
    }
}

#[test]
fn test_allocated_chunk_valid() {
    // Check that we can always construct an AllocatedChunk when we
    // pass in large enough regions.

    // The test cases below assume that the GUARD_PAGE_SIZE and
    // METADATA_PAGE_SIZE are multiples of the page size.
    assert_eq!(GUARD_PAGE_SIZE % map::page_size(), 0);
    assert_eq!(METADATA_PAGE_SIZE % map::page_size(), 0);

    let aligned = AllocatedChunk::new(
        NonZeroUsize::new(DATA_ALIGNMENT).unwrap(),
        MAPPED_REGION_SIZE,
    )
    .expect("must construct");
    aligned.check_rep();

    // We assume the zero page can't be mapped.  See what happens when
    // we get the next page.
    let at_start = AllocatedChunk::new(
        NonZeroUsize::new(map::page_size()).unwrap(),
        MAPPED_REGION_SIZE,
    )
    .expect("must construct");
    at_start.check_rep();

    // This is the highest possible address we might get, minus one
    // page: if the mapped region includes the last page, we fail to
    // represent the end of the region.  I don't think it's worth
    // adding complexity to handle a situation that never happens.
    let at_end = AllocatedChunk::new(
        NonZeroUsize::new(usize::MAX - MAPPED_REGION_SIZE - map::page_size() + 1).unwrap(),
        MAPPED_REGION_SIZE,
    )
    .expect("must construct");
    at_end.check_rep();

    let unaligned = AllocatedChunk::new(
        NonZeroUsize::new(DATA_ALIGNMENT + map::page_size()).unwrap(),
        MAPPED_REGION_SIZE,
    )
    .expect("must construct");
    unaligned.check_rep();

    let offset_guard = AllocatedChunk::new(
        NonZeroUsize::new(DATA_ALIGNMENT - GUARD_PAGE_SIZE).unwrap(),
        MAPPED_REGION_SIZE,
    )
    .expect("must construct");
    offset_guard.check_rep();

    let offset_meta = AllocatedChunk::new(
        NonZeroUsize::new(DATA_ALIGNMENT - GUARD_PAGE_SIZE - METADATA_PAGE_SIZE).unwrap(),
        MAPPED_REGION_SIZE,
    )
    .expect("must construct");
    offset_meta.check_rep();

    let off_by_one = AllocatedChunk::new(
        NonZeroUsize::new(
            DATA_ALIGNMENT - 2 * GUARD_PAGE_SIZE - METADATA_PAGE_SIZE + map::page_size(),
        )
        .unwrap(),
        MAPPED_REGION_SIZE,
    )
    .expect("must construct");
    off_by_one.check_rep();

    let exact_fit = AllocatedChunk::new(
        NonZeroUsize::new(DATA_ALIGNMENT - 2 * GUARD_PAGE_SIZE - METADATA_PAGE_SIZE).unwrap(),
        MAPPED_REGION_SIZE,
    )
    .expect("must construct");
    exact_fit.check_rep();
}
