#include "cache.h"

struct thread_cache {
	size_t n;
	struct cache_magazines *mags;
};

static __thread struct thread_cache slitter_cache __attribute__((tls_model("initial-exec")));

void
slitter__cache_register(struct cache_magazines *mags, size_t n)
{
	slitter_cache = (struct thread_cache) {
		.n = n,
		.mags = mags,
	};

	return;
}
