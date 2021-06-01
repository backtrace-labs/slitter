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
use std::fs::File;
use std::path::PathBuf;
use std::ptr::NonNull;
use std::sync::Mutex;

use crate::Mapper;

#[derive(Debug)]
pub struct FileBackedMapper {}

lazy_static::lazy_static! {
    static ref FILE_BACKED_PATH: Mutex<Option<PathBuf>> = Default::default();
}

/// Updates the parent directory for the file-backed mapper's
/// temporary files to `path`.
pub fn set_file_backed_slab_directory(path: Option<PathBuf>) {
    let mut global_path = FILE_BACKED_PATH.lock().unwrap();

    *global_path = path;
}

/// Returns a temporary File in `FILE_BACKED_PATH`, or in the the
/// global `TMPDIR`.
///
/// TODO: return a `std::io::Result<File>`.
fn get_temp_file() -> Result<File, i32> {
    let path = FILE_BACKED_PATH.lock().unwrap();

    match &*path {
        Some(dir) => tempfile::tempfile_in(dir),
        None => tempfile::tempfile(),
    }
    .map_err(|e| e.raw_os_error().unwrap_or(0))
}

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
        let tempfile = get_temp_file()?;

        crate::map::allocate_file_region(tempfile, base, size)
    }
}
