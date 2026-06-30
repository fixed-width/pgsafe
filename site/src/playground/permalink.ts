export interface State {
  sql: string;
  inTransaction: boolean;
}

/** Encode the editor state into the URL hash (UTF-8-safe base64). */
export function writeHash(s: State): void {
  const bytes = new TextEncoder().encode(JSON.stringify(s));
  // Build the binary string without spreading bytes as call args — a large
  // paste would exceed the argument limit and throw (RangeError).
  let bin = "";
  for (const byte of bytes) bin += String.fromCharCode(byte);
  history.replaceState(null, "", `#${btoa(bin)}`);
}

/** Decode the URL hash back into editor state, or null if absent/invalid.
 *  The hash is fully user-controlled, so the decoded shape is validated. */
export function readHash(): State | null {
  const h = location.hash.slice(1);
  if (!h) return null;
  try {
    const bytes = Uint8Array.from(atob(h), (c) => c.charCodeAt(0));
    const parsed = JSON.parse(new TextDecoder().decode(bytes)) as unknown;
    if (parsed && typeof parsed === "object" && typeof (parsed as State).sql === "string") {
      return {
        sql: (parsed as State).sql,
        inTransaction: Boolean((parsed as { inTransaction?: unknown }).inTransaction),
      };
    }
    return null;
  } catch {
    return null;
  }
}
