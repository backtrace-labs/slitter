Slitter is a less footgunny slab allocator
==========================================
[![Build Status](https://app.travis-ci.com/backtrace-labs/slitter.svg?branch=main)](https://app.travis-ci.com/backtrace-labs/slitter)

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

How to use Slitter as a C library
---------------------------------

In order to use Slitter as a C library, we must first build a static
library with Cargo. Slitter can *only* be called from C via static
linkage because Cargo will otherwise hide our C functions.  However,
this also exposes internal Rust symbols, so there can only be one
statically linked Rust library in any executable.

See `examples/demo.c` for a sample integration, which also
demonstrates some of the additional checks unlocked by explicit
allocation class tags.  Execute that file in sh (i.e., `sh
examples/demo.c`) to build Slitter, build `demo.c` and link it against
Slitter, and run the resulting `demo` executable.

The `#ifdef MISMATCH` section allocates an object of a derived struct
type, and releases it as the base type.  The base field is the first
member of the derived struct, so a plain malloc/free interface is
unable to tell the difference.  However, since the caller must tell
`slitter_release` what allocation class it expects the freed pointer
to be in, Slitter can detect the mismatch.

How to use Slitter within a Rust uber-crate
-------------------------------------------

When linking multiple Rust libraries with other languages like C or
C++, one must build a single statically linked (`crate-type =
["staticlib"]`) Rust crate that depends on all the Rust libraries we
want to expose, and make sure to re-export the public `extern "C"` 
definitions from each of the dependencies.
    
How to use Slitter from Rust
----------------------------

We haven't explored that use case, except for tests.  You can look at
the unit tests at the bottom on `src/class.rs`.  TL;DR: create new
`Class` objects (they're copiable wrappers around `NonZeroU32`), and
call `Class::allocate` and `Class::release`.
