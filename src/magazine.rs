//! The cache layer always allocates from and releases into small
//! arrays of pointers to pre-allocated block.  These small arrays are
//! "magazines," and are themselves allocated and released by a
//! "rack."
use crate::linear_ref::LinearRef;

const MAGAZINE_SIZE: u32 = 30;

#[repr(C)] // We play fun tricks with zero-intialisation.
pub struct Magazine {
    // The `allocations` array is populated from the bottom up;
    // the first `num_allocated` indices have NonNull values,
    // and the remainder are undefined.
    num_allocated: u32,
    allocations: [Option<LinearRef>; MAGAZINE_SIZE as usize],
}

/// A `Rack` allocates and recycles empty magazines.
pub struct Rack {
    // No state yet.
}

/// Returns a reference to the global default magazine rack.
pub fn get_default_rack() -> &'static Rack {
    lazy_static::lazy_static! { static ref RACK: Rack = Rack{}; };

    &RACK
}

impl Magazine {
    /// Attempts to get an unused block from the magazine.
    pub fn get(&mut self) -> Option<LinearRef> {
        if self.num_allocated == 0 {
            return None;
        }

        self.num_allocated -= 1;
        self.allocations[self.num_allocated as usize].take()
    }

    /// Attempts to put an unused block back in the magazine.
    ///
    /// Returns that unused block on failure.
    pub fn put(&mut self, freed: LinearRef) -> Option<LinearRef> {
        let index = self.num_allocated;
        if index >= MAGAZINE_SIZE {
            return Some(freed);
        }

        self.num_allocated += 1;
        self.allocations[index as usize] = Some(freed);
        None
    }

    /// Fills `self` with allocations returned by `allocator`
    pub fn populate(&mut self, mut allocator: impl FnMut() -> Option<LinearRef>) {
        let mut count = self.num_allocated as usize;

        while count < MAGAZINE_SIZE as usize {
            match allocator() {
                Some(block) => self.allocations[count] = Some(block),
                None => break,
            }

            count += 1;
        }

        self.num_allocated = count as u32;
    }
}

impl Default for Magazine {
    fn default() -> Self {
        // Proof that Magazine its constituents are FFI-safe.
        #[allow(dead_code)]
        extern "C" fn unused(_mag: Magazine, _ref: Option<LinearRef>) {}

        // This is safe, despite using `NonNull`: `Option<NonNull<T>>`
        // has the same layout as `*T`
        // (https://doc.rust-lang.org/std/option/index.html#representation),
        // and that's FFI-safe
        // (https://rust-lang.github.io/unsafe-code-guidelines/layout/enums.html#discriminant-elision-on-option-like-enums).
        // We also know that we only run on implementations of C where NULL
        // is all zero bits.
        unsafe { std::mem::zeroed() }
    }
}

impl Rack {
    pub fn allocate_empty_magazine(&self) -> Box<Magazine> {
        Box::new(Default::default())
    }

    pub fn release_empty_magazine(&self, mag: Box<Magazine>) {
        // We can only release empty magazines.
        assert_eq!(mag.num_allocated, 0);
        // And now drop it.
    }
}

impl crate::class::ClassInfo {
    /// Returns a magazine; it may be empty, full, or partially
    /// populated.
    #[inline(never)]
    pub(crate) fn allocate_magazine(&self) -> Box<Magazine> {
        self.rack.allocate_empty_magazine()
    }

    /// Attempts to return one allocation and to refill `mag`.
    #[inline(never)]
    pub(crate) fn refill_magazine(&self, mag: &mut Box<Magazine>) -> Option<LinearRef> {
        let allocated = self.allocate_slow()?;

        mag.populate(|| self.allocate_slow());
        Some(allocated)
    }

    /// Acquires ownership of `spilled` and all cached allocations from
    /// the magazine, and removes some allocations from `mag`.
    #[inline(never)]
    pub(crate) fn clear_magazine(&self, mag: &mut Box<Magazine>, spilled: Option<LinearRef>) {
        while let Some(block) = mag.get() {
            self.release_slow(block);
        }

        if let Some(block) = spilled {
            assert_eq!(mag.put(block), None);
        }
    }

    /// Acquires ownership of `mag` and its cached allocations.
    #[inline(never)]
    pub(crate) fn release_magazine(&self, mut mag: Box<Magazine>) {
        self.clear_magazine(&mut mag, None);
        self.rack.release_empty_magazine(mag);
    }
}

#[test]
fn smoke_test_rack() {
    let rack = get_default_rack();
    let mag = rack.allocate_empty_magazine();

    rack.release_empty_magazine(mag);
}

#[test]
fn smoke_test_magazine() {
    let rack = get_default_rack();
    let mut mag = rack.allocate_empty_magazine();

    // Getting an empty magazine should return None
    assert_eq!(mag.get(), None); // mag: []

    // And getting it again should still return None.
    assert_eq!(mag.get(), None); // mag: []

    assert_eq!(mag.put(LinearRef::from_address(1)), None); // mag: [1]
    assert_eq!(mag.put(LinearRef::from_address(2)), None); // mag: [1, 2]

    assert_eq!(mag.get(), Some(LinearRef::from_address(2))); // mag: [1]

    assert_eq!(mag.put(LinearRef::from_address(3)), None); // mag: [1, 3]

    assert_eq!(mag.get(), Some(LinearRef::from_address(3))); // mag: [1]
    assert_eq!(mag.get(), Some(LinearRef::from_address(1))); // mag: []

    rack.release_empty_magazine(mag);
}

#[test]
fn magazine_fill_up() {
    let rack = get_default_rack();
    let mut mag = rack.allocate_empty_magazine();

    // Fill up the magazine.
    for i in 1..=MAGAZINE_SIZE {
        assert_eq!(mag.num_allocated, i - 1);
        assert_eq!(mag.put(LinearRef::from_address(i as usize)), None);
        assert_eq!(mag.num_allocated, i);
    }

    // This insert should fail
    assert_eq!(
        mag.put(LinearRef::from_address(usize::MAX)),
        Some(LinearRef::from_address(usize::MAX))
    );
    assert_eq!(mag.num_allocated, MAGAZINE_SIZE);

    // We should pop in LIFO order.
    for i in (1..=MAGAZINE_SIZE).rev() {
        assert_eq!(mag.num_allocated, i);
        assert_eq!(mag.get(), Some(LinearRef::from_address(i as usize)));
        assert_eq!(mag.num_allocated, i - 1);
    }

    // And now the magazine should be empty.
    assert_eq!(mag.num_allocated, 0);
    // So all subsequent `get()` calls will return None.
    assert_eq!(mag.get(), None);
    assert_eq!(mag.get(), None);
    assert_eq!(mag.num_allocated, 0);

    rack.release_empty_magazine(mag);
}

#[test]
fn magazine_populate() {
    let rack = get_default_rack();
    let mut mag = rack.allocate_empty_magazine();

    // Fill up the magazine.
    let mut count = 0usize;
    mag.populate(|| {
        count += 1;
        Some(LinearRef::from_address(count))
    });

    assert_eq!(mag.num_allocated, MAGAZINE_SIZE);

    // We should pop in LIFO order.
    for i in (1..=MAGAZINE_SIZE).rev() {
        assert_eq!(mag.num_allocated, i);
        assert_eq!(mag.get(), Some(LinearRef::from_address(i as usize)));
        assert_eq!(mag.num_allocated, i - 1);
    }

    // And now the magazine should be empty.
    assert_eq!(mag.num_allocated, 0);

    rack.release_empty_magazine(mag);
}
