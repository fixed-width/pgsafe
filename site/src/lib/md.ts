const escapeHtml = (s: string): string =>
  s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");

/** Render trusted inline prose (from rules.ts) to HTML: backtick code spans
 *  become <code>, everything else is HTML-escaped. Only emits <code>, so it is
 *  XSS-safe even though it feeds set:html — not for untrusted input regardless. */
export function inlineMd(s: string): string {
  let out = "";
  let last = 0;
  const re = /`([^`]+)`/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(s)) !== null) {
    out += escapeHtml(s.slice(last, m.index)) + `<code>${escapeHtml(m[1])}</code>`;
    last = re.lastIndex;
  }
  return out + escapeHtml(s.slice(last));
}

/** Strip inline-code markers for plain-text contexts (meta descriptions, filters). */
export function stripMd(s: string): string {
  return s.replace(/`([^`]+)`/g, "$1");
}
