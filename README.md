# slitter-wip
Temporary review repo for the slitter allocator

Slitter is a less footgunny slab allocator
==========================================

Slitter is a classically structured thread-caching slab allocator
that's meant to help write reliable long-running programs.

Given this goal, Slitter does not prioritise speed.  The primary goal
is instead to help applications handle inevitable memory management
errors--be it with built-in statistics, with runtime detection, or by
controlling their blast radius--while keeping the allocator's
performance competitive with the state of the art.  The other
important goal is to let applications customise how Slitter requests
memory from the operating system.

See `doc/fast_path.md` for details on allocation performance.  For the
rest of the allocator at a high level, refer to `doc/design.md`.  Due
to the type-stability guarantee, external fragmentation is essentially
out of scope, and the slab/bump pointer allocation scheme keeps
internal fragmentation to a very low level (only for metadata and
guard pages, on the order of 2-3%, more once we add internal guard
pages).

Current guarantees:

1. Slitter will detect mismatching classes when recycling allocations,
   and will often also crash when it receives an address that it does
   not manage.  Without this guarantee, it's too easy for heap usage
   statistics to become useless, and for incorrectly paired release
   calls to turn into memory corruption, far from the erroneous call.

2. Slitter does not have any in-band metadata.  This means no metadata
   next to the application's allocations, ripe for buffer overflows
   (we maintain guard pages between data and metadata), and also no
   intrusive linked list that would be vulnerable to use-after-free.

3. Slitter-allocated data has a stable type: once an address has been
   returned to the mutator for a given allocation class, that address
   will always be valid, and will always be used for data of that
   class.  Per #2, Slitter does not use intrusive linked lists, so the
   data reflects what the application stored there, even once it has
   been recycled.  This lets application code rely on benign
   use-after-free in non-blocking algorithms instead of, e.g., SMR.
   The guarantee also means that any double-free or malign
   use-after-free will only affect the faulty allocation class.

Future guarantees:

4. Slitter will detect when an interior pointer is freed.

5. Slitter will detect most buffer overflows that cross allocation
   classes, with guard pages.

6. Slitter will always detect frees of addresses it does not manage.

7. Slitter will detect most back-to-back double-frees.

Future features:

a. Slitter will let each allocation class determine how its backing
   memory should be allocated (e.g., cold data could live in a
   file-backed mapping for opt-in swapping).

b. Slitter will track the number of objects allocated and recycled in
   each allocation class.

c. Slitter will sample a small fraction of allocations for heap
   profiling.
