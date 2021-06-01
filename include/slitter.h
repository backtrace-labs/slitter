#pragma once

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

/**
 * Each slitter object class is uniquely identified by a non-zero
 * 32-bit integer.
 *
 * Passing a zero-initialised `slitter_class` to `slitter_allocate`
 * or `slitter_release` causes undefined behaviour.
 */
struct slitter_class {
	uint32_t id;
};

struct slitter_class_config {
	/*
	 * The name of the object class. Nullable. 
	 *
	 * Must point to a NUL-terminated string of utf-8 bytes if non-NULL.
	 */
	const char *name;

	/*
	 * The size of each object in the allocation class.  Allocations
	 * are only guaranteed alignment to 8 bytes.
	 */
	size_t size;

	/*
	 * If true, zero-fill recycled allocations.
	 */
	bool zero_init;

        /*
         * The name of the underlying mapper, or NULL for default.
         *
         * A mapper name of "file" will use the file-backed mapper.
         */
        const char *mapper_name;
};

#define DEFINE_SLITTER_CLASS(NAME, ...)					\
	struct slitter_class NAME;					\
									\
	__attribute__((__constructor__))				\
	static void slitter_register_##NAME##_fn(void)			\
	{								\
									\
		NAME = slitter_class_register(				\
		    &(struct slitter_class_config) { __VA_ARGS__ });	\
		return;							\
	}


/**
 * Registers a new allocation class, or dies trying.
 *
 * The config must be a valid pointer.  On error, this function will abort.
 */
struct slitter_class slitter_class_register(const struct slitter_class_config *);

/**
 * Returns a new allocation for the object class.
 *
 * On error, this function will abort.
 *
 * Behaviour is undefined if the `slitter_class` argument is
 * zero-filled or was otherwise not returned by
 * `slitter_class_register`.
 */
void *slitter_allocate(struct slitter_class);

/**
 * Passes ownership of `ptr` back to the object class.
 *
 * `ptr` must be NULL, or have been returned by a call to
 * `slitter_alloc`.
 *
 * On error, this function will abort.
 *
 * Behaviour is undefined if the `slitter_class` argument is
 * zero-filled or was otherwise not returned by
 * `slitter_class_register`.
 */
void slitter_release(struct slitter_class, void *ptr);
