//! The file-backed mapper ensures object are allocated in shared file
//! mappings of private temporary files.  This lets the operating
//! system eagerly swap out cold data when under memory pressure.
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
use std::ptr::NonNull;

use crate::Mapper;

#[derive(Debug)]
pub struct FileBackedMapper {}

#[contract_trait]
impl Mapper for FileBackedMapper {
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
