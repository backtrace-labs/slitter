mod cache;
mod class;
mod individual;
mod linear_ref;
mod magazine;
mod map;
mod mill;
mod press;

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

use std::ffi::c_void;
use std::ptr::NonNull;

pub use class::Class;
pub use class::ClassConfig;
pub use class::ForeignClassConfig;

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

#[no_mangle]
pub extern "C" fn slitter_allocate(class: Class) -> *mut c_void {
    class.allocate().expect("Allocation must succeed").as_ptr()
}

#[no_mangle]
pub extern "C" fn slitter_release(class: Class, ptr: *mut c_void) {
    if let Some(block) = NonNull::new(ptr) {
        class.release(block);
    }
}
