The high-level design of Slitter
================================

Slitter is a slab allocator that heavily relies on monotonic state to
keep its code easy to understand, even once we replace locks with
non-blocking atomic operations, and to ensure misuses crash early or
at least remain localised to the buggy allocation class.

At its core, Slitter is a [magazine-caching slab allocator](https://www.usenix.org/legacy/publications/library/proceedings/usenix01/full_papers/bonwick/bonwick.pdf),
except that the caches are per-thread rather than per-CPU.

Each allocation class must be registered before being used in
allocations or deallocations.  Classes are assigned opaque identifiers
linearly, and are immortal: once a class has been registered, it
cannot be unregistered, and its identifier is never reused (i.e.,
the set of classes is another instance of monotonic state).

The data model
--------------

The data model is hierarchical; the only deviation from a pure
hierarchy (`Mapper`, `Mill`, `Press`/`ClassInfo`, thread cache entry)
is the thread cache (one thread cache contains one entry for each
allocation class), and the `Rack`, which is shared by multiple
`ClassInfo`, independently of the `Mill`.

1. Each thread has a thread cache (`cache.rs`) for all the allocations
   classes that are known (a vector, indexed with the class's id).

2. Each cache entry refers to its class's immortal `ClassInfo` struct
   (`class.rs`).

3. The `ClassInfo` (`class.rs`) struct contains read-only information
   about the class and two freelists of magazines, and refers to an
   immortal `Rack` (`magazine.rs`), and owns a `Press` (`press.rs`).

4. The `Rack` is shared between an arbitrary number of `ClassInfo`,
   and handles the allocation and recycling of empty magazines
   (bounded vectors of cached allocations).

5. Each `Press` is specific to the class, and allocates new objects
   for the class.  Allocations are mostly serviced with a bump
   pointer; a `Press` refers to an immortal `Mill` (`mill.rs`) from
   which it obtains new address ranges for bump allocation.

6. The `Mill` is shared between an arbitrary number of `Press`es,
   and handles parcelling out address space.  Requests are again
   mostly serviced from a 1GB "chunk" with a bump pointer.  When
   a `Mill` needs a new chunk, it defers to an immortal `Mapper`
   to grab address space from the OS, release unused slop at the
   edges, and mark some of that space as ready for memory accesses.

The heap layout
---------------

Slitter carves out allocations from independent `Chunk`s of data.
Each `Chunk`'s data is a 1 GB-aligned range of 1 GB, and its array of
metadata lives in a 2 MB region that starts 4 MB below the data
region.  Slitter also keeps 2 MB guard regions before the metadata,
between the metadata and the data, and after the data.

The data region is incrementally partitioned into `Span`s, which are
always aligned to `SPAN_ALIGNMENT` both in address and in size.  Each
`SPAN_ALIGNMENT` range of data bytes maps to a `SpanMetadata` struct
in the metadata region: the first range maps to the first struct in
the region, the second to the next, etc.

A given `Span` (and thus its constituent span-aligned ranges) is only
used to satisfy allocations of a single class.  This makes it easy to
guarantee alignment, and to confirm that deallocation requests make
sense.

The allocation flow, from the outside in
----------------------------------------

The allocation fast path for a class id `C` finds the `C`th entry in
the thread-local cache, and looks at the magazine stored there.
Slitter currently only has one allocation magazine and one
deallocation magazine per cache, as well as a "buffer": our target
application mostly performs bursts of allocations and bursts of
deallocation, so locality of repeated allocations and deallocations
isn't a concern... at least not as much as checking deallocations in
the slow path.

If that magazine still has some allocations, we pop off one
allocation, and return that to the caller.

Otherwise, we must enter the slow path.

The slow path (in `cache.rs`) ensures that:

1. The thread-local cache has an entry for class id `C`; if it doesn't
   (the local cache array is too short), the local cache is extended to
   match all the allocation classes that are currently registered, which
   must include `C`, but may include other allocation classes.

2. The thread-local entry for class `C` has a non-empty magazine.

3. The allocation request is satisfied (usually from that magazine).

The magazine allocation / refilling logic lives in `magazine.rs`, and
mostly manipulates two intrusive LIFO freelists of magazines in the
class's `ClassInfo` (one immortal struct per class): one for magazines
that are fully populated, and another for magazines are partially
populated (fully empty magazines go in the `Rack`).

When the thread-local array must be extended, each entry is filled
with a magazine, in an arbitrary state.  The `ClassInfo` (all
thread-local cache entries for a given class share the same
`ClassInfo`) first pops from its freelists, and only defers to the
`Rack` when both freelists are empty (multiple `ClassInfo`s share the
same `Rack`).

When the thread-local entry instead has an empty magazine, the
`ClassInfo` refills that magazine.  If the freelists aren't empty, the
empty magazine is released to the `ClassInfo`'s `Rack` and replaced
with one from the `ClassInfo`'s freelists.  These freelists only
contain non-empty magazines, so we can always satisfy at least one
allocation from the newly popped magazine.

When the freelists are empty, the `ClassInfo` hits the `Press` (each
`ClassInfo` owns one `Press`) for new allocations: first, for the
allocation itself, and then to opportunistically refill the currently
empty magazine.

The `Press` allocates from its current `Span` with a bump pointer.
When the `Span` is exhausted, the `Press` asks its `Mill` (multiple
`Press`es share the same `Mill`) for another span, and bumps the
allocation index in that new spac.

The `Mill` allocates from its current `Chunk` with a bump pointer.
When the `Chunk` is exhausted, it asks its `Mapper` (multiple `Mill`s
share the same `Mapper`) for another one.

Finally, the mapper allocates address space by asking the operating
system.

The deallocation flow, from the outside in
------------------------------------------

The allocation fast path for a class id `C` finds the `C`th entry in
the thread-local cache, and looks at the magazine stored there.

If that magazine isn't full, we push the newly released allocation
to the magazine, and return to the caller.

Otherwise, we must enter the deallocation slow path.

The slow path (in `cache.rs`) ensures that:

1. The thread-local cache has an entry for class id `C`; if it doesn't
   (the local cache array is too short), the local cache is extended to
   match all the allocation classes that are currently registered, which
   must include `C`, but may include other allocation classes.

2. The thread-local entry for class `C` has a non-full magazine.

3. The newly released allocation is pushed to that magazine.

We must handle the case when there is no entry for class `C` in the
thread-local cache, because allocation and deallocation can happen
in different threads.

In order to get a non-full magazine, the `ClassInfo` (all the cache
entries for a given class refer to the same `ClassInfo`) pops from its
freelist of partial-filled magazines, and otherwise asks its `Rack`
(multiple `ClassInfo`s share the same `Rack`) for a new empty magazine.

The thread cache entry's current full magazine enters the `ClassInfo`'s
freelist, and is replaced by the new non-full magazine.  At this point,
there must be room to push the newly released deallocation to the
magazine in the thread cache entry.

Exceptional situations
----------------------

Until now, we have never populated the freelist of partially populated
magazines: allocation only releases empty ones back to the `Rack`, and
deallocation pushes full magazines on the `ClassInfo`'s freelist.

We need to handle partially populated magazines to clean up when
threads are shut down.

When a thread is shutdown, we must evict everything from its
thread-local cache (otherwise we'll leak magazines and allocations).
Full magazines still go to the relevant `ClassInfo`, and empty ones to
their `Rack`.  However, there may also be magazines that are neither
full nor empty; these enter the `ClassInfo`'s freelist of partial
magazines.

Once a thread has begun the shutdown process, we also don't want to
repopulate its thread cache.  We instead satisfy allocations by
hitting the `Press` directly (we should first pop from any non-empty
magazines), and deallocations by grabbing a non-full magazine, pushing
to it, and immediately giving the resulting magazing back to the
`ClassInfo`.

The allocation slow path, from the inside out
---------------------------------------------

Each `Mill` carves out `Span`s from one `Chunk` at a time.  A `Chunk`
is a data region of 1 GB aligned to 1 GB, with a 2 MB region of
metadata that begins 4 MB before the data region.  A `Mill` obtains
such a chunk of data and associated metadata by asking a `Mapper` to
reserve address space, using the same `Mapper` to cut off any
over-reservation at the edges, and finally letting the `Mapper` ask
the OS to back the data and metadata regions with memory.

Within a `Chunk`'s data region, `Span`s are aligned to 16 KB, and each
such 16 KB range is associated 1:1 with an entry in the metadata array.
The metadata for a Span-aligned range must thus not exceed 32 bytes
(a constraint that is checked at compile-time), so that the metadata
array can fit in the 2 MB region.

This layout avoids interleaving Slitter metadata in-band with the
mutator's allocations.  The `Mill` also leaves guard pages not only
between the metadata and the data region, but also before the metadata
region and after the data region.  This makes it unlikely that buffer
overflows will affect a metadata region, or scribble past a Slitter
chunk.

The simple mapping between Span-aligned ranges and `SpanMetadata`s in
the metadata array means we can efficiently check that a deallocation
request is valid.  While we currently only confirm that the class id
provided by the caller matches the class in the allocation's
`SpanMetadata`, we plan to add more checks:

1. The deallocated address must be a multiple of the allocation size
2. The metadata region must actually be managed by Slitter

Chunks never go away: once allocated, they are immortal.  That's why
it's important to avoid fragmenting the address space with chunks.

When a `Press` needs a new bump region, its `Mill` will return a
single `Span` that usually contains multiple Span-aligned ranges in
the data region, and is thus associated with multiple `SpanMetadata`
objects.  The `Press` will use one of these metadata objects to track
its bump allocation, but must initialise all of them to signal that
the corresponding ranges belong to the `Press`'s allocation class.

Once a Span-aligned range is associated with an allocation class, it
stays that way: the address range is never released back to the OS,
nor can it be recycled for a different class.

Eventually, a `ClassInfo` will ask its press for a newly allocated
object.  That allocation will either be immediately returned to the
mutator, or cached in a magazine.  Either way, an allocation never
returns to the `Press`: it will always be owned by the mutator, or by
a magazine.

A `ClassInfo` also manages magazines.  Each `ClassInfo` manages its
own freelist of non-empty magazines (they contain allocations
for the `ClassInfo`'s class, so are specific to that `ClassInfo`).

However, `ClassInfo`s defer to their `Rack` for empty magazines.  A
`Rack` allocates and releases empty magazines.  When we switch to
lock-free `MagazineStack`s, `Magazine` too will become immortal: while
they will be allocated arbitrarily (from the system allocator at
first), they will never be released, and will instead enter the
`Rack`'s freelist.

This monotonicity will make it trivial to implement lock-free stacks
without safe memory protection.
