#pragma once
#include "slitter.h"

#include <stddef.h>

#include "mag.h"

/*
 * Matches the `Magazines` struct in `cache.rs`.
 */
struct cache_magazines {
	struct magazine alloc;
	struct magazine release;
};

/**
 * Returns the thread's pre-allocated array of `cache_magazines`.
 *
 * That array lives next to the fast-path's internal thread-local data
 * structure, so using that array improves locality.
 */
struct cache_magazines *slitter__cache_borrow(size_t *OUT_n);

/**
 * Registers an array of `n` `cache_magazines` for this thread.
 *
 * That array may be the same one returned by `slitter__cache_borrow`.
 */
void slitter__cache_register(struct cache_magazines *, size_t n);
