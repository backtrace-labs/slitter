mod cache;
mod class;
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

pub use class::Class;
pub use class::ClassConfig;
pub use class::ForeignClassConfig;
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

// TODO: we would like to re-export `slitter_allocate` and
// `slitter_release`, but cargo won't let us do that.  We
// can however generate a static archive, which will let
// the caller grab the C-side definition as needed.
