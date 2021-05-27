#pragma once

#include <stdbool.h>
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
 * `top_of_stack` goes from SLITTER__MAGAZINE_SIZE to 0 when popping,
 * and from -SLITTER__MAGAZINE_SIZE to 0 when pushing.  In both cases,
 * `storage->allocations` is populated with cached objects
 * at low indices, and empty / garbage at high ones.
 */
struct magazine {
	ssize_t top_of_stack;
	struct magazine_storage *storage;
};

/**
 * Returns whether the magazine is exhausted (empty for push
 * magazines, full for pop ones).
 */
inline bool
slitter__magazine_is_exhausted(const struct magazine *restrict mag)
{

	return mag->top_of_stack == 0;
}

/**
 * Consumes one cached allocation from a non-exhausted "Pop" magazine.
 */
inline void *
slitter__magazine_get_non_empty(struct magazine *restrict mag)
{
	void *ret;

	ret = mag->storage->allocations[--mag->top_of_stack];
	if (ret == NULL)
		__builtin_unreachable();

	return ret;
}

/**
 * Pushes one cached allocation to a non-exhausted "Push" magazine.
 */
inline void
slitter__magazine_put_non_full(struct magazine *restrict mag, void *alloc)
{

	mag->storage->allocations
	    [SLITTER__MAGAZINE_SIZE + mag->top_of_stack++] = alloc;
	return;
}

/**
 * Attempts to consume one cached allocation from a "Pop" magazine.
 *
 * Returns the cached allocation on success, NULL on failure.
 */
void *slitter__magazine_get(struct magazine *restrict mag);

/**
 * Attempts to push one allocation to a "Push" magazine.
 *
 * Returns NULL on success, `alloc` on failure.
 */
void *slitter__magazine_put(struct magazine *restrict mag, void *alloc);

/**
 * Returns the value of the `SLITTER__MAGAZINE_SIZE` constant on the C side.
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
