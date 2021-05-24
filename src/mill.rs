//! A `Mill` hands out parcels of span data and associated span
//! metadata to `Press`es.  We expect multiple `Press`es to share
//! the same `Mill`, and `Press`es belong to `ClassInfo`, and are
//! thus immortal; `Mill`s are also immortal.
//!
//! Each `Mill` owns a large `Chunk`, and associated range of
//! metadata, and partitions that chunk into smaller `Span`s.  The
//! chunk data is a 1 GB range, aligned to 1 GB, and the associated
//! array of metadata lives at a fixed offset from the beginning of
//! the chunk.  The layout looks like
//!
//! | guard | meta | guard | data ... data | guard |
//!
//! where the guard and meta(data) regions are 2 MB each, and the
//! data is 1 GB, aligned to 1 GB.
//!
//! Each chunk is divided 64 K spans of 16 KB each.  Each span is
//! associated with a metadata object in the parallel flat array that
//! lives in the metadata range.
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
use std::num::NonZeroU32;
use std::num::NonZeroUsize;
use std::ptr::NonNull;
use std::sync::atomic::AtomicUsize;

#[cfg(any(
    all(test, feature = "check_contracts_in_tests"),
    feature = "check_contracts"
))]
use crate::debug_arange_map;

/// Data chunks are naturally aligned to their size, 1GB.
const DATA_ALIGNMENT: usize = 1 << 30;
/// We use 2 MB sizes to enable huge pages.  3 guard superpages + 1
/// metadata superpage per chunk is still less than 1% overhead.
const GUARD_PAGE_SIZE: usize = 2 << 20;
const METADATA_PAGE_SIZE: usize = 2 << 20;

/// Spans are aligned to 16 KB, within the chunk.
pub const SPAN_ALIGNMENT: usize = 16 << 10;

/// Maximum size in bytes we can service for a single span.  The
/// higher this value, the more bytes we may lose to fragmentation
/// when the remaining bytes in a chunk aren't enough.
///
/// Setting this to 1/16th of the chunk size means we lose at most
/// ~6.25% to that source of fragmentation.
pub const MAX_SPAN_SIZE: usize = DATA_ALIGNMENT / 16;

/// By default, we want to carve our *nearly* 1 MB spans: the slight
/// misalignment spreads out our metadata to different cache sets.
pub const DEFAULT_DESIRED_SPAN_SIZE: usize = (1 << 20) - SPAN_ALIGNMENT;

static_assertions::const_assert!(DEFAULT_DESIRED_SPAN_SIZE <= MAX_SPAN_SIZE);

/// Grabbing an address space of at least this many bytes should be
/// enough to find a spot for the span + its metadata.
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
/// to keep around the data span.
const MAPPED_REGION_SIZE: usize = 2 * DATA_ALIGNMENT + 3 * GUARD_PAGE_SIZE + METADATA_PAGE_SIZE;

const PREFIX_SIZE: usize = GUARD_PAGE_SIZE + METADATA_PAGE_SIZE + GUARD_PAGE_SIZE;
const SUFFIX_SIZE: usize = GUARD_PAGE_SIZE;

// The constants are tightly coupled.  Make sure they make sense.
static_assertions::const_assert_eq!(
    MAPPED_REGION_SIZE,
    2 * DATA_ALIGNMENT + PREFIX_SIZE + SUFFIX_SIZE
);

// We must have enough room in the metadata page for our spans'
// metadata.  There are `DATA_ALIGNMENT / SPAN_ALIGNMENT` span-aligned
// regions per data region, and each one needs a `SpanMetadata`.  The
// corresponding array of `SpanMetadata` must fit in the
// `METADATA_PAGE_SIZE`.
static_assertions::const_assert!(
    DATA_ALIGNMENT / SPAN_ALIGNMENT <= METADATA_PAGE_SIZE / std::mem::size_of::<SpanMetadata>()
);

static_assertions::const_assert!(std::mem::size_of::<MetaArray>() <= METADATA_PAGE_SIZE);

/// `Mill` are parameterised on `Mapper`s that are responsible for
/// acquiring address space from the operating system.
#[allow(clippy::inline_fn_without_body)]
#[contract_trait]
pub trait Mapper: std::fmt::Debug + Sync {
    /// Returns the mapping granularity for this mapper.  All calls
    /// into the mapper will align addresses and sizes to that page
    /// size.
    ///
    /// The page size must be constant for the lifetime of a process.
    #[ensures(ret > 0 && ret & (ret - 1) == 0, "page size must be a power of 2")]
    #[ensures(ret <= GUARD_PAGE_SIZE, "pages should be smaller than guard ranges")]
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
    #[requires(desired_size % self.page_size() == 0)]
    #[requires(desired_size > 0)]
    #[requires(data_size > 0)]
    #[ensures(ret.is_ok() -> debug_arange_map::reserve_range(ret.unwrap().0.as_ptr() as usize, ret.unwrap().1).is_ok())]
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
    #[requires(base.as_ptr() as usize % self.page_size() == 0)]
    #[requires(size % self.page_size() == 0)]
    #[requires(debug_arange_map::releasable_range(base.as_ptr() as usize, size).is_ok())]
    #[ensures(ret.is_ok() -> debug_arange_map::release_range(base.as_ptr() as usize, size).is_ok())]
    fn release(&self, base: NonNull<c_void>, size: usize) -> Result<(), i32>;

    /// Prepares a page-aligned range of metadata for read and write
    /// access.  The `allocate`d range is always a subset of a range
    /// that was returned by a single `reserve` call.
    #[requires(debug_arange_map::can_mark_metadata(base.as_ptr() as usize, size).is_ok())]
    #[ensures(ret.is_ok() -> debug_arange_map::mark_metadata(base.as_ptr() as usize, size).is_ok())]
    fn allocate_meta(&self, base: NonNull<c_void>, size: usize) -> Result<(), i32>;

    /// Prepares a page-aligned range of object data for read and
    /// write access.  The `allocate`d range is always a subset of a
    /// range that was returned by a single `reserve` call.
    #[requires(debug_arange_map::can_mark_data(base.as_ptr() as usize, size).is_ok())]
    #[ensures(ret.is_ok() -> debug_arange_map::mark_data(base.as_ptr() as usize, size).is_ok())]
    fn allocate_data(&self, base: NonNull<c_void>, size: usize) -> Result<(), i32>;
}

#[derive(Debug)]
#[repr(C)]
pub struct SpanMetadata {
    /// The id of the class for all allocations in this span.  We
    /// don't use a `Class` because 0-initialised data must be valid.
    pub(crate) class_id: Option<NonZeroU32>,
    /// Number of elements available in the span
    pub(crate) bump_limit: u32,
    /// Number of elements allocated in the span
    pub(crate) bump_ptr: AtomicUsize,
    /// Start address for the span.
    pub(crate) span_begin: usize,
}

#[allow(unused)]
extern "C" {
    fn unused_span_metadata_is_zero_safe() -> SpanMetadata;
}

/// A `MilledRange` represents a newly allocated span of data, and
/// associated metadata struct.
#[derive(Debug)]
pub struct MilledRange {
    /// The meta object is initially zero-filled, and is the first
    /// SpanMetadata object for the allocated span.
    pub meta: &'static mut SpanMetadata,
    /// A span may be associated with multiple metadata structs (but
    /// each SpanMetadata only maps to one span); the trail slice
    /// contains the remaining metadata structs.  They must be
    /// initialised so that we can later get the metadata (class id)
    /// for an allocated object.
    pub trail: &'static mut [SpanMetadata],
    pub data: *mut c_void,
    pub data_size: usize,
}

/// The array of metadata associated with a chunk.
#[derive(Debug)]
struct MetaArray {
    chunk_meta: [SpanMetadata; DATA_ALIGNMENT / SPAN_ALIGNMENT],
}

#[derive(Debug)]
struct Chunk {
    meta: *mut MetaArray,
    spans: usize,      // address where the chunk's spans start.
    span_count: usize, // chunk size in in spans

    // Bump pointer to allocate spans
    next_free_span: usize,
}

/// We manage the metadata array and the spans in each Chunk to avoid
/// accidental sharing.  That's why it's safe to `Send` them.
unsafe impl Send for Chunk {}

#[derive(Debug)]
pub struct Mill {
    mapper: &'static dyn Mapper,
    current_chunk: std::sync::Mutex<Option<Chunk>>,
}

#[derive(Debug)]
struct DefaultMapper {}

/// Returns a reference to the shared default `Mill`.
pub fn get_default_mill() -> &'static Mill {
    lazy_static::lazy_static! {
        static ref DEFAULT_MILL: &'static Mill = {
            let default_mapper = Box::leak(Box::new(DefaultMapper{}));

            Box::leak(Box::new(Mill{ mapper: default_mapper, current_chunk: Default::default() }))
        };
    };

    &DEFAULT_MILL
}

impl SpanMetadata {
    /// Maps a Press-allocated address to its metadata.
    pub fn from_allocation_address(address: usize) -> *mut SpanMetadata {
        let base = address - (address % DATA_ALIGNMENT);
        let index = (address - base) / SPAN_ALIGNMENT; // Span id.
        let meta = base - GUARD_PAGE_SIZE - METADATA_PAGE_SIZE;

        unsafe { (meta as *mut SpanMetadata).add(index) }
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
    pub meta: *mut MetaArray,
    pub data: *mut c_void,
    pub data_end: usize,
    top_slop_begin: usize, // page-aligned
}

impl<'a> AllocatedChunk<'a> {
    /// Attempts to carve out a span + metadata from a new range of
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

    /// Attempts to carve out a span + metadata in `[base, base + size)`.
    fn new_from_range(
        mapper: &'a dyn Mapper,
        base: NonZeroUsize,
        size: usize,
    ) -> Result<AllocatedChunk<'a>, &'static str> {
        let page_size = mapper.page_size();

        // Try to find the data address.  It must be aligned to 2MB,
        // and have room for the metadata + guard pages before/after
        // the data span.
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
        // another span size.
        if (data - base.get()) < PREFIX_SIZE {
            data = data.checked_add(DATA_ALIGNMENT).ok_or("overflow in bump")?;
        }

        // This subtraction is safe before data >= base + PREFIX_SIZE.
        let mut bottom_slop_end = data.checked_sub(PREFIX_SIZE).unwrap();
        // Make sure bottom_slop_end is aligned to a page.  It's always OK to remove *less*.
        bottom_slop_end -= bottom_slop_end % page_size;

        // This addition is safe for the same reason.
        let meta = bottom_slop_end.checked_add(GUARD_PAGE_SIZE).unwrap() as *mut MetaArray;

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
            SpanMetadata::from_allocation_address(self.data as usize),
            self.meta as *mut SpanMetadata
        );
        assert_eq!(
            SpanMetadata::from_allocation_address(self.data as usize + (DATA_ALIGNMENT - 1)),
            unsafe { (self.meta as *mut SpanMetadata).add(DATA_ALIGNMENT / SPAN_ALIGNMENT - 1) }
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
        // backed by memory, by rounding outward to a page.
        fn rounded_allocate(
            page_size: usize,
            mut begin: usize,
            size: usize,
            allocator: impl FnOnce(NonNull<c_void>, usize) -> Result<(), i32>,
        ) -> Result<(), i32> {
            let mut top = begin + size;

            begin -= begin % page_size;
            if (top % page_size) > 0 {
                top = page_size * (1 + (top / page_size));
            }

            allocator(
                NonNull::new(begin as *mut c_void).expect("must be valid"),
                top - begin,
            )
        }

        let page_size = self.mapper.page_size();
        rounded_allocate(
            page_size,
            self.meta as usize,
            METADATA_PAGE_SIZE,
            |begin, size| self.mapper.allocate_meta(begin, size),
        )?;
        rounded_allocate(
            page_size,
            self.data as usize,
            DATA_ALIGNMENT,
            |begin, size| self.mapper.allocate_data(begin, size),
        )
    }

    /// Releases any slop around the allocated memory.
    fn commit(self) -> Result<(), i32> {
        fn release(mapper: &dyn Mapper, begin: usize, end: usize) -> Result<(), i32> {
            let page_size = mapper.page_size();

            assert!(begin <= end);
            assert_eq!(begin % page_size, 0);
            assert_eq!(end % page_size, 0);

            if begin == end {
                return Ok(());
            }

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
    /// Returns a fresh chunk from `mapper`.
    fn allocate_chunk(mapper: &dyn Mapper) -> Result<Chunk, i32> {
        AllocatedChunk::new(mapper)?.call_with_chunk(|chunk| {
            let meta = unsafe { chunk.meta.as_mut() }.expect("must be valid");
            Ok(Chunk {
                meta,
                spans: chunk.data as usize,
                span_count: DATA_ALIGNMENT / SPAN_ALIGNMENT,
                next_free_span: 0,
            })
        })
    }

    /// Attempts to chop a new set of spans from a chunk.
    ///
    /// On success, it will contain at least `min` spans, and up to `desired` spans.
    fn allocate_span(chunk: &mut Chunk, min: usize, desired: usize) -> Option<MilledRange> {
        if chunk.next_free_span >= chunk.span_count {
            return None;
        }

        let remaining = chunk.span_count - chunk.next_free_span;
        // TODO: it would be nice to save these unused spans somewhere.
        if remaining < min {
            return None;
        }

        let allocated = remaining.min(desired);
        let index = chunk.next_free_span;
        chunk.next_free_span += allocated;

        let meta: &'static mut _ = unsafe { chunk.meta.as_mut() }.unwrap();
        // Bamboozle the borrow checker... we will pass two mutable
        // references to the chunk_meta array (and many more that the
        // checker isn't aware of), but they're all disjoint.
        let meta2: &'static mut _ = unsafe { chunk.meta.as_mut() }.unwrap();

        Some(MilledRange {
            meta: &mut meta.chunk_meta[index],
            trail: &mut meta2.chunk_meta[index + 1..index + allocated],
            data: (chunk.spans + index * SPAN_ALIGNMENT) as *mut c_void,
            data_size: allocated * SPAN_ALIGNMENT,
        })
    }

    /// Attempts to return a fresh range of allocation space.  On
    /// success, the newly milled range will contain at least
    /// `min_size` bytes, but the implementation tries to get
    /// `desired_size`, if possible.
    ///
    /// The `min_size` must be at most `MAX_SPAN_SIZE`.
    ///
    /// # Errors
    ///
    /// Returns `Err` on mapping failures (OOM-like conditions).
    pub fn get_span(
        &self,
        min_size: usize,
        desired_size: Option<usize>,
    ) -> Result<MilledRange, i32> {
        assert!(min_size <= MAX_SPAN_SIZE);

        // We must want at least min_size, and not more than the maximum span size.
        let desired = desired_size
            .unwrap_or(DEFAULT_DESIRED_SPAN_SIZE)
            .clamp(min_size, MAX_SPAN_SIZE);
        let min_span_count =
            (min_size / SPAN_ALIGNMENT) + ((min_size % SPAN_ALIGNMENT) > 0) as usize;
        let desired_span_count =
            (desired / SPAN_ALIGNMENT) + ((desired % SPAN_ALIGNMENT) > 0) as usize;

        let mut chunk_or = self.current_chunk.lock().unwrap();

        if chunk_or.is_none() {
            *chunk_or = Some(Mill::allocate_chunk(self.mapper)?);
        }

        if let Some(range) = Mill::allocate_span(
            chunk_or.as_mut().unwrap(),
            min_span_count,
            desired_span_count,
        ) {
            return Ok(range);
        }

        *chunk_or = Some(Mill::allocate_chunk(self.mapper)?);
        Ok(Mill::allocate_span(
            chunk_or.as_mut().unwrap(),
            min_span_count,
            desired_span_count,
        )
        .expect("New chunk must have a span"))
    }
}

#[contract_trait]
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

    fn allocate_meta(&self, base: NonNull<c_void>, size: usize) -> Result<(), i32> {
        crate::map::allocate_region(base, size)
    }

    fn allocate_data(&self, base: NonNull<c_void>, size: usize) -> Result<(), i32> {
        crate::map::allocate_region(base, size)
    }
}

#[test]
fn test_allocated_span_valid() {
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
