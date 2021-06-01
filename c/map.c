#include "map.h"

#include <assert.h>
#include <errno.h>
#include <sys/mman.h>
#include <unistd.h>

static_assert(sizeof(size_t) == sizeof(uintptr_t),
    "Our rust code assumes usize == size_t, but rust-the-language "
    "only guarantees usize == uintptr_t.");

int64_t
slitter__page_size(void)
{
	long ret;

	ret = sysconf(_SC_PAGESIZE);
	if (ret < 0)
		return -errno;

	return ret;
}

void *
slitter__reserve_region(size_t desired_size, int32_t *OUT_errno)
{
	void *ret;

	*OUT_errno = 0;
	ret = mmap(NULL, desired_size, PROT_NONE,
	    MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
	if (ret != MAP_FAILED) {
		assert(ret != NULL && "We assume NULL is not a valid address");
		return ret;
	}

	*OUT_errno = errno;
	return NULL;
}

int32_t
slitter__release_region(void *base, size_t size)
{

	if (size == 0 || munmap(base, size) == 0)
		return 0;

	return -errno;
}

int32_t
slitter__allocate_region(void *base, size_t size)
{
	void *ret;

	ret = mmap(base, size, PROT_READ | PROT_WRITE,
	    MAP_FIXED | MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
	if (ret != MAP_FAILED)
		return 0;

	return -errno;
}

int32_t
slitter__allocate_fd_region(int fd, size_t offset, void *base, size_t size)
{
	void *ret;

	ret = mmap(base, size, PROT_READ | PROT_WRITE,
            MAP_FIXED | MAP_SHARED, fd, (off_t)offset);
	if (ret != MAP_FAILED)
		return 0;

	return -errno;
}
