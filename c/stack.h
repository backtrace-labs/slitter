#pragma once

#include <stdbool.h>
#include <stdint.h>

#include "mag.h"

/**
 * A `stack` matches the `MagazineStack` in Rust.
 *
 * The lock-free implementation is a straightforward double-wide-CAS
 * logic, with a generation counter for ABA protection: we don't have
 * to worry about safe memory reclamation because `struct
 * magazine_storage` are immortal.
 */
struct __attribute__((__aligned__(16))) stack {
	union {
		struct {
			struct magazine_storage *top_of_stack;
			uintptr_t generation;
		};
		__uint128_t bits;
	};
};

/**
 * Pushes a new magazine to the stack.
 */
void slitter__stack_push(struct stack *, struct magazine_storage *);

/**
 * Attempts to pop one element from the stack.
 *
 * On success, returns true and populates the `out` pointer.
 * On failure, returns false.
 */
bool slitter__stack_pop(struct stack *, struct magazine_storage **out);

/**
 * Quickly attempts to pop one element from the stack.
 *
 * Once success, returns true and populates the `out` pointer.
 * On failure, returns false.
 *
 * Unlike `slitter__stack_pop`, this function may fail for any reason.
 */
bool slitter__stack_try_pop(struct stack *, struct magazine_storage **out);
