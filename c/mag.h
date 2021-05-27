#pragma once

#include <stddef.h>
#include <stdint.h>
#include <sys/types.h>

#ifndef SLITTER__MAGAZINE_SIZE
/* Must match the default in magazine_impl.rs */
#define SLITTER__MAGAZINE_SIZE 30
#endif

/**
 * Matches `MagazineStorage` on the Rust side.
 */
struct magazine_storage {
	uint32_t num_allocated_slow;
	void *allocations[SLITTER__MAGAZINE_SIZE];
        /*
         * The `link` pointer is only used by the C side.
         * It's always NULL (None) on the Rust side.
         */
	struct magazine_storage *volatile link;
};

/**
 * Matches `MagazineImpl` on the Rust side.
 *
 * `top_of_stack` goes from MAGAZINE_SIZE to 0 when popping,
 * and from -MAGAZINE_SIZE to 0 when pushing.  In both cases,
 * `storage->allocations` is populated with cached objects
 * at low indices, and empty / garbage at high ones.
 */
struct magazine {
	ssize_t top_of_stack;
	struct magazine_storage *storage;
};

/**
 * Returns the value of the `MAGAZINE_SIZE` constant on the C side.
 *
 * The Rust code uses this function to confirm that the constant has
 * the same value on both sides.
 */
size_t slitter__magazine_capacity(void);

/**
 * Returns `sizeof(struct magazine_storage)`.
 */
size_t slitter__magazine_storage_sizeof(void);

/**
 * Returns `sizeof(struct magazine)`.
 */
size_t slitter__magazine_sizeof(void);
