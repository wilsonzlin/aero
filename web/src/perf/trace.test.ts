import { describe, expect, it } from "vitest";

import { nowEpochUs, TraceRecorder } from "./trace";

describe("TraceRecorder", () => {
  it("records spans, instants, and counters when enabled", () => {
    const rec = new TraceRecorder({
      pid: 1,
      tid: 2,
      processName: "Aero",
      threadName: "Test",
      maxRecords: 16,
      maxStrings: 16,
    });

    rec.start(nowEpochUs(), true);
    rec.spanBegin("span");
    rec.counter("counter", 123);
    rec.instant("instant", "t", { foo: 1 });
    rec.spanEnd("span");

    const events = rec.exportEvents("unit");
    expect(events.map((e) => e.ph)).toEqual(["B", "C", "i", "E"]);
    expect(events[0]).toMatchObject({ name: "span", cat: "unit", pid: 1, tid: 2 });
    expect(events[1]).toMatchObject({ name: "counters", ph: "C", args: { counter: 123 } });
    expect(events[2]).toMatchObject({ name: "instant", ph: "i", s: "t", args: { foo: 1 } });
    expect(events[3]).toMatchObject({ name: "span", cat: "unit", pid: 1, tid: 2 });
  });

  it("is a no-op when disabled", () => {
    const rec = new TraceRecorder({
      pid: 1,
      tid: 1,
      processName: "Aero",
      threadName: "Test",
      maxRecords: 16,
      maxStrings: 16,
    });

    rec.spanBegin("a");
    rec.spanEnd("a");
    rec.instant("i");
    rec.counter("c", 1);

    expect(rec.exportEvents()).toEqual([]);
    expect(rec.getDroppedRecords()).toBe(0);
    expect(rec.getDroppedStrings()).toBe(0);
  });

  it("bounds the record buffer and reports overwritten records", () => {
    const rec = new TraceRecorder({
      pid: 1,
      tid: 1,
      processName: "Aero",
      threadName: "Test",
      maxRecords: 2,
      maxStrings: 16,
    });

    rec.start(nowEpochUs(), true);
    rec.instant("e1");
    rec.instant("e2");
    rec.instant("e3");

    const events = rec.exportEvents();
    expect(events.map((e) => e.name)).toEqual(["e2", "e3"]);
    expect(rec.getDroppedRecords()).toBe(1);
  });

  it("bounds the string table and falls back to <unknown>", () => {
    const rec = new TraceRecorder({
      pid: 1,
      tid: 1,
      processName: "Aero",
      threadName: "Test",
      maxRecords: 8,
      // includes "<unknown>", so only one extra unique string can be stored
      maxStrings: 2,
    });

    rec.start(nowEpochUs(), true);
    rec.spanBegin("a");
    rec.spanBegin("b");

    const events = rec.exportEvents();
    expect(events.map((e) => e.name)).toEqual(["a", "<unknown>"]);
    expect(rec.getDroppedStrings()).toBe(1);
  });
});

