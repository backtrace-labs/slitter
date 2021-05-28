The allocation and release fast paths
=====================================

Slitter attempts to use as much Rust as possible; while this does
imply a certain loss of control, the structure of a thread-caching
slab allocator means that we can easily pinpoint what little code
is really hot, and rewrite that in C or assembly language.

Allocations
-----------

The core of object allocation is `slitter_allocate`.

```
void *
slitter_allocate(struct slitter_class class)
{
	struct magazine *restrict mag;
	size_t next_index;
	uint32_t id = class.id;

	if (__builtin_expect(id >= slitter_cache.n, 0))
		return slitter__allocate_slow(class);

	mag = &slitter_cache.mags[id].alloc;
	if (__builtin_usubl_overflow(mag->top_of_stack, 2, &next_index)) {
		next_index++;
	}

	if (__builtin_expect(slitter__magazine_is_exhausted(mag), 0))
		return slitter__allocate_slow(class);

	/*
	 * The magazine was not empty, so next_index did not overflow
	 * by more than 1.
	 */
	__builtin_prefetch(mag->storage->allocations[next_index], 1);
	return slitter__magazine_get_non_empty(mag);
}
```

Which assembles to something like the following on x86-64:

```
   0:   48 8b 15 00 00 00 00    mov    0x0(%rip),%rdx        # 7 <slitter_allocate+0x7>
                        3: R_X86_64_GOTTPOFF    slitter_cache-0x4
   7:   89 f8                   mov    %edi,%eax
   9:   64 48 3b 02             cmp    %fs:(%rdx),%rax
   d:   73 39                   jae    48 <slitter_allocate+0x48>  ; Check if the cache must be (re-)initialised
   f:   48 c1 e0 05             shl    $0x5,%rax
  13:   64 48 03 42 08          add    %fs:0x8(%rdx),%rax
  18:   48 8b 10                mov    (%rax),%rdx
  1b:   48 83 fa 02             cmp    $0x2,%rdx
  1f:   48 89 d6                mov    %rdx,%rsi
  22:   48 83 d6 fe             adc    $0xfffffffffffffffe,%rsi    ; Generate the prefetch index
  26:   48 85 d2                test   %rdx,%rdx
  29:   74 1d                   je     48 <slitter_allocate+0x48>  ; Is the magazine empty?
  2b:   48 8b 48 08             mov    0x8(%rax),%rcx              ; Load the magazine's array
  2f:   48 83 ea 01             sub    $0x1,%rdx
  33:   48 8b 34 f1             mov    (%rcx,%rsi,8),%rsi
  37:   0f 18 0e                prefetcht0 (%rsi)                  ; Prefetch the next allocation
  3a:   48 89 10                mov    %rdx,(%rax)                 ; Update the allocation index
  3d:   48 8b 04 d1             mov    (%rcx,%rdx,8),%rax          ; Grab our allocation
  41:   c3                      retq   
  42:   66 0f 1f 44 00 00       nopw   0x0(%rax,%rax,1)
  48:   e9 00 00 00 00          jmpq   4d <slitter_allocate+0x4d>
                        49: R_X86_64_PLT32      slitter__allocate_slow-0x4
```

The first branch is taken ~once per thread, and the second once per
30-allocation magazine.  Most of the remaining instructions are used
to prefetch the next allocation; the prefetch isn't part of any
dependency chain, and code that allocates memory typically doesn't
saturate execution units, so that's not a problem.

When we must refill the magazine, the slow path isn't that slow
either.  At a high level, we first check if we can pop some non-empty
magazines off the allocation class's linked stack.  If both stacks are
empty, that's just a quick load and comparison with 0.  Otherwise, we
call `slitter__stack_pop` or `slitter__stack_try_pop`, straightforward
double-wide CAS jobs (we avoid ABA with a generation counter, and
don't have to worry about reclamation races because magazines are
immortal).  The plain `pop` looks like:

```
   0:   4c 8b 4f 08             mov    0x8(%rdi),%r9
   4:   4c 8b 07                mov    (%rdi),%r8
   7:   4d 85 c0                test   %r8,%r8
   a:   74 34                   je     40 <slitter__stack_pop+0x40>
   c:   53                      push   %rbx
   d:   4c 89 c0                mov    %r8,%rax
  10:   4c 89 ca                mov    %r9,%rdx
  13:   49 8d 49 01             lea    0x1(%r9),%rcx
  17:   49 8b 98 f0 00 00 00    mov    0xf0(%r8),%rbx
  1e:   f0 48 0f c7 0f          lock cmpxchg16b (%rdi)
  23:   49 39 d1                cmp    %rdx,%r9
  26:   75 20                   jne    48 <slitter__stack_pop+0x48>
  28:   49 c7 80 f0 00 00 00    movq   $0x0,0xf0(%r8)
  2f:   00 00 00 00 
  33:   b8 01 00 00 00          mov    $0x1,%eax
  38:   5b                      pop    %rbx
  39:   4c 89 06                mov    %r8,(%rsi)
  3c:   c3                      retq   
  3d:   0f 1f 00                nopl   (%rax)
  40:   31 c0                   xor    %eax,%eax
  42:   c3                      retq   
  43:   0f 1f 44 00 00          nopl   0x0(%rax,%rax,1)
  48:   49 89 c0                mov    %rax,%r8
  4b:   49 89 d1                mov    %rdx,%r9
  4e:   48 85 c0                test   %rax,%rax
  51:   75 c0                   jne    13 <slitter__stack_pop+0x13>
  53:   31 c0                   xor    %eax,%eax
  55:   5b                      pop    %rbx
  56:   c3                      retq   
```

If we found a non-empty magazine, we must push our currenty empty one
to a freelist for recycling.  That's another compare-and-swap.

If both stacks are empty, we must allocate more objects.  We implement
that in `press.rs` with a single atomic increment, which does not fail
under contention, but only when we notice (after the fact) that we
must find a new bump allocation region.

At a high level, we expect the slow path to incur one atomic increment
during bulk allocation phases, when no allocation is cached in
magazines.  When mixing allocation and deallocation (steady state),
this turns into two atomics, one CAS to acquire a new magazine of
cahed allocations, and another to recycle our empty magazine.

However, we will enter an even slower path whenever we exhaust the
current bump allocation reigon. When that happens (roughly once per
megabyte), we take a lock and grab another piece of address space.
Finally, we can also run out of pre-reserved address space, in which
case we must `mmap` to ask the kernel for more address space; we only
do that in 1 GB increments (and never release memory to the OS), so
that's a rare occasion.

Release
-------

Releasing allocations is special because it's a fundamentally
asynchronous operation: `slitter_release`, like `free` doesn't return
anything, so nothing can wait on it.  That's why we try to pack more
safety checks in the release code, as long as we can avoid
(unpredictable) control flow.

The code for `slitter_release` is

```
void
slitter_release(struct slitter_class class, void *ptr)
{
	uintptr_t address = (uintptr_t)ptr;
	uintptr_t chunk_base = address & -SLITTER__DATA_ALIGNMENT;
	uintptr_t chunk_offset = address % SLITTER__DATA_ALIGNMENT;
	size_t span_index = chunk_offset / SLITTER__SPAN_ALIGNMENT;
	uintptr_t meta_base = chunk_base -
	    (SLITTER__GUARD_PAGE_SIZE + SLITTER__METADATA_PAGE_SIZE);
	struct magazine *restrict mag;
	uint32_t id = class.id;

	if (ptr == NULL)
		return;

	/* Check the span metadata. */
	{
		const struct span_metadata *meta = (void *)meta_base;
		const struct span_metadata *span = &meta[span_index];

		assert(class.id == span->class_id && "class mismatch");
	}

	if (__builtin_expect(id >= slitter_cache.n, 0))
		return slitter__release_slow(class, ptr);

	mag = &slitter_cache.mags[id].release;
	if (__builtin_expect(slitter__magazine_is_exhausted(mag), 0))
		return slitter__release_slow(class, ptr);

	return slitter__magazine_put_non_full(mag, ptr);
}
```

All the shifting and masking above are there to help detect
mismatching releases.  The real work starts at
`if (__builtin_expect(id >= slitter_cache.n, 0))`.

Again, the majority of instructions aren't the deallocation itself:
that's just two range checks followed by a store and a stack index
update.

```
   0:   48 89 f2                mov    %rsi,%rdx
   3:   48 89 f0                mov    %rsi,%rax
   6:   48 81 e2 00 00 00 c0    and    $0xffffffffc0000000,%rdx
   d:   48 c1 e8 0e             shr    $0xe,%rax
  11:   48 81 ea 00 00 40 00    sub    $0x400000,%rdx
  18:   48 85 f6                test   %rsi,%rsi
  1b:   74 41                   je     5e <slitter_release+0x5e>  ; check for NULL
  1d:   0f b7 c0                movzwl %ax,%eax
  20:   48 8d 04 40             lea    (%rax,%rax,2),%rax
  24:   39 3c c2                cmp    %edi,(%rdx,%rax,8)
  27:   75 3c                   jne    65 <slitter_release+0x65>  ; Assert out on class mismatch
  29:   48 8b 15 00 00 00 00    mov    0x0(%rip),%rdx        # 30 <slitter_release+0x30>
                        2c: R_X86_64_GOTTPOFF   slitter_cache-0x4
  30:   89 f8                   mov    %edi,%eax
  32:   64 48 3b 02             cmp    %fs:(%rdx),%rax
  36:   73 28                   jae    60 <slitter_release+0x60>  ; Maybe (re-)initialise the cache
  38:   48 c1 e0 05             shl    $0x5,%rax
  3c:   64 48 03 42 08          add    %fs:0x8(%rdx),%rax
  41:   48 8b 50 10             mov    0x10(%rax),%rdx
  45:   48 85 d2                test   %rdx,%rdx                  ; Is the target magazine full?
  48:   74 16                   je     60 <slitter_release+0x60>
  4a:   48 8b 48 18             mov    0x18(%rax),%rcx
  4e:   48 8d 7a 01             lea    0x1(%rdx),%rdi
  52:   48 89 78 10             mov    %rdi,0x10(%rax)            ; Update the release index
  56:   48 89 b4 d1 f0 00 00    mov    %rsi,0xf0(%rcx,%rdx,8)     ; Store the freed object
  5d:   00 
  5e:   c3                      retq   
  5f:   90                      nop
  60:   e9 00 00 00 00          jmpq   65 <slitter_release+0x65>
                        61: R_X86_64_PLT32      slitter__release_slow-0x4
  65:   50                      push   %rax
  66:   48 8d 0d 00 00 00 00    lea    0x0(%rip),%rcx        # 6d <slitter_release+0x6d>
                        69: R_X86_64_PC32       .rodata.__PRETTY_FUNCTION__.2337-0x4
  6d:   ba 4e 00 00 00          mov    $0x4e,%edx
  72:   48 8d 35 00 00 00 00    lea    0x0(%rip),%rsi        # 79 <slitter_release+0x79>
                        75: R_X86_64_PC32       .LC0-0x4
  79:   48 8d 3d 00 00 00 00    lea    0x0(%rip),%rdi        # 80 <slitter_release+0x80>
                        7c: R_X86_64_PC32       .LC1-0x4
  80:   e8 00 00 00 00          callq  85 <slitter_release+0x85>
                        81: R_X86_64_PLT32      __assert_fail-0x4
```

Here as well, the first slow path branch is taken ~once per thread,
and the second once per 30-allocation magazine.

When a magazine is full, we must push it to the allocation class's
stack (`slitter__stack_push`).  That's another double-wide CAS loop.

After that, we must find an empty one.  We hope to find one from the
global cache of empty magazines (an atomic stack pop); otherwise, we
create a new one... by calling the system allocator for now.

In total, that's two atomics, or one atomic and a malloc... a bit
worse than allocation, but the application for which we wrote slitter
cares more about the performance of parallel allocation during startup
than anything else.
