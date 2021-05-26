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
    pub fn allocate_empty_magazine(&self) -> Magazine {
        self.freelist
            .pop()
            .unwrap_or_else(|| Magazine(MagazineImpl::new(Box::leak(Box::new(Default::default())))))
    }

    #[requires(mag.is_empty(), "Only empty magazines are released to the Rack.")]
    pub fn release_empty_magazine(&self, mag: Magazine) {
        self.freelist.push(mag);
    }
}

#[test]
fn smoke_test_rack() {
    let rack = get_default_rack();
    let mag = rack.allocate_empty_magazine();

    rack.release_empty_magazine(mag);
}
