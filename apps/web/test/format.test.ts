import { describe, expect, it } from "vitest";
import { formatAge, formatDateTime, formatDateTimeCompact, renderPayload } from "../src/lib/format";
import { workflowTransition } from "../src/lib/events";

describe("formatting", () => {
  it("renders ISO timestamps in a locale string", () => {
    const text = formatDateTime("2026-01-15T10:30:00Z");
    expect(text).not.toContain("T10:30:00");
    expect(text.length).toBeGreaterThan(8);
  });

  it("passes invalid timestamps through untouched", () => {
    expect(formatDateTime("not-a-date")).toBe("not-a-date");
  });

  it("renders a compact stamp that fits tight columns", () => {
    // Same year: the year is dropped; invalid input passes through.
    const now = Date.parse("2026-08-28T12:00:00Z");
    const text = formatDateTimeCompact("2026-08-26T16:44:07Z", now);
    expect(text).not.toContain("2026");
    expect(text.length).toBeGreaterThan(4);
    expect(formatDateTimeCompact("not-a-date", now)).toBe("not-a-date");
    // A different year keeps it - old data must not lose its year.
    expect(formatDateTimeCompact("2024-01-15T10:30:00Z", now)).toContain("2024");
  });

  it("ages timestamps compactly", () => {
    const now = Date.parse("2026-01-15T12:00:00Z");
    expect(formatAge("2026-01-15T11:59:58Z", now)).toBe("now");
    expect(formatAge("2026-01-15T11:59:30Z", now)).toBe("30s ago");
    expect(formatAge("2026-01-15T11:00:00Z", now)).toBe("1h ago");
    expect(formatAge("2026-01-15T06:00:00Z", now)).toBe("6h ago");
    expect(formatAge("2026-01-10T12:00:00Z", now)).toBe("5d ago");
  });

  it("bounds payload text hard", () => {
    const long = "x".repeat(1000);
    const rendered = renderPayload({ blob: long }, 100);
    expect(rendered.length).toBeLessThanOrEqual(100);
    expect(rendered.endsWith("\u2026")).toBe(true);
  });
});

describe("event interpretation", () => {
  it("recognizes workflow transitions", () => {
    const view = workflowTransition({
      type: "workflow_state_changed",
      data: { from: "planning", to: "awaiting_approval", trigger: "submit_for_approval" },
    });
    expect(view?.from).toBe("planning");
    expect(view?.to).toBe("awaiting_approval");
    expect(view?.trigger).toBe("submit_for_approval");
  });

  it("returns null for anything else instead of guessing", () => {
    expect(workflowTransition(null)).toBeNull();
    expect(workflowTransition({ type: "something_else" })).toBeNull();
    expect(workflowTransition({ type: "workflow_state_changed", data: "oops" })).toBeNull();
  });
});
