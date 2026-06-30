import { EditorView, basicSetup } from "codemirror";
import { sql, PostgreSQL } from "@codemirror/lang-sql";
import { oneDark } from "@codemirror/theme-one-dark";
import { StateField, StateEffect } from "@codemirror/state";
import { Decoration, type DecorationSet } from "@codemirror/view";
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

// Hovering a finding highlights its line in the editor.
const setHighlight = StateEffect.define<number | null>();
const highlightField = StateField.define<DecorationSet>({
  create: () => Decoration.none,
  update(deco, tr) {
    deco = deco.map(tr.changes);
    for (const e of tr.effects) {
      if (e.is(setHighlight)) {
        if (e.value == null) {
          deco = Decoration.none;
        } else {
          const lineNo = Math.max(1, Math.min(e.value, tr.state.doc.lines));
          const line = tr.state.doc.line(lineNo);
          deco = Decoration.set([
            Decoration.line({ class: "cm-hl-line" }).range(line.from),
          ]);
        }
      }
    }
    return deco;
  },
  provide: (f) => EditorView.decorations.from(f),
});
const highlightTheme = EditorView.theme({
  // Higher specificity than .cm-activeLine so the hover highlight wins even
  // when the cursor is on that line, plus an inset bar that shows over any bg.
  ".cm-line.cm-hl-line": {
    backgroundColor: "rgba(255, 159, 28, 0.22)",
    boxShadow: "inset 3px 0 0 0 #ff9f1c",
  },
});

const view = new EditorView({
  doc: startDoc,
  extensions: [
    basicSetup,
    sql({ dialect: PostgreSQL }),
    oneDark,
    highlightField,
    highlightTheme,
    EditorView.updateListener.of((u) => {
      if (u.docChanged) schedule();
    }),
  ],
  parent: byId("editor"),
});

function highlightLine(line: number | null): void {
  view.dispatch({ effects: setHighlight.of(line) });
}

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
    el.addEventListener("mouseenter", () => highlightLine(f.location.line));
    el.addEventListener("mouseleave", () => highlightLine(null));

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
    resultsEl.replaceChildren(para("status", `Linter error: ${String(e)}`));
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
