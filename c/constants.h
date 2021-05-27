#pragma once

#include <stddef.h>

/*
 * Define default constants
 */
#ifndef SLITTER__SMALL_CONSTANTS
# define SLITTER__MAGAZINE_SIZE 30
# define SLITTER__DATA_ALIGNMENT (1UL << 30)
# define SLITTER__GUARD_PAGE_SIZE (2UL << 20)
# define SLITTER__METADATA_PAGE_SIZE (2UL << 20)
# define SLITTER__SPAN_ALIGNMENT (16UL << 10)
#else
# define SLITTER__MAGAZINE_SIZE 6
# define SLITTER__DATA_ALIGNMENT (2UL << 20)
# define SLITTER__GUARD_PAGE_SIZE (16UL << 10)
# define SLITTER__METADATA_PAGE_SIZE (16UL << 10)
# define SLITTER__SPAN_ALIGNMENT (4UL << 10)
#endif

/**
 * Returns the value of the `SLITTER__MAGAZINE_SIZE` constant on the C
 * side.
 *
 * The Rust code uses this function to confirm that the constant has
 * the same value on both sides.
 */
size_t slitter__magazine_size(void);

/**
 * Returns the value of the `SLITTER__DATA_ALIGNMENT` constant on the
 * C side.
 */
size_t slitter__data_alignment(void);

/**
 * Returns the value of the `SLITTER__GUARD_PAGE_SIZE` constant on the
 * C side.
 */
size_t slitter__guard_page_size(void);

/**
 * Returns the value of the `SLITTER__METADATA_PAGE_SIZE` constant on
 * the C side.
 */
size_t slitter__metadata_page_size(void);

/**
 * Returns the value of the `SLITTER__SPAN_ALIGNMENT` constant on the
 * C side.
 */
size_t slitter__span_alignment(void);
