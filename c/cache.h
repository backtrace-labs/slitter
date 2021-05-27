#pragma once

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
 * Registers an array of `n` `cache_magazines` for this thread.
 */
void slitter__cache_register(struct cache_magazines *, size_t n);
