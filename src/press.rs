//! A `Press` creates new allocations for a given `Class`.  The
//! allocations must be such that the `Press` can also map valid
//! addresses back to their `Class`.
//!
//! While each class gets its own press, the latter requirement means
//! that the presses must all implement compatible metadata stashing
//! schemes.
//!
//! For now, we assume that each `Press` allocates data linearly (with
//! a bump pointer) from 2 MB-aligned chunks of 2 MB, and hides the
//! corresponding metadata 8 KB *before* that chunk, with a guard page
//! between the chunk's metadata and the actual chunk data, and more
//! guard pages before the metadata and after the chunk itself.
//!
//! We enable mostly lock-free operations by guaranteeing that each
//! chunk and corresponding metadata is immortal once allocated.
use crate::linear_ref::LinearRef;
use crate::mill;
use crate::mill::ChunkMetadata;
use crate::mill::Mill;
use crate::mill::MAX_DATA_SIZE;
use crate::Class;
use std::alloc::Layout;
use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::atomic::AtomicPtr;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::Mutex;

#[derive(Debug)]
pub struct Press {
    /// The current chunk that services bump pointer allocation.
    bump: AtomicPtr<ChunkMetadata>,

    /// Writes to the bump itself (i.e., updating the `AtomicPtr`
    /// itself) go through this lock.
    mill: Mutex<&'static Mill>,
    layout: Layout,
    class: Class,
}

/// Returns Ok if the allocation `address` might have come from a `Press` for `class`.
///
/// # Errors
///
/// Returns Err if the address definitely did not come from that `class`.
#[inline]
pub fn check_allocation(class: Class, address: usize) -> Result<(), &'static str> {
    let meta_ptr = ChunkMetadata::from_allocation_address(address);

    let meta = unsafe { meta_ptr.as_mut() }.ok_or("Derived a bad metadata address")?;
    if meta.class_id != Some(class.id()) {
        Err("Incorrect class id")
    } else {
        Ok(())
    }
}

impl Press {
    pub fn new(class: Class, mut layout: Layout) -> Result<Self, &'static str> {
        layout = layout.pad_to_align();

        if layout.size() > MAX_DATA_SIZE / 2 {
            Err("Class elements too large (after alignment)")
        } else {
            Ok(Self {
                bump: Default::default(),
                mill: Mutex::new(mill::get_default_mill()),
                layout,
                class,
            })
        }
    }

    /// Attempts to allocate one object by bumping the metadata
    /// pointer.
    fn try_allocate_from_chunk(&self, meta: &mut ChunkMetadata) -> Option<LinearRef> {
        let allocated_id = meta.bump_ptr.fetch_add(1, Ordering::Relaxed);

        if allocated_id >= meta.bump_limit as usize {
            return None;
        }

        let address = meta.chunk_begin + allocated_id * self.layout.size();
        Some(LinearRef::new(NonNull::new(address as *mut c_void)?))
    }

    /// Attempts to replace our bump pointer with a new one.
    fn try_replace_chunk(&self, expected: *mut ChunkMetadata) -> Result<(), i32> {
        if self.bump.load(Ordering::Relaxed) != expected {
            // Someone else made progress.
            return Ok(());
        }

        let mill = self.mill.lock().unwrap();
        // Check again with the lock held, before allocating a new chunk.
        if self.bump.load(Ordering::Relaxed) != expected {
            return Ok(());
        }

        let range = mill.get_chunk()?;
        let meta = range.meta;

        meta.class_id = Some(self.class.id());
        meta.bump_limit = (range.data_size / self.layout.size()) as u32;
        assert!(
            meta.bump_limit > 0,
            "layout.size > MAX_DATA_SIZE, but we check for that in the constructor."
        );
        meta.bump_ptr = AtomicUsize::new(0);
        meta.chunk_begin = range.data as usize;

        // Publish the metadata for our fresh chunk.
        assert_eq!(self.bump.load(Ordering::Relaxed), expected);
        self.bump.store(meta, Ordering::Release);
        Ok(())
    }

    /// Attempts to allocate one object.  Returns Ok(_) if we tried to
    /// allocate from the current bump region.
    ///
    /// # Errors
    ///
    /// Returns `Err` if we failed to grab a new bump region.
    fn try_allocate_once(&self) -> Result<Option<LinearRef>, i32> {
        let meta_ptr: *mut ChunkMetadata = self.bump.load(Ordering::Acquire);

        if let Some(meta) = unsafe { meta_ptr.as_mut() } {
            if let Some(ret) = self.try_allocate_from_chunk(meta) {
                return Ok(Some(ret));
            }
        }

        // Either we didn't find any chunk metadata, and bump
        // allocation failed.  Either way, let's try to put
        // a new chunk in.
        self.try_replace_chunk(meta_ptr).map(|_| None)
    }

    pub fn allocate_one_object(&self) -> Option<LinearRef> {
        loop {
            match self.try_allocate_once() {
                Err(_) => return None, // TODO: log
                Ok(Some(ret)) => return Some(ret),
                _ => continue,
            }
        }
    }
}
