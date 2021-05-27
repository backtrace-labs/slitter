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

use std::mem::MaybeUninit;
use std::ptr::NonNull;
use std::sync::atomic::AtomicPtr;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use crate::magazine::Magazine;
use crate::magazine_impl::MagazineImpl;
use crate::magazine_impl::MagazineStorage;

/// A `MagazineStack` is a single-linked stack with a generation
/// counter to protect against ABA.  We do not have to worry
/// about reclamation races because `MagazineStorage` are immortal:
/// `Rack`s never free them, and simply cache empty magazines in
/// a `MagazineStack`.
#[repr(C)]
#[repr(align(16))]
pub struct MagazineStack {
    top_of_stack: AtomicPtr<MagazineStorage>,
    generation: AtomicUsize,
}

// These are declared in stack.h
extern "C" {
    fn slitter__stack_push(stack: &MagazineStack, mag: NonNull<MagazineStorage>);
    fn slitter__stack_pop(stack: &MagazineStack, out_mag: *mut NonNull<MagazineStorage>) -> bool;
    fn slitter__stack_try_pop(
        stack: &MagazineStack,
        out_mag: *mut NonNull<MagazineStorage>,
    ) -> bool;
}

impl MagazineStack {
    pub fn new() -> Self {
        Self {
            top_of_stack: Default::default(),
            generation: AtomicUsize::new(0),
        }
    }

    #[requires(mag.check_rep(None).is_ok(),
               "Magazine must make sense.")]
    #[inline(always)]
    pub fn push<const PUSH_MAG: bool>(&self, mag: Magazine<PUSH_MAG>) {
        if let Some(storage) = mag.0.storage() {
            unsafe { slitter__stack_push(&self, storage.into()) }
        }
    }

    #[ensures(ret.is_some() ->
              ret.as_ref().unwrap().check_rep(None).is_ok(),
              "Magazine should make sense.")]
    #[inline(always)]
    pub fn pop<const PUSH_MAG: bool>(&self) -> Option<Magazine<PUSH_MAG>> {
        if self.top_of_stack.load(Ordering::Relaxed).is_null() {
            return None;
        }

        let mut dst: MaybeUninit<NonNull<MagazineStorage>> = MaybeUninit::uninit();
        if unsafe { slitter__stack_pop(&self, dst.as_mut_ptr()) } {
            // If `stack_pop` returns true, `dst` must contain a valid owning pointer
            // to a `MagazineStorage`.
            let storage = unsafe { &mut *dst.assume_init().as_ptr() };
            Some(Magazine(MagazineImpl::new(Some(storage))))
        } else {
            None
        }
    }

    #[ensures(ret.is_some() ->
              ret.as_ref().unwrap().check_rep(None).is_ok(),
              "Magazine should make sense.")]
    #[inline(always)]
    pub fn try_pop<const PUSH_MAG: bool>(&self) -> Option<Magazine<PUSH_MAG>> {
        if self.top_of_stack.load(Ordering::Relaxed).is_null() {
            return None;
        }

        let mut dst: MaybeUninit<NonNull<MagazineStorage>> = MaybeUninit::uninit();
        if unsafe { slitter__stack_try_pop(&self, dst.as_mut_ptr()) } {
            // If `stack_pop` returns true, `dst` must contain a valid owning pointer
            // to a `MagazineStorage`.
            let storage = unsafe { &mut *dst.assume_init().as_ptr() };
            Some(Magazine(MagazineImpl::new(Some(storage))))
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

    // Push/pop shouldn't care about the magazines' polarity.
    stack.push(rack.allocate_empty_magazine::<false>());
    stack.push(rack.allocate_empty_magazine::<true>());

    assert!(stack.pop::<false>().is_some());

    stack.push(rack.allocate_empty_magazine::<true>());
    assert!(stack.pop::<true>().is_some());
    assert!(stack.pop::<false>().is_some());

    assert!(stack.pop::<true>().is_none());
}
