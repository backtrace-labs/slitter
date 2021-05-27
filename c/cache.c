#include "cache.h"

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
	uint32_t id = class.id;

	if (__builtin_expect(id >= slitter_cache.n, 0))
		return slitter__allocate_slow(class);

	mag = &slitter_cache.mags[id].alloc;
	if (__builtin_expect(slitter__magazine_is_exhausted(mag), 0))
		return slitter__allocate_slow(class);

	return slitter__magazine_get_non_empty(mag);
}

void
slitter_release(struct slitter_class class, void *ptr)
{
	struct magazine *restrict mag;
	uint32_t id = class.id;

	if (ptr == NULL)
		return;

	if (__builtin_expect(id >= slitter_cache.n, 0))
		return slitter__release_slow(class, ptr);

	mag = &slitter_cache.mags[id].release;
	if (__builtin_expect(slitter__magazine_is_exhausted(mag), 0))
		return slitter__release_slow(class, ptr);

	return slitter__magazine_put_non_full(mag, ptr);
}
