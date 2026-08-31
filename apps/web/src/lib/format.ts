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
 * Compact variant for tight columns (the workflow timeline's current
 * step): drops the year when it is the current one and the seconds,
 * so the timestamp fits without forcing horizontal overflow.
 */
export function formatDateTimeCompact(iso: string, now: number = Date.now()): string {
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return iso;
  const sameYear = date.getFullYear() === new Date(now).getFullYear();
  return date.toLocaleString(undefined, {
    ...(sameYear ? {} : { year: "numeric" }),
    month: "short",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
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
 * Byte count rendered in the smallest whole unit that fits, so the
 * artifact panel's sizes stay readable from a few KB to a few GB.
 * Negative or non-finite input renders as "0 B" rather than garbage.
 */
export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = bytes;
  let index = 0;
  while (value >= 1024 && index < units.length - 1) {
    value /= 1024;
    index += 1;
  }
  const rounded = index === 0 ? String(value) : value.toFixed(1).replace(/\.0$/, "");
  return rounded + " " + units[index];
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
