//! A `Rack` manages empty `Magazine`s: it allocates them as needed,
//! and recycles unused empty ones.
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

use crate::magazine::Magazine;
use crate::magazine_impl::MagazineImpl;
use crate::magazine_stack::MagazineStack;

/// A `Rack` allocates and recycles empty magazines.
pub struct Rack {
    freelist: MagazineStack,
}

impl Rack {
    pub fn new() -> Self {
        use crate::magazine_impl::MagazineStorage;
        use crate::magazine_impl::MAGAZINE_SIZE;

        extern "C" {
            fn slitter__magazine_capacity() -> usize;
            fn slitter__magazine_storage_sizeof() -> usize;
            fn slitter__magazine_sizeof() -> usize;
        }

        unsafe {
            assert_eq!(MAGAZINE_SIZE as usize, slitter__magazine_capacity());
            assert_eq!(
                std::mem::size_of::<MagazineStorage>(),
                slitter__magazine_storage_sizeof()
            );
            assert_eq!(
                std::mem::size_of::<MagazineImpl<true>>(),
                slitter__magazine_sizeof()
            );
            assert_eq!(
                std::mem::size_of::<MagazineImpl<false>>(),
                slitter__magazine_sizeof()
            );
            assert_eq!(
                std::mem::size_of::<Magazine<true>>(),
                slitter__magazine_sizeof()
            );
            assert_eq!(
                std::mem::size_of::<Magazine<false>>(),
                slitter__magazine_sizeof()
            );
        }

        Self {
            freelist: MagazineStack::new(),
        }
    }
}

/// Returns a reference to the global default magazine rack.
pub fn get_default_rack() -> &'static Rack {
    lazy_static::lazy_static! { static ref RACK: Rack = Rack::new(); };

    &RACK
}

impl Rack {
    #[ensures(ret.is_empty(), "Newly allocated magazines are empty.")]
    #[inline(always)]
    pub fn allocate_empty_magazine<const PUSH_MAG: bool>(&self) -> Magazine<PUSH_MAG> {
        self.freelist.pop().unwrap_or_else(|| {
            Magazine(MagazineImpl::new(Some(Box::leak(Box::new(
                Default::default(),
            )))))
        })
    }

    #[requires(mag.is_empty(), "Only empty magazines are released to the Rack.")]
    pub fn release_empty_magazine<const PUSH_MAG: bool>(&self, mag: Magazine<PUSH_MAG>) {
        // This function is only called during thread shutdown, and
        // things will really break if mag is actually non-empty.
        assert!(mag.is_empty());
        self.freelist.push(mag);
    }
}

#[test]
fn smoke_test_rack() {
    let rack = get_default_rack();
    let mag = rack.allocate_empty_magazine::<true>();

    rack.release_empty_magazine(mag);
}
