#pragma once

#include <stddef.h>
#include <stdint.h>

/**
 * Must match `SpanMetadata` in `mill.rs`.
 */
struct span_metadata {
	uint32_t class_id;
	uint32_t bump_limit;
	uintptr_t bump_ptr;
	uintptr_t span_begin;
};

/**
 * Returns the size of `struct span_metadata` in C.
 */
size_t slitter__span_metadata_size(void);
