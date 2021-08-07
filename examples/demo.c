#define RUN_ME /*
set -e

CURRENT="$PWD"
SELF=$(readlink -f "$0")
EXEC=$(basename "$SELF" .c)
BASE="$(dirname "$SELF")/../"

(cd "$BASE"; cargo build  --release --target-dir "$CURRENT/target")

echo "Build with CFLAGS=-DMISMATCH for a crash"

cc $CFLAGS -I"$BASE/include"  "$SELF" "$CURRENT/target/release/libslitter.a" -lpthread -ldl -o "$EXEC"

exec "./$EXEC"
*/
#include <assert.h>
#include <slitter.h>
#include <stdio.h>

struct base {
        int x;
};

struct derived {
        struct base base;
        int y;
};

DEFINE_SLITTER_CLASS(base_tag,
    .name = "base",
    .size = sizeof(struct base),
    .zero_init = true);

DEFINE_SLITTER_CLASS(derived_tag,
    .name = "derived",
    .size = sizeof(struct derived),
    .zero_init = true);

int
main()
{
        struct base *base;
        struct derived *derived;

        /* Release is NULL-safe. */
        slitter_release(base_tag, NULL);

        /* Allocate from our two class tags. */
        base = slitter_allocate(base_tag);
        derived = slitter_allocate(derived_tag);

        /* We asked for zero-initialisation. */
        assert(base->x == 0);
        assert(derived->base.x == 0 && derived->y == 0);

        base->x = 1;
        derived->y = 2;

        /* Release the two objects. */
        slitter_release(base_tag, base);
        slitter_release(derived_tag, derived);

        /* Allocate again, they're still zero-filled. */
        base = slitter_allocate(base_tag);
        derived = slitter_allocate(derived_tag);
        assert(base->x == 0);
        assert(derived->base.x == 0 && derived->y == 0);

        slitter_release(base_tag, base);
        slitter_release(derived_tag, derived);

#ifdef MISMATCH
        /* Allocate from the "derived" tag. */
        derived = slitter_allocate(derived_tag);

        /*
         * Free its "base" member.  This will crash with
         * something like
         * `demo: c/cache.c:94: slitter_release: Assertion `class.id == span->class_id && "class mismatch"' failed.`
         */
        slitter_release(base_tag, &derived->base);
#endif

        printf("exiting demo.c\n");
        return 0;
}
