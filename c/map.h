#pragma once
/**
 * Internal low-level memory mapping code for Slitter.  We use C
 * instead of relying on (unstable) `libc`.
 *
 * The corresponding Rust definition live in `src/map.rs`.
 */

#include <stddef.h>
#include <stdint.h>

/**
 * Returns the system page size, or `-errno` on failure.
 */
int64_t slitter__page_size(void);

/**
 * Attempts to reserve a region of address space of `desired_size`
 * bytes.
 *
 * On success, returns the address of the first byte in the
 * new region and overwrites `OUT_errno` with 0.
 *
 * On failure, returns NULL and overwrites `OUT_errno` with the
 * `errno` from `mmap`.
 */
void *slitter__reserve_region(size_t desired_size,
    int32_t *OUT_errno);

/**
 * Attempts to release the region of address space starting at `base`,
 * and continuing for `size` bytes.
 *
 * Returns 0 on success, and `-errno` on failure.
 */
int32_t slitter__release_region(void *base, size_t size);

/**
 * Attempts to back the region of address space starting at `base`
 * and continuing for `size` bytes with actual memory.  The caller
 * must have first acquired ownership of the address space with
 * `slitter__reserve_region`.
 *
 * The region will be safe for read and writes, but may be
 * demand-faulted later.
 *
 * Returns 0 on success, and `-errno` on failure.
 */
int32_t slitter__allocate_region(void *base, size_t size);

/**
 * Attempts to back the region of address space starting at `base` and
 * continuing for `size` bytes with memory from `fd`, starting at
 * `offset`.  The caller must have first acquired ownership of the
 * address space with `slitter__reserve_region`.
 *
 * The region will be safe for read and writes, but may be
 * demand-faulted later.
 *
 * Returns 0 on success, and `-errno` on failure.
 */
int32_t slitter__allocate_fd_region(int fd, size_t offset,
    void *base, size_t size);
