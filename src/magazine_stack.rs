//! A `MagazineStack` is a thread-safe single-linked intrusive stack
//! of magazines.
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

use std::ptr::NonNull;
use std::sync::Mutex;

use crate::magazine::Magazine;
use crate::magazine_impl::MagazineImpl;

/// A `MagazineStack` is a single-linked intrusive stack of magazines.
pub struct MagazineStack {
    inner: Mutex<Option<NonNull<MagazineImpl>>>,
}

impl MagazineStack {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(None),
        }
    }

    #[requires(mag.check_rep(None).is_ok(),
               "Magazine must make sense.")]
    pub fn push(&self, mag: Magazine) {
        assert!(mag.0.link.is_none());
        let mut stack = self.inner.lock().unwrap();

        mag.0.link = stack.take();
        *stack = Some(mag.0.into())
    }

    #[ensures(ret.is_some() ->
              ret.as_ref().unwrap().check_rep(None).is_ok(),
              "Magazine should make sense.")]
    pub fn pop(&self) -> Option<Magazine> {
        let mut stack = self.inner.lock().unwrap();

        if let Some(mag_ptr) = stack.take() {
            let mag: &'static mut _ = unsafe { &mut *mag_ptr.as_ptr() };
            std::mem::swap(&mut mag.link, &mut *stack);
            assert!(mag.link.is_none());
            Some(Magazine(mag))
        } else {
            None
        }
    }
}

// MagazineStack is safe to `Send` because we convert `NonNull`
// to/from mutable references around the lock.
unsafe impl Send for MagazineStack {}
unsafe impl Sync for MagazineStack {}

#[test]
fn magazine_stack_smoke_test() {
    let rack = crate::rack::get_default_rack();
    let stack = MagazineStack::new();

    stack.push(rack.allocate_empty_magazine());
    stack.push(rack.allocate_empty_magazine());

    assert!(stack.pop().is_some());

    stack.push(rack.allocate_empty_magazine());
    assert!(stack.pop().is_some());
    assert!(stack.pop().is_some());

    assert!(stack.pop().is_none());
}
