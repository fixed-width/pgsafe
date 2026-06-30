import { EditorView, basicSetup } from "codemirror";
import { sql, PostgreSQL } from "@codemirror/lang-sql";
import { oneDark } from "@codemirror/theme-one-dark";
import { loadLinter, type Envelope, type Finding, type Lint } from "./pgsafe-wasm";
import { EXAMPLES } from "./examples";
import { readHash, writeHash } from "./permalink";

const byId = <T extends HTMLElement>(id: string): T =>
  document.getElementById(id) as T;

const statusEl = byId("status");
const resultsEl = byId<HTMLElement>("results");
const intx = byId<HTMLInputElement>("opt-intx");
const failon = byId<HTMLSelectElement>("opt-failon");

const initial = readHash();
const startDoc =
  initial?.sql ??
  "ALTER TABLE users ADD COLUMN email text;\nCREATE INDEX idx_users_email ON users (email);";
if (initial) intx.checked = !!initial.inTransaction;

const view = new EditorView({
  doc: startDoc,
  extensions: [
    basicSetup,
    sql({ dialect: PostgreSQL }),
    oneDark,
    EditorView.updateListener.of((u) => {
      if (u.docChanged) schedule();
    }),
  ],
  parent: byId("editor"),
});

let lint: Lint | null = null;
let timer: number | undefined;
function schedule(): void {
  window.clearTimeout(timer);
  timer = window.setTimeout(run, 250);
}

function gatePasses(findings: Finding[]): boolean {
  const level = failon.value;
  if (level === "never") return true;
  return !findings.some((f) => level === "warning" || f.severity === "error");
}

function para(cls: string, text: string): HTMLParagraphElement {
  const p = document.createElement("p");
  p.className = cls;
  p.textContent = text;
  return p;
}

function render(env: Envelope): void {
  const file = env.files?.[0];
  resultsEl.replaceChildren();
  if (env.error || file?.error) {
    resultsEl.append(para("status", `Parse error: ${env.error ?? file?.error}`));
    return;
  }
  const findings = file?.findings ?? [];
  if (!findings.length) {
    resultsEl.append(para("clean", "✓ No findings — this migration looks safe."));
    return;
  }
  for (const f of findings) {
    const el = document.createElement("div");
    el.className = "finding";

    const head = document.createElement("div");
    head.className = "head";
    const sev = document.createElement("span");
    sev.className = f.severity === "error" ? "sev-error" : "sev-warning";
    sev.textContent = f.severity;
    const link = document.createElement("a");
    link.href = `/rules/${encodeURIComponent(f.rule_id)}/`;
    link.textContent = f.rule_id;
    head.append(sev, link);

    el.append(
      head,
      para("msg", f.message),
      para("loc", `line ${f.location.line}, col ${f.location.column}`),
    );
    resultsEl.append(el);
  }
  resultsEl.append(
    para("gate", gatePasses(findings) ? "Gate: would pass" : "Gate: would fail (exit 1)"),
  );
}

function run(): void {
  if (!lint) return;
  const text = view.state.doc.toString();
  writeHash({ sql: text, inTransaction: intx.checked });
  try {
    render(lint(text, { inTransaction: intx.checked }));
  } catch (e) {
    resultsEl.innerHTML = `<p class="status">Linter error: ${String(e)}</p>`;
  }
}

intx.addEventListener("change", run);
failon.addEventListener("change", run);

const examplesSel = byId<HTMLSelectElement>("examples");
for (const ex of EXAMPLES) {
  const o = document.createElement("option");
  o.value = ex.id;
  o.textContent = ex.label;
  examplesSel.append(o);
}
examplesSel.addEventListener("change", () => {
  const ex = EXAMPLES.find((e) => e.id === examplesSel.value);
  if (ex) {
    intx.checked = !!ex.inTransaction;
    view.dispatch({
      changes: { from: 0, to: view.state.doc.length, insert: ex.sql },
    });
    examplesSel.value = "";
  }
});

byId("permalink").addEventListener("click", async () => {
  writeHash({ sql: view.state.doc.toString(), inTransaction: intx.checked });
  await navigator.clipboard.writeText(location.href);
  const b = byId("permalink");
  const t = b.textContent;
  b.textContent = "Copied!";
  setTimeout(() => (b.textContent = t), 1200);
});

loadLinter()
  .then((fn) => {
    lint = fn;
    statusEl.textContent = "";
    run();
  })
  .catch((e) => {
    statusEl.textContent = `Could not load the linter: ${String(e)}`;
  });
