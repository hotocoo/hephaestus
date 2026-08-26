import { describe, expect, it } from "vitest";
import {
  WORKFLOW_STATES,
  isTerminalState,
  priorityRank,
  priorityTone,
  riskTone,
  stateLabel,
  stateProgress,
  stateTone,
} from "../src/lib/workflow";

describe("workflow spine", () => {
  it("orders states along the lifecycle with terminals last", () => {
    expect(WORKFLOW_STATES[0]).toBe("created");
    expect(WORKFLOW_STATES.at(-3)).toBe("completed");
    expect(WORKFLOW_STATES.at(-2)).toBe("failed");
    expect(WORKFLOW_STATES.at(-1)).toBe("cancelled");
  });

  it("labels states for humans", () => {
    expect(stateLabel("awaiting_approval")).toBe("awaiting approval");
    expect(stateLabel("created")).toBe("created");
  });

  it("classifies tones", () => {
    expect(stateTone("completed")).toBe("ok");
    expect(stateTone("failed")).toBe("bad");
    expect(stateTone("cancelled")).toBe("bad");
    expect(stateTone("awaiting_approval")).toBe("idle");
    expect(stateTone("implementing")).toBe("busy");
  });

  it("knows terminal states", () => {
    expect(isTerminalState("completed")).toBe(true);
    expect(isTerminalState("implementing")).toBe(false);
  });
});

describe("stateProgress", () => {
  it("measures distance along the happy path", () => {
    const created = stateProgress("created");
    expect(created.reachedIndex).toBe(0);
    expect(created.percent).toBe(0);

    const awaiting = stateProgress("awaiting_approval");
    expect(awaiting.percent).toBeGreaterThan(20);
    expect(awaiting.percent).toBeLessThan(50);
  });

  it("keeps the reached distance for terminal failures", () => {
    const failed = stateProgress("failed");
    const completed = stateProgress("completed");
    // failed sits one slot past completed on the spine, so a dead run
    // still shows how far it got - never less than what it reached.
    expect(failed.known).toBe(true);
    expect(failed.percent).toBeGreaterThan(completed.percent);
  });

  it("renders unknown future-server states verbatim at full bar", () => {
    const unknown = stateProgress("quantum_forge");
    expect(unknown.known).toBe(false);
    expect(unknown.percent).toBe(100);
  });
});

describe("classification tones", () => {
  it("ranks priorities most-urgent first", () => {
    expect(priorityRank("critical")).toBeLessThan(priorityRank("high"));
    expect(priorityRank("high")).toBeLessThan(priorityRank("medium"));
    expect(priorityRank("medium")).toBeLessThan(priorityRank("low"));
  });

  it("tones priorities and risks", () => {
    expect(priorityTone("critical")).toBe("bad");
    expect(priorityTone("low")).toBe("idle");
    expect(riskTone("high")).toBe("warn");
    expect(riskTone("critical")).toBe("bad");
  });
});
