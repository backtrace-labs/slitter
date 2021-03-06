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

use std::collections::HashMap;
use std::ffi::c_void;
use std::num::NonZeroU32;
use std::num::NonZeroUsize;
use std::ptr::NonNull;
use std::sync::atomic::AtomicUsize;
use std::sync::Mutex;

#[cfg(any(
    all(test, feature = "check_contracts_in_tests"),
    feature = "check_contracts"
))]
use crate::debug_arange_map;

use crate::mapper::Mapper;

/// Data chunks are naturally aligned to their size, 1GB.
#[cfg(not(feature = "test_only_small_constants"))]
const DATA_ALIGNMENT: usize = 1 << 30;
/// We use 2 MB sizes to enable huge pages.  3 guard superpages + 1
/// metadata superpage per chunk is still less than 1% overhead.
#[cfg(not(feature = "test_only_small_constants"))]
pub const GUARD_PAGE_SIZE: usize = 2 << 20;
#[cfg(not(feature = "test_only_small_constants"))]
const METADATA_PAGE_SIZE: usize = 2 << 20;

/// Spans are aligned to 16 KB, within the chunk.
#[cfg(not(feature = "test_only_small_constants"))]
pub const SPAN_ALIGNMENT: usize = 16 << 10;

// Try to shrink everything by ~512.  We have to bump
// up the metadata page size a little, since we also
// want a smaller span alignment (and the metadata
// array must include one entry per potential span).
// Keep `GUARD_PAGE_SIZE` equal to `METADATA_PAGE_SIZE`
// to better match production.
#[cfg(feature = "test_only_small_constants")]
const DATA_ALIGNMENT: usize = 2 << 20;
#[cfg(feature = "test_only_small_constants")]
pub const GUARD_PAGE_SIZE: usize = 16 << 10;
#[cfg(feature = "test_only_small_constants")]
const METADATA_PAGE_SIZE: usize = 16 << 10;

#[cfg(feature = "test_only_small_constants")]
pub const SPAN_ALIGNMENT: usize = 4 << 10;

/// Maximum size in bytes we can service for a single span.  The
/// higher this value, the more bytes we may lose to fragmentation
/// when the remaining bytes in a chunk aren't enough.
///
/// Setting this to 1/16th of the chunk size means we lose at most
/// ~6.25% to that source of fragmentation.
pub const MAX_SPAN_SIZE: usize = DATA_ALIGNMENT / 16;

/// By default, we want to carve our *nearly* 1 MB spans: the slight
/// misalignment spreads out our metadata to different cache sets.
#[cfg(not(feature = "test_only_small_constants"))]
pub const DEFAULT_DESIRED_SPAN_SIZE: usize = (1 << 20) - SPAN_ALIGNMENT;

#[cfg(feature = "test_only_small_constants")]
pub const DEFAULT_DESIRED_SPAN_SIZE: usize = (8 << 10) - SPAN_ALIGNMENT;

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
    current_chunk: Mutex<Option<Chunk>>,
}

/// Returns a reference to the `Mill` for that `mapper_name`,
/// or the default `Mill` if the name is `None`.
///
/// # Errors
///
/// Returns `Err` if no such mapper is defined.
pub fn get_mill(mapper_name: Option<&str>) -> Result<&'static Mill, &'static str> {
    lazy_static::lazy_static! {
        // The keys are the addresses of `&'static dyn Mapper`.
        static ref MILLS: Mutex<HashMap<usize, &'static Mill>> = Default::default();
    }

    let mapper: &'static _ = crate::mapper::get_mapper(mapper_name)?;
    let address = mapper as *const _ as *const () as usize;
    let mut mills = MILLS.lock().unwrap();
    Ok(mills
        .entry(address)
        .or_insert_with(|| Box::leak(Box::new(Mill::new(mapper)))))
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
    ///
    /// TODO: Test the core of this function symbolically.
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
    #[ensures(ret.is_ok() == debug_arange_map::is_metadata(old(self.meta as usize), METADATA_PAGE_SIZE).is_ok(),
               "The metadata region is marked as such on success.")]
    #[ensures(ret.is_ok() == debug_arange_map::is_data(old(self.data as usize), DATA_ALIGNMENT).is_ok(),
               "The data region is marked as such on success.")]
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
    #[ensures(debug_arange_map::is_metadata(self.meta as usize, METADATA_PAGE_SIZE).is_ok(),
               "The metadata region is marked as such.")]
    #[ensures(debug_arange_map::is_data(self.data as usize, DATA_ALIGNMENT).is_ok(),
               "The data region is marked as such.")]
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
    #[requires(debug_arange_map::is_metadata(self.meta as usize, METADATA_PAGE_SIZE).is_ok(),
               "The metadata region is marked as such.")]
    #[requires(debug_arange_map::is_data(self.data as usize, DATA_ALIGNMENT).is_ok(),
               "The data region is marked as such.")]
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
    pub fn new(mapper: &'static dyn Mapper) -> Self {
        extern "C" {
            fn slitter__data_alignment() -> usize;
            fn slitter__guard_page_size() -> usize;
            fn slitter__metadata_page_size() -> usize;
            fn slitter__span_alignment() -> usize;
            fn slitter__span_metadata_size() -> usize;
        }

        unsafe {
            assert_eq!(DATA_ALIGNMENT, slitter__data_alignment());
            assert_eq!(GUARD_PAGE_SIZE, slitter__guard_page_size());
            assert_eq!(METADATA_PAGE_SIZE, slitter__metadata_page_size());
            assert_eq!(SPAN_ALIGNMENT, slitter__span_alignment());
            assert_eq!(
                std::mem::size_of::<SpanMetadata>(),
                slitter__span_metadata_size()
            );
        }

        Self {
            mapper,
            current_chunk: Default::default(),
        }
    }

    /// Returns a fresh chunk from `mapper`.
    #[ensures(ret.is_ok() ->
              debug_arange_map::is_metadata(ret.as_ref().unwrap().meta as usize,
                                            METADATA_PAGE_SIZE).is_ok(),
              "The metadata region is marked as such.")]
    #[ensures(ret.is_ok() ->
              debug_arange_map::is_data(ret.as_ref().unwrap().spans,
                                        ret.as_ref().unwrap().span_count * SPAN_ALIGNMENT).is_ok(),
              "The data region is marked as such.")]
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
    #[requires(debug_arange_map::is_metadata(chunk.meta as usize,
                                             METADATA_PAGE_SIZE).is_ok(),
               "The metadata region must be marked as such")]
    #[requires(debug_arange_map::is_data(chunk.spans as usize,
                                         chunk.span_count * SPAN_ALIGNMENT).is_ok(),
               "The data region must be marked as such")]
    #[requires(min <= DATA_ALIGNMENT / SPAN_ALIGNMENT,
        "The request must fit in a data chunk.")]
    #[ensures(ret.is_none() -> chunk.next_free_span == old(chunk.next_free_span),
              "Must leave the chunk untouched on success")]
    #[ensures(ret.is_some() ->
              ret.as_ref().unwrap().data_size >= min * SPAN_ALIGNMENT,
              "Must allocate enough for the min size on success.")]
    #[ensures(ret.is_some() ->
              chunk.next_free_span == old(chunk.next_free_span) + ret.as_ref().unwrap().data_size / SPAN_ALIGNMENT,
              "Must consume the bump pointer on success.")]
    fn allocate_span(chunk: &mut Chunk, min: usize, desired: usize) -> Option<MilledRange> {
        // Inspect this routine to confirm that the meta and data
        // regions are allocated according to `next_free_span`.  The
        // postconditions are a mess of casts.
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
    #[requires(min_size <= MAX_SPAN_SIZE)]
    #[requires(min_size <= desired_size.unwrap_or(min_size))]
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

#[test]
fn test_allocated_span_valid() {
    // Check that we can always construct an AllocatedChunk when we
    // pass in large enough regions.
    let mapper = crate::mapper::get_mapper(None).expect("Default mapper exists");

    // The test cases below assume that the GUARD_PAGE_SIZE and
    // METADATA_PAGE_SIZE are multiples of the page size.
    assert_eq!(GUARD_PAGE_SIZE % mapper.page_size(), 0);
    assert_eq!(METADATA_PAGE_SIZE % mapper.page_size(), 0);

    // We assume the zero page can't be mapped.  See what happens when
    // we get the next page.
    let at_start = AllocatedChunk::new_from_range(
        mapper,
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
        mapper,
        NonZeroUsize::new(usize::MAX - MAPPED_REGION_SIZE - mapper.page_size() + 1).unwrap(),
        MAPPED_REGION_SIZE,
    )
    .expect("must construct");
    at_end.check_rep();

    let aligned = AllocatedChunk::new_from_range(
        mapper,
        NonZeroUsize::new(DATA_ALIGNMENT).unwrap(),
        MAPPED_REGION_SIZE,
    )
    .expect("must construct");
    aligned.check_rep();

    let unaligned = AllocatedChunk::new_from_range(
        mapper,
        NonZeroUsize::new(DATA_ALIGNMENT + mapper.page_size()).unwrap(),
        MAPPED_REGION_SIZE,
    )
    .expect("must construct");
    unaligned.check_rep();

    let offset_guard = AllocatedChunk::new_from_range(
        mapper,
        NonZeroUsize::new(DATA_ALIGNMENT - GUARD_PAGE_SIZE).unwrap(),
        MAPPED_REGION_SIZE,
    )
    .expect("must construct");
    offset_guard.check_rep();

    let offset_meta = AllocatedChunk::new_from_range(
        mapper,
        NonZeroUsize::new(DATA_ALIGNMENT - GUARD_PAGE_SIZE - METADATA_PAGE_SIZE).unwrap(),
        MAPPED_REGION_SIZE,
    )
    .expect("must construct");
    offset_meta.check_rep();

    let off_by_one = AllocatedChunk::new_from_range(
        mapper,
        NonZeroUsize::new(
            DATA_ALIGNMENT - 2 * GUARD_PAGE_SIZE - METADATA_PAGE_SIZE + mapper.page_size(),
        )
        .unwrap(),
        MAPPED_REGION_SIZE,
    )
    .expect("must construct");
    off_by_one.check_rep();

    let exact_fit = AllocatedChunk::new_from_range(
        mapper,
        NonZeroUsize::new(DATA_ALIGNMENT - 2 * GUARD_PAGE_SIZE - METADATA_PAGE_SIZE).unwrap(),
        MAPPED_REGION_SIZE,
    )
    .expect("must construct");
    exact_fit.check_rep();
}
