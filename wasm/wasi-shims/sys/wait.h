/* Minimal stub of <sys/wait.h> for wasm32-wasip1 — see ../netdb.h for
 * rationale. wasm has no processes; libpg_query never waits on one. */
#ifndef PGSAFE_WASI_SHIM_SYS_WAIT_H
#define PGSAFE_WASI_SHIM_SYS_WAIT_H

#include <sys/types.h> /* pid_t */

#define WNOHANG 1
#define WUNTRACED 2

#define WIFEXITED(s) (((s) & 0x7f) == 0)
#define WEXITSTATUS(s) (((s) >> 8) & 0xff)
#define WIFSIGNALED(s) (((signed char)(((s) & 0x7f) + 1) >> 1) > 0)
#define WTERMSIG(s) ((s) & 0x7f)
#define WIFSTOPPED(s) (((s) & 0xff) == 0x7f)
#define WSTOPSIG(s) WEXITSTATUS(s)

pid_t wait(int *);
pid_t waitpid(pid_t, int *, int);

#endif /* PGSAFE_WASI_SHIM_SYS_WAIT_H */
