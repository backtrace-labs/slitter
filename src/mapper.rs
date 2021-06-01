//! A `Mapper` is responsible for acquiring address space and backing
//! memory from the operating system.  Each `Mill` is parameterised on
//! such a `Mapper`.
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
use std::ptr::NonNull;
use std::sync::Mutex;

#[cfg(any(
    all(test, feature = "check_contracts_in_tests"),
    feature = "check_contracts"
))]
use crate::debug_arange_map;

pub use crate::mill::GUARD_PAGE_SIZE;

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
    #[ensures(ret.is_ok() -> ret.as_ref().unwrap().0.as_ptr() != std::ptr::null_mut(),
              "The mapped range never includes NULL")]
    #[ensures(ret.is_ok() -> ret.as_ref().unwrap().1 < usize::MAX - ret.as_ref().unwrap().0.as_ptr() as usize,
              "The mapped range never overflows")]
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
    ///
    /// On successful return, the range must be zero-filled.
    #[requires(debug_arange_map::can_mark_metadata(base.as_ptr() as usize, size).is_ok())]
    #[ensures(ret.is_ok() -> debug_arange_map::mark_metadata(base.as_ptr() as usize, size).is_ok())]
    fn allocate_meta(&self, base: NonNull<c_void>, size: usize) -> Result<(), i32>;

    /// Prepares a page-aligned range of object data for read and
    /// write access.  The `allocate`d range is always a subset of a
    /// range that was returned by a single `reserve` call.
    ///
    /// On successful return, the range must be zero-filled.
    #[requires(debug_arange_map::can_mark_data(base.as_ptr() as usize, size).is_ok())]
    #[ensures(ret.is_ok() -> debug_arange_map::mark_data(base.as_ptr() as usize, size).is_ok())]
    fn allocate_data(&self, base: NonNull<c_void>, size: usize) -> Result<(), i32>;
}

#[derive(Debug)]
struct DefaultMapper {}

lazy_static::lazy_static! {
    static ref NAMED_MAPPERS: Mutex<HashMap<String, &'static dyn Mapper>> = {
        let mut map: HashMap<String, &'static dyn Mapper> = HashMap::new();

        map.insert("file".to_string(), Box::leak(Box::new(crate::file_backed_mapper::FileBackedMapper{})));
        Mutex::new(map)
    };
}

/// Upserts the mapper associated with `name`.
pub fn register_mapper(name: String, mapper: &'static dyn Mapper) {
    let mut mappers = NAMED_MAPPERS.lock().unwrap();

    mappers.insert(name, mapper);
}

/// Returns the mapper for the given `name`, if one exists, or the
/// default mapper if `name` is `None`.
///
/// # Errors
///
/// Returns `Err` if no such mapper is defined.
pub fn get_mapper(name: Option<&str>) -> Result<&'static dyn Mapper, &'static str> {
    lazy_static::lazy_static! {
        static ref DEFAULT_MAPPER: DefaultMapper = DefaultMapper{};
    }

    match name {
        Some(key) => {
            let mappers = NAMED_MAPPERS.lock().unwrap();

            Ok(*mappers.get(key).ok_or("Mapper not found")?)
        }
        None => Ok(&*DEFAULT_MAPPER),
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
