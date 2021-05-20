//! A `Mill` hands out parcels of chunk data and associated chunk
//! metadata to `Press`es.  We expect multiple `Press`es to share
//! the same `Mill`, and `Press`es belong to `ClassInfo`, and are
//! thus immortal; `Mill`s are also immortal.
use std::ffi::c_void;
use std::num::NonZeroU32;
use std::num::NonZeroUsize;
use std::ptr::NonNull;
use std::sync::atomic::AtomicUsize;

/// Data chunks are naturally aligned to their chunk size.
const DATA_ALIGNMENT: usize = 2 << 20;
const GUARD_PAGE_SIZE: usize = 4096;
const METADATA_PAGE_SIZE: usize = 4096;

/// Maximum size in bytes we can service for a single allocation.
pub const MAX_DATA_SIZE: usize = DATA_ALIGNMENT;

/// Grabbing an address space of at least this many bytes should be
/// enough to find a spot for the chunk + its metadata.
///
/// If the first aligned region has enough of a prefix for 2 guard
/// pages and the metadata, we're good.
///
/// Otherwise, the next aligned region is at most `DATA_ALIGNMENT + 2
/// * GUARD_PAGE_SIZE + METADATA_PAGE_SIZE` in, which must leave at
/// least the suffix `GUARD_PAGE` before the end of the region.
///
/// The code in this module, including this constant, assumes that
/// `DATA_ALIGNMENT` is greater than the total amount of bytes we wish
/// to keep around the data chunk.
const MAPPED_REGION_SIZE: usize = 2 * DATA_ALIGNMENT + 3 * GUARD_PAGE_SIZE + METADATA_PAGE_SIZE;

const PREFIX_SIZE: usize = GUARD_PAGE_SIZE + METADATA_PAGE_SIZE + GUARD_PAGE_SIZE;
const SUFFIX_SIZE: usize = GUARD_PAGE_SIZE;

/// `Mill` are parameterised on `Mapper`s that are responsible for
/// acquiring address space from the operating system.
pub trait Mapper: std::fmt::Debug + Sync {
    /// Returns the mapping granularity for this mapper.  All calls
    /// into the mapper will align addresses and sizes to that page
    /// size.
    ///
    /// The page size must be constant for the lifetime of a process.
    fn page_size(&self) -> usize;

    /// Attempts to reserve a range of address space.  On success,
    /// returns the address of the first byte in the reserved range,
    /// and the number of bytes actually reserved.  Both values
    /// should be aligned to the `page_size()`.
    ///
    /// Any page-aligned allocation of `desired_size` bytes will
    /// suffice to satisfy the caller.  However, the mapper may also
    /// try to do something smarter, knowing that its caller wants an
    /// range of `data_size` bytes aligned to `data_size`, with
    /// `prefix` bytes before that range, and `suffix` bytes after.
    ///
    /// The `data_size`, `prefix`, and `suffix` values may
    /// be misaligned with respect to the page size.
    fn reserve(
        &self,
        desired_size: usize,
        data_size: usize,
        prefix: usize,
        suffix: usize,
    ) -> Result<(NonNull<c_void>, usize), i32>;

    /// Releases a page-aligned range that was previously obtained
    /// with a single call to `reserve`.  The `release`d range is
    /// always a subset of a range that was returned by a single
    /// `reserve` call.
    fn release(&self, base: NonNull<c_void>, size: usize) -> Result<(), i32>;

    /// Prepares a page-aligned range for read and write access.
    /// The `allocate`d range is always a subset of a range that was
    /// returned by a single `reserve` call.
    fn allocate(&self, base: NonNull<c_void>, size: usize) -> Result<(), i32>;
}

#[derive(Debug)]
#[repr(C)]
pub struct ChunkMetadata {
    /// The id of the class for all allocations in this chunk.  We
    /// don't use a `Class` because 0-initialised data must be valid.
    pub(crate) class_id: Option<NonZeroU32>,
    /// Number of elements available in the chunk
    pub(crate) bump_limit: u32,
    /// Number of elements allocated in the chunk
    pub(crate) bump_ptr: AtomicUsize,
    /// Start address for the chunk.
    pub(crate) chunk_begin: usize,
}

#[allow(unused)]
extern "C" {
    fn unused_chunk_metadata_is_zero_safe() -> ChunkMetadata;
}

/// A `MilledRange` represents a newly allocated chunk of data, and
/// associated metadata struct.
#[derive(Debug)]
pub struct MilledRange {
    // The meta object is initially zero-filled.
    pub meta: &'static mut ChunkMetadata,
    pub data: *mut c_void,
    pub data_size: usize,
}

#[derive(Debug)]
pub struct Mill {
    mapper: &'static dyn Mapper,
}

#[derive(Debug)]
struct DefaultMapper {}

/// Returns a reference to the shared default `Mill`.
pub fn get_default_mill() -> &'static Mill {
    lazy_static::lazy_static! {
    static ref DEFAULT_MILL: &'static Mill = {
        let default_mapper = Box::leak(Box::new(DefaultMapper{}));

        Box::leak(Box::new(Mill{ mapper: default_mapper }))
    };
    };

    &DEFAULT_MILL
}

impl ChunkMetadata {
    /// Maps a Press-allocated address to its metadata.
    pub fn from_allocation_address(address: usize) -> *mut ChunkMetadata {
        let base = address - (address % DATA_ALIGNMENT);
        let meta = base - GUARD_PAGE_SIZE - METADATA_PAGE_SIZE;

        meta as *mut ChunkMetadata
    }
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
struct AllocatedChunk<'a> {
    mapper: &'a dyn Mapper,
    // All these `usize` are addresses.
    base: NonZeroUsize,     // page-aligned
    top: NonZeroUsize,      // page-aligned
    bottom_slop_end: usize, // page-aligned
    pub meta: *mut ChunkMetadata,
    pub data: *mut c_void,
    pub data_end: usize,
    top_slop_begin: usize, // page-aligned
}

impl<'a> AllocatedChunk<'a> {
    /// Attempts to carve out a chunk + metadata from a new range of
    /// address space returned by `mapper`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the mapper itself fails.  Failures downstream
    /// indicate that the mapper returned an invalid range, and result
    /// in panic.
    pub fn new(mapper: &'a dyn Mapper) -> Result<AllocatedChunk<'a>, i32> {
        let page_size = mapper.page_size();
        let mut size = MAPPED_REGION_SIZE;
        if (size % page_size) > 0 {
            size = page_size * (1 + (size / page_size));
        }

        let (region, actual): (NonNull<c_void>, usize) =
            mapper.reserve(size, DATA_ALIGNMENT, PREFIX_SIZE, SUFFIX_SIZE)?;
        Ok(AllocatedChunk::new_from_range(
            mapper,
            NonZeroUsize::new(region.as_ptr() as usize).expect("NonNull should be NonZero"),
            actual,
        )
        .expect("mapper returned a bad region"))
    }

    /// Attempts to carve out a chunk + metadata in `[base, base + size)`.
    fn new_from_range(
        mapper: &'a dyn Mapper,
        base: NonZeroUsize,
        size: usize,
    ) -> Result<AllocatedChunk<'a>, &'static str> {
        let page_size = mapper.page_size();

        // Try to find the data address.  It must be aligned to 2MB,
        // and have room for the metadata + guard pages before/after
        // the data chunk.
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
            mapper,
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
        let page_size = self.mapper.page_size();

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
        self.mapper.release(
            NonNull::new(self.base.get() as *mut c_void).expect("must be valid"),
            self.top.get() - self.base.get(),
        )
    }

    /// Backs the data and metadata region with memory.
    fn allocate(&self) -> Result<(), i32> {
        // Ensures the region containing [begin, begin + size) is
        // backed by memory, by rounding outward.
        fn rounded_allocate(mapper: &dyn Mapper, mut begin: usize, size: usize) -> Result<(), i32> {
            let mut top = begin + size;

            let page_size = mapper.page_size();
            begin -= begin % page_size;
            if (top % page_size) > 0 {
                top = page_size * (1 + (top / page_size));
            }

            mapper.allocate(
                NonNull::new(begin as *mut c_void).expect("must be valid"),
                top - begin,
            )
        }

        rounded_allocate(self.mapper, self.meta as usize, METADATA_PAGE_SIZE)?;
        rounded_allocate(self.mapper, self.data as usize, DATA_ALIGNMENT)
    }

    /// Releases any slop around the allocated memory.
    fn commit(self) -> Result<(), i32> {
        fn release(mapper: &dyn Mapper, begin: usize, end: usize) -> Result<(), i32> {
            let page_size = mapper.page_size();

            assert!(begin <= end);
            if begin == end {
                return Ok(());
            }

            assert_eq!(begin % page_size, 0);
            assert_eq!(end % page_size, 0);
            mapper.release(
                NonNull::new(begin as *mut c_void).expect("must be valid"),
                end - begin,
            )
        }

        release(self.mapper, self.base.get(), self.bottom_slop_end)?;
        release(self.mapper, self.top_slop_begin, self.top.get())
    }
}

impl Mill {
    /// Attempts to return a fresh range of allocation space.
    ///
    /// # Errors
    ///
    /// Returns `Err` on mapping failures (OOM-like conditions).
    pub fn get_chunk(&self) -> Result<MilledRange, i32> {
        AllocatedChunk::new(self.mapper)?.call_with_chunk(|chunk| {
            let meta = unsafe { chunk.meta.as_mut() }.expect("must be valid");
            Ok(MilledRange {
                meta,
                data: chunk.data,
                data_size: MAX_DATA_SIZE,
            })
        })
    }
}

impl Mapper for DefaultMapper {
    fn page_size(&self) -> usize {
        crate::map::page_size()
    }

    fn reserve(
        &self,
        desired_size: usize,
        _data_size: usize,
        _prefix: usize,
        _suffix: usize,
    ) -> Result<(NonNull<c_void>, usize), i32> {
        let region: NonNull<c_void> = crate::map::reserve_region(desired_size)?;
        Ok((region, desired_size))
    }

    fn release(&self, base: NonNull<c_void>, size: usize) -> Result<(), i32> {
        crate::map::release_region(base, size)
    }

    fn allocate(&self, base: NonNull<c_void>, size: usize) -> Result<(), i32> {
        crate::map::allocate_region(base, size)
    }
}

#[test]
fn test_allocated_chunk_valid() {
    // Check that we can always construct an AllocatedChunk when we
    // pass in large enough regions.
    let mapper = DefaultMapper {};

    // The test cases below assume that the GUARD_PAGE_SIZE and
    // METADATA_PAGE_SIZE are multiples of the page size.
    assert_eq!(GUARD_PAGE_SIZE % mapper.page_size(), 0);
    assert_eq!(METADATA_PAGE_SIZE % mapper.page_size(), 0);

    // We assume the zero page can't be mapped.  See what happens when
    // we get the next page.
    let at_start = AllocatedChunk::new_from_range(
        &mapper,
        NonZeroUsize::new(mapper.page_size()).unwrap(),
        MAPPED_REGION_SIZE,
    )
    .expect("must construct");
    at_start.check_rep();

    // This is the highest possible address we might get, minus one
    // page: if the mapped region includes the last page, we fail to
    // represent the end of the region.  I don't think it's worth
    // adding complexity to handle a situation that never happens.
    let at_end = AllocatedChunk::new_from_range(
        &mapper,
        NonZeroUsize::new(usize::MAX - MAPPED_REGION_SIZE - mapper.page_size() + 1).unwrap(),
        MAPPED_REGION_SIZE,
    )
    .expect("must construct");
    at_end.check_rep();

    let aligned = AllocatedChunk::new_from_range(
        &mapper,
        NonZeroUsize::new(DATA_ALIGNMENT).unwrap(),
        MAPPED_REGION_SIZE,
    )
    .expect("must construct");
    aligned.check_rep();

    let unaligned = AllocatedChunk::new_from_range(
        &mapper,
        NonZeroUsize::new(DATA_ALIGNMENT + mapper.page_size()).unwrap(),
        MAPPED_REGION_SIZE,
    )
    .expect("must construct");
    unaligned.check_rep();

    let offset_guard = AllocatedChunk::new_from_range(
        &mapper,
        NonZeroUsize::new(DATA_ALIGNMENT - GUARD_PAGE_SIZE).unwrap(),
        MAPPED_REGION_SIZE,
    )
    .expect("must construct");
    offset_guard.check_rep();

    let offset_meta = AllocatedChunk::new_from_range(
        &mapper,
        NonZeroUsize::new(DATA_ALIGNMENT - GUARD_PAGE_SIZE - METADATA_PAGE_SIZE).unwrap(),
        MAPPED_REGION_SIZE,
    )
    .expect("must construct");
    offset_meta.check_rep();

    let off_by_one = AllocatedChunk::new_from_range(
        &mapper,
        NonZeroUsize::new(
            DATA_ALIGNMENT - 2 * GUARD_PAGE_SIZE - METADATA_PAGE_SIZE + mapper.page_size(),
        )
        .unwrap(),
        MAPPED_REGION_SIZE,
    )
    .expect("must construct");
    off_by_one.check_rep();

    let exact_fit = AllocatedChunk::new_from_range(
        &mapper,
        NonZeroUsize::new(DATA_ALIGNMENT - 2 * GUARD_PAGE_SIZE - METADATA_PAGE_SIZE).unwrap(),
        MAPPED_REGION_SIZE,
    )
    .expect("must construct");
    exact_fit.check_rep();
}
