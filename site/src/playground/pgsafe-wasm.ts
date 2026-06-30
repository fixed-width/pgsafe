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
  /** Present when the finding has a safe, machine-applicable fix (mirrors the
   *  Rust `Fix`): edits are absolute UTF-8 byte offsets into the submitted SQL. */
  fix?: { title: string; edits: { start: number; end: number; replacement: string }[] };
}
export interface FileReport {
  file: string;
  findings: Finding[];
  error?: string;
}
/** The wasm output is one of two disjoint shapes: a success envelope, or the
 *  shim's `{error}` object (bad request / render failure). */
export type Envelope =
  | { schema_version: number; files: FileReport[] }
  | { error: string };

export type Lint = (sql: string, opts: { inTransaction: boolean }) => Envelope;

/** Fetch + compile the module once; return a synchronous `lint`. */
export async function loadLinter(url = "/pgsafe.wasm"): Promise<Lint> {
  const res = await fetch(url);
  // fetch doesn't reject on 404/500 — guard so a missing wasm doesn't surface
  // as a confusing WebAssembly CompileError on the error page's HTML.
  if (!res.ok) throw new Error(`failed to fetch ${url}: ${res.status} ${res.statusText}`);
  const bytes = await res.arrayBuffer();
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
      if (e.code !== 0) {
        const err = new TextDecoder().decode(stderr.data).trim();
        throw new Error(`linter exited with code ${e.code}${err ? `: ${err}` : ""}`);
      }
    }
    const out = new TextDecoder().decode(stdout.data);
    if (!out) throw new Error("linter produced no output");
    return JSON.parse(out) as Envelope;
  };
}
