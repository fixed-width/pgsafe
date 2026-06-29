/* Minimal stub of <grp.h> for wasm32-wasip1 — see netdb.h for rationale. */
#ifndef PGSAFE_WASI_SHIM_GRP_H
#define PGSAFE_WASI_SHIM_GRP_H

#include <sys/types.h> /* gid_t */

struct group {
    char *gr_name;
    char *gr_passwd;
    gid_t gr_gid;
    char **gr_mem;
};

struct group *getgrgid(gid_t);
struct group *getgrnam(const char *);

#endif /* PGSAFE_WASI_SHIM_GRP_H */
