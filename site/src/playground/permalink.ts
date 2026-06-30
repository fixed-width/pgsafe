export interface State {
  sql: string;
  inTransaction: boolean;
}

/** Encode the editor state into the URL hash (UTF-8-safe base64). */
export function writeHash(s: State): void {
  const json = JSON.stringify(s);
  const b64 = btoa(String.fromCharCode(...new TextEncoder().encode(json)));
  history.replaceState(null, "", `#${b64}`);
}

/** Decode the URL hash back into editor state, or null if absent/invalid. */
export function readHash(): State | null {
  const h = location.hash.slice(1);
  if (!h) return null;
  try {
    const bytes = Uint8Array.from(atob(h), (c) => c.charCodeAt(0));
    return JSON.parse(new TextDecoder().decode(bytes)) as State;
  } catch {
    return null;
  }
}
