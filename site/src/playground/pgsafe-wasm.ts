import { WASI, WASIProcExit, OpenFile, File } from "@bjorn3/browser_wasi_shim";

export interface Finding {
  rule_id: string;
  severity: "error" | "warning";
  message: string;
  guidance: string;
  statement_index: number;
  location: { byte: number; line: number; column: number };
  snippet: string;
  /** Present when an inline `-- pgsafe:ignore <rule>` directive matched this
   *  finding: it's still reported but excluded from the gate. */
  suppression?: { reason: string };
}
export interface FileReport {
  file: string;
  findings: Finding[];
  error?: string;
}
export interface Envelope {
  schema_version: number;
  files: FileReport[];
  error?: string;
}

export type Lint = (sql: string, opts: { inTransaction: boolean }) => Envelope;

/** Fetch + compile the module once; return a synchronous `lint`. */
export async function loadLinter(url = "/pgsafe.wasm"): Promise<Lint> {
  const bytes = await (await fetch(url)).arrayBuffer();
  // Legacy-EH module — supported in current Chrome/Firefox/Safari (and Node V8).
  const wasmModule = await WebAssembly.compile(bytes);

  return (sql, opts) => {
    const request = JSON.stringify({ sql, inTransaction: opts.inTransaction });
    const stdin = new File(new TextEncoder().encode(request));
    const stdout = new File([]);
    const stderr = new File([]);
    const wasi = new WASI(
      [],
      [],
      [new OpenFile(stdin), new OpenFile(stdout), new OpenFile(stderr)],
    );
    const instance = new WebAssembly.Instance(wasmModule, {
      wasi_snapshot_preview1: wasi.wasiImport,
    });
    try {
      wasi.start(
        instance as unknown as {
          exports: { memory: WebAssembly.Memory; _start: () => unknown };
        },
      );
    } catch (e) {
      // A WASI command calls proc_exit on the way out; the shim throws that as
      // WASIProcExit even on a clean exit. Anything else is a real failure.
      if (!(e instanceof WASIProcExit)) throw e;
    }
    return JSON.parse(new TextDecoder().decode(stdout.data)) as Envelope;
  };
}
