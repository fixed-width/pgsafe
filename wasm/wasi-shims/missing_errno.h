/* Pulled in by the force-included prelude.h (which clang -include's), so it
 * lands before every translation unit.
 *
 * wasi-libc's <errno.h> omits a number of BSD/Linux errno constants that
 * Postgres's src/port/strerror.c (and a few others) reference in switch arms.
 * The parser never returns these errnos, so the values only need to exist for
 * the code to compile. Names here are disjoint from wasi-libc's errno set, so
 * there is no redefinition. Values follow Linux <asm-generic/errno.h>. */
#ifndef PGSAFE_WASI_SHIM_MISSING_ERRNO_H
#define PGSAFE_WASI_SHIM_MISSING_ERRNO_H

#ifndef ENOTBLK
#define ENOTBLK 15
#endif
#ifndef ECHRNG
#define ECHRNG 44
#endif
#ifndef EUSERS
#define EUSERS 87
#endif
#ifndef ESOCKTNOSUPPORT
#define ESOCKTNOSUPPORT 94
#endif
#ifndef EPFNOSUPPORT
#define EPFNOSUPPORT 96
#endif
#ifndef ESHUTDOWN
#define ESHUTDOWN 108
#endif
#ifndef ETOOMANYREFS
#define ETOOMANYREFS 109
#endif
#ifndef EHOSTDOWN
#define EHOSTDOWN 112
#endif
#ifndef ESTALE
#define ESTALE 116
#endif
#ifndef EREMOTE
#define EREMOTE 66
#endif
#ifndef EDQUOT
#define EDQUOT 122
#endif

#endif /* PGSAFE_WASI_SHIM_MISSING_ERRNO_H */
