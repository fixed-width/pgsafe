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

// Claimed lines get an editor-style wavy underline by severity, so the user can
// see at a glance which lines carry findings. Red (error) wins over yellow
// (warning) when a line has both. Suppressed (ignored) findings aren't
// underlined — they're acknowledged, and the results panel already dims them.
const setClaims = StateEffect.define<Map<number, "error" | "warning">>();
const claimsField = StateField.define<DecorationSet>({
  create: () => Decoration.none,
  update(deco, tr) {
    deco = deco.map(tr.changes);
    for (const e of tr.effects) {
      if (e.is(setClaims)) {
        // Resolve finding lines to line-start positions, letting error win over
        // warning when both land on the same line.
        const worst = new Map<number, "error" | "warning">();
        for (const [ln, sev] of e.value) {
          const lineNo = Math.max(1, Math.min(ln, tr.state.doc.lines));
          const from = tr.state.doc.line(lineNo).from;
          if (sev === "error" || !worst.has(from)) worst.set(from, sev);
        }
        deco = Decoration.set(
          [...worst.entries()]
            .sort((a, b) => a[0] - b[0])
            .map(([from, sev]) =>
              Decoration.line({
                class: sev === "error" ? "cm-claim-error" : "cm-claim-warning",
              }).range(from),
            ),
          true,
        );
      }
    }
    return deco;
  },
  provide: (f) => EditorView.decorations.from(f),
});
const claimsTheme = EditorView.theme({
  ".cm-line.cm-claim-error": {
    textDecoration: "underline wavy #f85149", // --error
    textDecorationSkipInk: "none",
    textUnderlineOffset: "3px",
  },
  ".cm-line.cm-claim-warning": {
    textDecoration: "underline wavy #e3b341", // --warning
    textDecorationSkipInk: "none",
    textUnderlineOffset: "3px",
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
    claimsField,
    claimsTheme,
    EditorView.updateListener.of((u) => {
      if (u.docChanged) schedule();
    }),
  ],
  parent: byId("editor"),
});

function highlightLine(line: number | null): void {
  view.dispatch({ effects: setHighlight.of(line) });
}

// Push the per-line worst severity to the editor so claimed lines are
// underlined. Error takes priority over warning on a line that has both.
function setClaimLines(findings: Finding[]): void {
  const worst = new Map<number, "error" | "warning">();
  for (const f of findings) {
    if (f.suppression) continue; // ignored findings aren't underlined
    const sev = f.severity === "error" ? "error" : "warning";
    if (sev === "error" || !worst.has(f.location.line)) {
      worst.set(f.location.line, sev);
    }
  }
  view.dispatch({ effects: setClaims.of(worst) });
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
  const active = findings.filter((f) => !f.suppression); // ignored findings don't gate
  return !active.some((f) => level === "warning" || f.severity === "error");
}

function para(cls: string, text: string): HTMLParagraphElement {
  const p = document.createElement("p");
  p.className = cls;
  p.textContent = text;
  return p;
}

/** Append text with backtick code spans rendered as <code> — XSS-safe (every
 *  piece goes in via textContent), for finding messages that contain code. */
function appendInline(parent: HTMLElement, text: string): void {
  const re = /`([^`]+)`/g;
  let last = 0;
  let m: RegExpExecArray | null;
  while ((m = re.exec(text)) !== null) {
    if (m.index > last) parent.appendChild(document.createTextNode(text.slice(last, m.index)));
    const code = document.createElement("code");
    code.textContent = m[1];
    parent.appendChild(code);
    last = re.lastIndex;
  }
  if (last < text.length) parent.appendChild(document.createTextNode(text.slice(last)));
}

/** Map a UTF-8 byte offset (what the wasm reports) to a CodeMirror (UTF-16)
 *  string index. 1:1 for ASCII; correct past multi-byte characters. */
function byteToChar(text: string, targetByte: number): number {
  let byte = 0;
  let i = 0;
  while (i < text.length && byte < targetByte) {
    const cp = text.codePointAt(i)!;
    byte += cp < 0x80 ? 1 : cp < 0x800 ? 2 : cp < 0x10000 ? 3 : 4;
    i += cp > 0xffff ? 2 : 1;
  }
  return i;
}

/** Apply a finding's fix to the editor. The edits are non-overlapping and
 *  ascending (engine contract), so CodeMirror composes them in one transaction;
 *  the resulting docChanged re-lints. */
function applyFixToEditor(fix: NonNullable<Finding["fix"]>): void {
  if (fix.edits.length === 0) {
    console.error("applyFixToEditor: fix has no edits", fix);
    return;
  }
  const text = view.state.doc.toString();
  view.dispatch({
    changes: fix.edits.map((e) => ({
      from: byteToChar(text, e.start),
      to: byteToChar(text, e.end),
      insert: e.replacement,
    })),
  });
}

/** Insert a one-click ignore directive above the finding's statement. The reason
 *  marks its provenance (and satisfies pgsafe's suppression-missing-reason rule). */
function ignoreFinding(f: Finding): void {
  const lineNo = Math.max(1, Math.min(f.location.line, view.state.doc.lines));
  const from = view.state.doc.line(lineNo).from;
  const directive = `-- pgsafe:ignore ${f.rule_id}  acknowledged in the pgsafe playground\n`;
  view.dispatch({ changes: { from, to: from, insert: directive } });
}

function render(env: Envelope): void {
  // Re-rendering replaces the finding rows, so any hovered row is gone without
  // a mouseleave — clear its stale line highlight before rebuilding.
  highlightLine(null);
  resultsEl.replaceChildren();
  if ("error" in env) {
    setClaimLines([]);
    resultsEl.append(para("status", `pgsafe error: ${env.error}`));
    return;
  }
  const file = env.files[0];
  if (file?.error) {
    setClaimLines([]);
    resultsEl.append(para("status", `Parse error: ${file.error}`));
    return;
  }
  const findings = file?.findings ?? [];
  setClaimLines(findings);
  // Capture the doc this render was computed against. Button click handlers
  // compare view.state.doc to this reference — identical object means the doc
  // hasn't changed since lint; a different object means a re-lint is pending
  // and the byte offsets are stale, so we bail rather than splice at the wrong
  // position or throw a CodeMirror RangeError.
  const lintedDoc = view.state.doc;
  if (!findings.length) {
    resultsEl.append(para("clean", "✓ No findings — this migration looks safe."));
    return;
  }
  for (const f of findings) {
    const el = document.createElement("div");
    el.className = f.suppression ? "finding suppressed" : "finding";
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
    // Open the rule reference in a new tab — keep the user's migration + findings
    // in the playground while they read.
    link.target = "_blank";
    link.rel = "noopener";
    link.title = "Open the rule reference in a new tab";
    head.append(sev, link);
    if (f.suppression) {
      const tag = document.createElement("span");
      tag.className = "ignored";
      tag.textContent = "ignored";
      head.append(tag);
    }

    const loc = f.suppression
      ? `ignored — ${f.suppression.reason} · line ${f.location.line}`
      : `line ${f.location.line}, col ${f.location.column}`;
    const msg = document.createElement("p");
    msg.className = "msg";
    appendInline(msg, f.message);
    if (f.suppression) {
      el.append(head, msg, para("loc", loc));
    } else {
      const actions = document.createElement("div");
      actions.className = "actions";
      if (f.fix) {
        const fixBtn = document.createElement("button");
        fixBtn.type = "button";
        fixBtn.className = "fix";
        fixBtn.textContent = f.fix.title || "Fix";
        fixBtn.title = "Apply this fix to the migration";
        const thisFix = f.fix;
        fixBtn.addEventListener("click", () => {
          if (view.state.doc !== lintedDoc) return; // doc changed since this lint; a re-lint is pending
          try {
            applyFixToEditor(thisFix);
          } catch (e) {
            console.error("Fix failed:", e);
            resultsEl.prepend(para("status", `Could not apply fix: ${String(e)}`));
          }
        });
        actions.append(fixBtn);
      }
      const ignoreBtn = document.createElement("button");
      ignoreBtn.type = "button";
      ignoreBtn.className = "ignore";
      ignoreBtn.textContent = "Ignore";
      ignoreBtn.title = "Insert a pgsafe:ignore directive for this finding";
      ignoreBtn.addEventListener("click", () => {
        if (view.state.doc !== lintedDoc) return; // doc changed since this lint; a re-lint is pending
        try {
          ignoreFinding(f);
        } catch (e) {
          console.error("Ignore failed:", e);
          resultsEl.prepend(para("status", `Could not ignore finding: ${String(e)}`));
        }
      });
      actions.append(ignoreBtn);
      el.append(head, msg, para("loc", loc), actions);
    }
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
    console.error(e);
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
  const b = byId("permalink");
  const t = b.textContent;
  try {
    await navigator.clipboard.writeText(location.href);
    b.textContent = "Copied!";
  } catch {
    // clipboard can be blocked (permission, no focus, insecure context)
    b.textContent = "Copy from the address bar";
  }
  setTimeout(() => (b.textContent = t), 1500);
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
