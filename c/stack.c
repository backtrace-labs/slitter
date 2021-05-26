#include "stack.h"

#include <assert.h>

static_assert(sizeof(struct stack) == 2 * sizeof(void *),
    "A stack must consist of exactly two pointers.");

#define LOAD_ACQUIRE(X) __atomic_load_n(&(X), __ATOMIC_ACQUIRE)
#define STORE_RELEASE(X, V) __atomic_store_n(&(X), (V), __ATOMIC_RELEASE)

void
slitter__stack_push(struct stack *stack, struct magazine_storage *mag)
{
	struct stack curr, next;

	/*
	 * Make sure to load `generation` first: it's our monotonic
	 * counter, so, if the CAS claims the `generation` hasn't
	 * changed since the read of `top_of_stack`, we have a
	 * consistent snapshot.
	 *
	 * These could be relaxed (and thus in any program order) if
	 * we could easily access the CAS's success flag and the old
	 * value on failure.  Unfortunately, GCC always falls back to
	 * libatomic for C11 u128 operations, so we have to make do
	 * with the legacy.
	 *
	 * These acquires don't actually pair with any release (they
	 * could pair with successful CASes).
	 */
	curr.generation = LOAD_ACQUIRE(stack->generation);
	curr.top_of_stack = LOAD_ACQUIRE(stack->top_of_stack);

	for (;;) {
		struct stack actual;

		/* The CAS should be the release barrier. */
		STORE_RELEASE(mag->link, curr.top_of_stack);
		next = (struct stack) {
			.top_of_stack = mag,
			.generation = curr.generation + 1,
		};

		/*
		 * GCC only obeys -mcx16 for the legacy "sync" atomics:
		 * the C11 standard operations still rely on libatomic
		 * in order to offer atomic loads...
		 */
		actual.bits = __sync_val_compare_and_swap(&stack->bits,
		    curr.bits, next.bits);
		/*
		 * If the generation matches, the CAS succeeded:
		 * tearing only happens in the first iteration, and
		 * that comes from a sequence that loads generation
		 * before top_of_stack.	 Subsequent iteration use the
		 * atomic snapshot provided by the CAS.
		 *
		 * This sad workaround for the lack of a `__sync`
		 * operation that returns both the success flag and
		 * the actual value on failure *only works because
		 * all operations increment the generation counter*.
		 *
		 * In theory, it would be safe for `push` to perform a
		 * regular CAS, without changing the generation
		 * counter.  However, the difference is marginal (an
		 * atomic is an atomic), correctness slightly more
		 * involved, and we'd have to compare both words
		 * when popping... and popping is on the allocation
		 * path, which is marginally more latency sensitive
		 * than the release path.
		 */
		if (__builtin_expect(actual.generation == curr.generation, 1))
			return;

		curr = actual;
	}

	return;
}

bool
slitter__stack_pop(struct stack *stack, struct magazine_storage **out)
{
	struct stack curr, next;

	curr.generation = LOAD_ACQUIRE(stack->generation);
	curr.top_of_stack = LOAD_ACQUIRE(stack->top_of_stack);
	if (curr.top_of_stack == NULL)
		return false;

	for (;;) {
		struct stack actual;
		struct magazine_storage *tos = curr.top_of_stack;

		if (__builtin_expect(tos == NULL, 0))
			return false;

		/*
		 * The ordering semantics of
		 * `__sync_val_compare_and_swap` aren't super clear.
		 * Insert an explicit acquire (load-load) fence here,
		 * before deferencing `curr.top_of_stack`, which may
		 * come from the CAS.  The CAS should be acquire on
		 * failure.
		 */
		__atomic_thread_fence(__ATOMIC_ACQUIRE);
		next = (struct stack) {
			.top_of_stack = LOAD_ACQUIRE(tos->link),
			.generation = curr.generation + 1,
		};

		actual.bits = __sync_val_compare_and_swap(&stack->bits,
		    curr.bits, next.bits);
		if (__builtin_expect(actual.generation == curr.generation, 1)) {
			tos->link = NULL;
			*out = tos;
			return true;
		}

		curr = actual;
	}

	return false;
}

bool
slitter__stack_try_pop(struct stack *stack, struct magazine_storage **out)
{
	struct stack actual, curr, next;
	struct magazine_storage *tos;

	curr.generation = LOAD_ACQUIRE(stack->generation);
	curr.top_of_stack = LOAD_ACQUIRE(stack->top_of_stack);

	tos = curr.top_of_stack;
	if (tos == NULL)
		return false;

	next = (struct stack) {
		.top_of_stack = tos->link,
		.generation = curr.generation + 1,
	};

	actual.bits = __sync_val_compare_and_swap(&stack->bits,
	    curr.bits, next.bits);
	if (__builtin_expect(actual.generation == curr.generation, 1)) {
		tos->link = NULL;
		*out = tos;
		return true;
	}

	return false;
}
