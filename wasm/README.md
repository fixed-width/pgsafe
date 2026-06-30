# pgsafe-wasm — WASM spike (Milestone 1)

**Status: ✅ SUCCEEDED.** The full pgsafe analyzer — including `libpg_query`
(PostgreSQL's real C parser) — compiles to `wasm32-wasip1` and lints correctly
in a browser-class WASM engine (V8 / Node, and therefore browsers).

It is a WASI *command*: stdin is a `{"sql": "...", "inTransaction": false}` JSON
request, stdout is the same JSON envelope as `pgsafe --format json`. The site
loads this module in the browser via a WASI shim (`site/src/playground/`).

For the site, `build-web.sh` builds the release wasm and copies it to
`site/public/pgsafe.wasm` (gitignored; rebuilt by `npm run build:wasm` and by CI):

```sh
WASI_SDK_PATH="$HOME/wasi-sdk-33.0-x86_64-linux" ./build-web.sh
```

## Result

- Artifact: `target/wasm32-wasip1/release/pgsafe-wasm.wasm` (~4.1 MB, in line
  with the JS-ecosystem `libpg-query` WASM build).
- Verified end-to-end under **Node's V8** (`run-node.mjs`): unsafe SQL →
  expected `rule_id`s, safe SQL → empty findings, invalid SQL → the real
  Postgres parse-error message (exercises the `sigsetjmp`/`longjmp` error path
  at runtime).

## Toolchain

- **wasi-sdk 33** (clang 22) — compiles the `libpg_query` C for wasm. Set
  `WASI_SDK_PATH` to the install dir.
- **Rust target** `wasm32-wasip1` (`rustup target add wasm32-wasip1`).
- bindgen runs on **wasi-sdk's own libclang** (no host LLVM needed). See the
  bindgen note below.
- **Node ≥ 20** (V8) to run/validate — *not* wasmtime (see runtime note).

## Build

```sh
WASI_SDK_PATH="$HOME/wasi-sdk-33.0-x86_64-linux" ./spike.sh
```

`spike.sh` is the single source of truth for the flags. The non-obvious bits it
encodes, each discovered against a concrete failure:

1. **C compiler** = wasi-sdk clang via `CC_wasm32_wasip1` / `CFLAGS_wasm32_wasip1`.
2. **setjmp/longjmp** — Postgres's error handling needs it; lowered to wasm SjLj
   with `-mllvm -wasm-enable-sjlj`. The runtime helpers
   (`__wasm_setjmp`/`__wasm_longjmp`/`__c_longjmp`) come from **`-lsetjmp`**
   (wasi-sysroot's `libsetjmp.a`).
3. **POSIX emulation** — wasi-libc gates signals/mman/getpid/process-clocks
   behind `-D_WASI_EMULATED_*`; the matching `-lwasi-emulated-*` archives are
   linked.
4. **Missing headers** — wasi-libc has no `netdb.h`, `pwd.h`, `grp.h`,
   `sys/wait.h`, `syslog.h`. `libpg_query` includes them for types only (the
   parser never uses them), so minimal stubs in `wasi-shims/` satisfy the
   compile.
5. **`wasi-shims/prelude.h`** (force-included via `-include`): defines the BSD
   errno constants wasi-libc omits, and maps `sigsetjmp`/`siglongjmp` →
   `setjmp`/`longjmp` (wasm has no signal masks).
6. **bindgen `-fvisibility=default` — essential.** wasm32 defaults function
   visibility to *hidden*, and bindgen silently drops hidden-visibility
   functions: without this flag the generated bindings contain types/consts but
   **zero functions**, and `pg_query` fails to link. With this flag,
   wasi-sdk's own libclang (`LIBCLANG_PATH=$WASI_SDK_PATH/lib`) generates the
   functions fine — so there is no host-LLVM dependency.

## Runtime note — legacy exception handling

LLVM's `-wasm-enable-sjlj` emits the **legacy** wasm exception-handling
encoding. Consequences:

- **Browsers (V8/SpiderMonkey) and Node run it fine** — legacy EH has shipped in
  Chrome/Firefox/Safari. This is the playground target, so we're good.
- **wasmtime 46 does *not*** run it (`-W exceptions=y` only enables the new
  exnref proposal; there is no legacy flag). So validate with Node/V8
  (`run-node.mjs`), not wasmtime. Milestone 2 should confirm the new-EH path if
  we ever want a non-browser host.

## Files

- `spike.sh` — build recipe (all env/flags).
- `build-web.sh` — builds the release wasm and copies it to `site/public/pgsafe.wasm`.
- `wasi-shims/` — stub headers + force-included `prelude.h` for wasi-libc gaps.
- `run-node.mjs` — V8/WASI harness (feeds a JSON request on stdin).
- `src/` — the WASI command (`lint_json()` lints a `{sql,inTransaction}` request;
  `main` does stdin→stdout).
