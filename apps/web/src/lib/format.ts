/**
 * Rendering helpers for wire data.
 *
 * Everything here treats server values as text to display, never as
 * markup: the dashboard renders untrusted payloads with text nodes
 * only (no v-html anywhere in the app).
 *
 * @module
 */

/** ISO timestamp rendered in the viewer's locale; invalid input passes through untouched. */
export function formatDateTime(iso: string): string {
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return iso;
  return date.toLocaleString(undefined, {
    year: "numeric",
    month: "short",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });
}

/**
 * Compact age like "3m ago"; recomputed on each call so views can
 * refresh it on their poll cycle. Future timestamps render as "now".
 */
export function formatAge(iso: string, now: number = Date.now()): string {
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return iso;
  const seconds = Math.max(0, Math.round((now - date.getTime()) / 1000));
  if (seconds < 5) return "now";
  if (seconds < 60) return seconds + "s ago";
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return minutes + "m ago";
  const hours = Math.floor(minutes / 60);
  if (hours < 48) return hours + "h ago";
  const days = Math.floor(hours / 24);
  return days + "d ago";
}

/** Short id form for tables and headings: first UUID group. */
export function shortId(id: string): string {
  const head = id.split("-")[0] ?? id;
  return head.length > 0 ? head : id;
}

/**
 * Event payload rendered as bounded text. Payloads are versioned JSON
 * of untrusted provenance; they are stringified, truncated hard and
 * displayed verbatim - never parsed into instructions, never marked up.
 */
export function renderPayload(payload: unknown, maxChars = 400): string {
  let text: string;
  if (typeof payload === "string") {
    text = payload;
  } else {
    try {
      text = JSON.stringify(payload);
    } catch {
      text = String(payload);
    }
  }
  if (text === undefined) text = "null";
  return text.length > maxChars ? text.slice(0, maxChars - 1) + "\u2026" : text;
}

/** Provenance classification with a stable short form for badges. */
export function provenanceLabel(provenance: string): string {
  return provenance;
}
