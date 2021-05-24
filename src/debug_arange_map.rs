//! This module tracks metadata about mapped address ranges in debug
//! builds.
use std::collections::BTreeMap;
use std::sync::Mutex;

#[derive(Clone, Copy)]
struct Range {
    begin: usize,
    size: usize,
}

#[derive(Clone, Copy)]
struct AddressRange {
    /// The range of address space reserved.
    begin: usize,
    size: usize,

    /// If populated, the *one* metadata range completely inside this
    /// reserved range.
    metadata: Option<Range>,

    /// If populated, the one data range completely inside this
    /// reserved range.
    data: Option<Range>,
}

lazy_static::lazy_static! {
    static ref ADDRESS_RANGE_MAP: Mutex<BTreeMap<usize, AddressRange>> = Default::default();
}

/// Returns the address range associated with the highest key less
/// than or equal to `ptr`.
fn predecessor(ptr: usize) -> Option<AddressRange> {
    let map = ADDRESS_RANGE_MAP.lock().unwrap();
    map.range(0..=ptr).last().map(|x| *x.1)
}

/// Registers a new address range.  It must not overlap with any other
/// registered range.
pub fn reserve_range(begin: usize, size: usize) -> Result<(), &'static str> {
    if usize::MAX - begin < size {
        return Err("Address is too high.");
    }

    let mut map = ADDRESS_RANGE_MAP.lock().unwrap();

    // Make sure nothing overlaps with the new range.
    for (_, info) in map.range(0..=(begin + size)).rev() {
        // We're walking allocations from the top down.  If the
        // current allocation is too high, keep looking.
        if info.begin >= begin + size {
            continue;
        }

        // If the current allocation is too low, stop.
        if begin >= info.begin + info.size {
            break;
        }

        //return Err("Found address range");
    }

    map.insert(
        begin,
        AddressRange {
            begin,
            size,
            metadata: None,
            data: None,
        },
    );
    Ok(())
}

pub fn releasable_range(begin: usize, size: usize) -> Result<(), &'static str> {
    if size > usize::MAX - begin {
        return Err("Range too large");
    }

    let reserved = predecessor(begin).ok_or("Parent range not found")?;

    if begin >= reserved.begin + reserved.size {
        return Err("Parent range too short for begin");
    }

    if begin + size > reserved.begin + reserved.size {
        return Err("Parent range too short for size");
    }

    if reserved.begin == begin && reserved.size == size {
        return Ok(());
    }

    if let Some(meta) = reserved.metadata {
        if !(begin >= meta.begin + meta.size || begin + size <= meta.begin) {
            return Err("Released range overlaps with metadata region");
        }
    }

    if let Some(data) = reserved.data {
        if !(begin >= data.begin + data.size || begin + size <= data.begin) {
            return Err("Released range overlaps with data region");
        }
    }

    if reserved.begin == begin {
        return Ok(());
    }

    if reserved.begin + reserved.size == begin + size {
        return Ok(());
    }

    Err("Released range is in the middle of the reservation")
}

/// Unregisters a fragment of a pre-existing address range.  The
/// fragment must be at either end of the registered range.
///
/// Unless the range is fully released, its data or metadata must not
/// overlap with the released range.
pub fn release_range(begin: usize, size: usize) -> Result<(), &'static str> {
    if size > usize::MAX - begin {
        return Err("Range too large");
    }

    let reserved = predecessor(begin).ok_or("Parent range not found")?;

    if begin >= reserved.begin + reserved.size {
        return Err("Parent range too short for begin");
    }

    if begin + size > reserved.begin + reserved.size {
        return Err("Parent range too short for size");
    }

    let mut map = ADDRESS_RANGE_MAP.lock().unwrap();

    if reserved.begin == begin && reserved.size == size {
        map.remove(&begin);
        return Ok(());
    }

    if let Some(meta) = reserved.metadata {
        if !(begin >= meta.begin + meta.size || begin + size <= meta.begin) {
            return Err("Released range overlaps with metadata region");
        }
    }

    if let Some(data) = reserved.data {
        if !(begin >= data.begin + data.size || begin + size <= data.begin) {
            return Err("Released range overlaps with data region");
        }
    }

    if reserved.begin == begin {
        map.remove(&begin);
        assert!(size < reserved.size);
        map.insert(
            begin + size,
            AddressRange {
                begin: begin + size,
                size: reserved.size - size,
                metadata: reserved.metadata,
                data: reserved.data,
            },
        );
        return Ok(());
    }

    if reserved.begin + reserved.size == begin + size {
        let entry: &mut _ = map
            .get_mut(&reserved.begin)
            .ok_or("Parent range not found on second lookup")?;

        assert!(size < entry.size);
        entry.size -= size;
        return Ok(());
    }

    Err("Released range is in the middle of the reservation")
}

pub fn can_mark_metadata(begin: usize, size: usize) -> Result<usize, &'static str> {
    if size > usize::MAX - begin {
        return Err("Range too large");
    }

    let reserved = predecessor(begin).ok_or("Parent range not found")?;
    if begin < reserved.begin {
        return Err("Metadata address too low");
    }

    if begin + size > reserved.begin + reserved.size {
        return Err("Metadata address too high");
    }

    if reserved.metadata.is_some() {
        return Err("Metadata registered twice");
    }

    if let Some(data) = reserved.data {
        if begin + size <= data.begin {
            return Ok(reserved.begin);
        }

        if begin >= data.begin + data.size {
            return Ok(reserved.begin);
        }

        return Err("Metadata range overlaps with data");
    }

    Ok(reserved.begin)
}

/// Marks a new metadata subrange in a previously reserved range.
pub fn mark_metadata(begin: usize, size: usize) -> Result<(), &'static str> {
    let reservation_begin = can_mark_metadata(begin, size)?;

    let mut map = ADDRESS_RANGE_MAP.lock().unwrap();
    let entry: &mut _ = map
        .get_mut(&reservation_begin)
        .ok_or("Parent range not found on second lookup")?;

    if entry.metadata.is_some() {
        return Err("Metadata registered twice");
    }

    entry.metadata = Some(Range { begin, size });
    Ok(())
}

pub fn can_mark_data(begin: usize, size: usize) -> Result<usize, &'static str> {
    if size > usize::MAX - begin {
        return Err("Range too large");
    }

    let reserved = predecessor(begin).ok_or("Parent range not found")?;
    if begin < reserved.begin {
        return Err("Data address too low");
    }

    if begin + size > reserved.begin + reserved.size {
        return Err("Data address too high");
    }

    if reserved.data.is_some() {
        return Err("Data registered twice");
    }

    if let Some(data) = reserved.data {
        if begin + size <= data.begin {
            return Ok(reserved.begin);
        }

        if begin >= data.begin + data.size {
            return Ok(reserved.begin);
        }

        return Err("Data range overlaps with metadata");
    }

    Ok(reserved.begin)
}

/// Marks a new data subrange in a previously reserved range.
pub fn mark_data(begin: usize, size: usize) -> Result<(), &'static str> {
    let reservation_begin = can_mark_data(begin, size)?;

    let mut map = ADDRESS_RANGE_MAP.lock().unwrap();
    let entry: &mut _ = map
        .get_mut(&reservation_begin)
        .ok_or("Parent range not found on second lookup")?;

    if entry.data.is_some() {
        return Err("Data registered twice");
    }

    entry.data = Some(Range { begin, size });
    Ok(())
}

/// Returns Ok if the range is fully in a metadata region.
pub fn is_metadata(begin: usize, size: usize) -> Result<(), &'static str> {
    if size > usize::MAX - begin {
        return Err("Range too large");
    }

    let reserved = predecessor(begin).ok_or("Parent range not found")?;
    let range = reserved.metadata.ok_or("Parent range has no metadata")?;

    if begin < range.begin {
        return Err("Address below metadata");
    }

    if begin + size > range.begin + range.size {
        return Err("Address above metadata");
    }

    Ok(())
}

/// Returns Ok if the range is fully in a data region.
pub fn is_data(begin: usize, size: usize) -> Result<(), &'static str> {
    if size > usize::MAX - begin {
        return Err("Range too large");
    }

    let reserved = predecessor(begin).ok_or("Parent range not found")?;
    let range = reserved.data.ok_or("Parent range has no metadata")?;
    if begin < range.begin {
        return Err("Address below data");
    }

    if begin + size > range.begin + range.size {
        return Err("Address above data");
    }

    Ok(())
}
