#include "cache.h"

#include <assert.h>

#include "constants.h"
#include "span_metadata.h"

struct thread_cache {
	size_t n;
	struct cache_magazines *mags;
};

static __thread struct thread_cache slitter_cache __attribute__((tls_model("initial-exec")));

/**
 * Defined in cache.rs
 */
extern void *slitter__allocate_slow(struct slitter_class);
extern void slitter__release_slow(struct slitter_class, void *);

void
slitter__cache_register(struct cache_magazines *mags, size_t n)
{
	slitter_cache = (struct thread_cache) {
		.n = n,
		.mags = mags,
	};

	return;
}

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
