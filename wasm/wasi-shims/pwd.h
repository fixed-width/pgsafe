/* Minimal stub of <pwd.h> for wasm32-wasip1 — see netdb.h for rationale.
 * libpg_query includes it transitively for struct passwd; the parser never
 * looks up users. */
#ifndef PGSAFE_WASI_SHIM_PWD_H
#define PGSAFE_WASI_SHIM_PWD_H

#include <sys/types.h> /* uid_t, gid_t, size_t */

struct passwd {
    char *pw_name;
    char *pw_passwd;
    uid_t pw_uid;
    gid_t pw_gid;
    char *pw_gecos;
    char *pw_dir;
    char *pw_shell;
};

struct passwd *getpwuid(uid_t);
struct passwd *getpwnam(const char *);
int getpwuid_r(uid_t, struct passwd *, char *, size_t, struct passwd **);

#endif /* PGSAFE_WASI_SHIM_PWD_H */
