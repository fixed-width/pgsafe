// Run the wasip1 pgsafe-wasm command under Node's V8 (browser-representative).
// Reads a {"sql":"...","inTransaction":false} JSON request on stdin, prints the
// pgsafe JSON envelope on stdout. e.g.:
//   echo '{"sql":"CREATE INDEX i ON t (c);"}' | node wasm/run-node.mjs <wasm>
import { WASI } from 'node:wasi';
import { readFileSync } from 'node:fs';
import { argv } from 'node:process';

const wasmPath = argv[2];
const wasi = new WASI({
  version: 'preview1',
  args: ['pgsafe-wasm'],
  env: {},
  // inherit the process's stdio fds (0/1/2)
  stdin: 0, stdout: 1, stderr: 2,
});
const bytes = readFileSync(wasmPath);
const module = await WebAssembly.compile(bytes);
const instance = await WebAssembly.instantiate(module, wasi.getImportObject());
wasi.start(instance);
