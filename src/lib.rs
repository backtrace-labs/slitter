mod cache;
mod class;
mod file_backed_mapper;
mod individual;
mod linear_ref;
mod magazine;
mod magazine_impl;
mod magazine_stack;
mod map;
mod mapper;
mod mill;
mod press;
mod rack;

#[cfg(any(
    all(test, feature = "check_contracts_in_tests"),
    feature = "check_contracts"
))]
mod debug_allocation_map;
#[cfg(any(
    all(test, feature = "check_contracts_in_tests"),
    feature = "check_contracts"
))]
mod debug_arange_map;
#[cfg(any(
    all(test, feature = "check_contracts_in_tests"),
    feature = "check_contracts"
))]
mod debug_type_map;

use std::os::raw::c_char;

pub use class::Class;
pub use class::ClassConfig;
pub use class::ForeignClassConfig;
pub use file_backed_mapper::set_file_backed_slab_directory;
pub use mapper::register_mapper;
pub use mapper::Mapper;

/// Registers a new allocation class globally
///
/// # Safety
///
/// This function assumes `config_ptr` is NULL or valid.
#[no_mangle]
pub unsafe extern "C" fn slitter_class_register(config_ptr: *const ForeignClassConfig) -> Class {
    let config = ClassConfig::from_c(config_ptr).expect("slitter_class_config must be valid");

    Class::new(config).expect("slitter class allocation should succeed")
}

/// Updates the directory for the file-backed slab's temporary files.
///
/// NULL reverts to the default, and ":memory:" forces regular
/// anonymous mappings.
///
/// # Safety
///
/// This function assumes `path` is NULL or valid.
#[no_mangle]
pub unsafe extern "C" fn slitter_set_file_backed_slab_directory(path: *const c_char) {
    use std::ffi::CStr;

    if path.is_null() {
        set_file_backed_slab_directory(None);
        return;
    }

    let path_str = CStr::from_ptr(path)
        .to_str()
        .expect("path must be valid")
        .to_owned();
    set_file_backed_slab_directory(Some(path_str.into()));
}

// TODO: we would like to re-export `slitter_allocate` and
// `slitter_release`, but cargo won't let us do that.  We
// can however generate a static archive, which will let
// the caller grab the C-side definition as needed.
