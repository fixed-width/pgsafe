#!/usr/bin/env bash
set -euo pipefail
: "${WASI_SDK_PATH:?set WASI_SDK_PATH to your wasi-sdk install (e.g. \$HOME/wasi-sdk-33.0-x86_64-linux)}"

# The cc crate compiles libpg_query for the wasm target with wasi-sdk's clang.
export CC_wasm32_wasip1="$WASI_SDK_PATH/bin/clang"
export AR_wasm32_wasip1="$WASI_SDK_PATH/bin/llvm-ar"
# --sysroot gives the C code a real libc; -wasm-enable-sjlj lowers Postgres's
# setjmp/longjmp error handling to wasm exception-handling. wasi-libc gates a
# few POSIX bits (signals, mmap, getpid, process clocks) behind emulation that
# must be enabled at compile time with -D_WASI_EMULATED_* and linked below.
WASI_EMUL_DEFS="-D_WASI_EMULATED_SIGNAL -D_WASI_EMULATED_MMAN -D_WASI_EMULATED_PROCESS_CLOCKS -D_WASI_EMULATED_GETPID"
# Stub headers for the few POSIX headers wasi-libc lacks (netdb.h, pwd.h,
# grp.h, sys/wait.h, syslog.h). libpg_query includes them for types/constants
# only; the parser never uses them. -I before the sysroot so the stubs resolve.
SHIM_DIR="$(cd "$(dirname "$0")/wasi-shims" && pwd)"
# -include force-prepends the prelude (missing errno constants + sigsetjmp map).
SHIM_FLAGS="-I$SHIM_DIR -include $SHIM_DIR/prelude.h"
export CFLAGS_wasm32_wasip1="$SHIM_FLAGS --sysroot=$WASI_SDK_PATH/share/wasi-sysroot -mllvm -wasm-enable-sjlj $WASI_EMUL_DEFS"
# bindgen runs on the host but must emit wasm32 (32-bit pointer) bindings.
# Use wasi-sdk's own libclang (no host llvm needed), fed the wasm target + wasi
# sysroot + its builtin headers (for stddef.h). The C compile also uses wasi-sdk
# clang (above).
export LIBCLANG_PATH="$WASI_SDK_PATH/lib"
CLANG_BUILTIN_INC=$(set -- "$WASI_SDK_PATH"/lib/clang/*/include; echo "$1")
[ -d "$CLANG_BUILTIN_INC" ] || { echo "error: clang builtin include dir not found under $WASI_SDK_PATH/lib/clang" >&2; exit 1; }
# -fvisibility=default is REQUIRED: wasm32 defaults function visibility to
# hidden, and bindgen silently skips hidden-visibility functions — without this
# the bindings contain types/consts but ZERO functions, and pg_query won't link.
export BINDGEN_EXTRA_CLANG_ARGS="-I$SHIM_DIR -isystem $CLANG_BUILTIN_INC --sysroot=$WASI_SDK_PATH/share/wasi-sysroot --target=wasm32-wasip1 -fvisibility=default $WASI_EMUL_DEFS"
# The final link (rustc/rust-lld) must pull in the matching emulation archives,
# plus libsetjmp.a which provides the wasm SjLj runtime helpers
# (__wasm_setjmp/__wasm_setjmp_test/__wasm_longjmp/__c_longjmp) that
# -wasm-enable-sjlj emits. All live in the wasi-sysroot lib dir.
WASI_LIB_DIR="$WASI_SDK_PATH/share/wasi-sysroot/lib/wasm32-wasip1"
export RUSTFLAGS="${RUSTFLAGS:-} -L native=$WASI_LIB_DIR -C link-arg=-lsetjmp -C link-arg=-lwasi-emulated-signal -C link-arg=-lwasi-emulated-mman -C link-arg=-lwasi-emulated-process-clocks -C link-arg=-lwasi-emulated-getpid"

cd "$(dirname "$0")"
cargo build --target wasm32-wasip1 --release
echo "built: target/wasm32-wasip1/release/pgsafe-wasm.wasm"
