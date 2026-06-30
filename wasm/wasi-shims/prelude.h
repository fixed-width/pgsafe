/* Force-included (clang -include) before every libpg_query translation unit.
 * Bundles the small set of wasi-libc gaps Postgres's C trips over. */
#ifndef PGSAFE_WASI_PRELUDE_H
#define PGSAFE_WASI_PRELUDE_H

/* BSD/Linux errno constants wasi-libc omits (referenced in strerror switches). */
#include "missing_errno.h"

/* wasi-libc has no sigsetjmp/siglongjmp. Postgres's PG_TRY/PG_CATCH uses them
 * for error unwinding; wasm has no signal masks, so map them to plain
 * setjmp/longjmp, which `-mllvm -wasm-enable-sjlj` lowers to wasm SjLj. */
#include <setjmp.h>
#ifndef sigsetjmp
#define sigsetjmp(env, savesigs) setjmp(env)
#endif
#ifndef siglongjmp
#define siglongjmp(env, val) longjmp(env, val)
#endif

#endif /* PGSAFE_WASI_PRELUDE_H */
