#include "mag.h"

extern bool slitter__magazine_is_exhausted(const struct magazine *);
extern void *slitter__magazine_get_non_empty(struct magazine *);
extern void slitter__magazine_put_non_full(struct magazine *, void *);

void *
slitter__magazine_get(struct magazine *restrict mag)
{

	if (slitter__magazine_is_exhausted(mag))
		return NULL;

	return slitter__magazine_get_non_empty(mag);
}

void *
slitter__magazine_put(struct magazine *restrict mag, void *alloc)
{

	if (slitter__magazine_is_exhausted(mag))
		return alloc;

	slitter__magazine_put_non_full(mag, alloc);
	return NULL;
}

size_t
slitter__magazine_capacity(void)
{

	return SLITTER__MAGAZINE_SIZE;
}

size_t
slitter__magazine_storage_sizeof(void)
{

	return sizeof(struct magazine_storage);
}

size_t
slitter__magazine_sizeof(void)
{

	return sizeof(struct magazine);
}
