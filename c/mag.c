#include "mag.h"

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
