import { describe, expect, it } from "vitest";

import { AudioFrameClock } from "./audio_frame_clock";

const SAMPLE_RATE_HZ = 48_000;
const NS_PER_SEC = 1_000_000_000;

function tick60HzNs(tickIndex: number): number {
  // 1 second / 60 = 16_666_666.666... ns. Use a common pattern that distributes the extra
  // 40 nanoseconds across the second: 40 ticks of 16_666_667ns and 20 ticks of 16_666_666ns.
  return tickIndex < 40 ? 16_666_667 : 16_666_666;
}

describe("AudioFrameClock", () => {
  it("advance one second is exact", () => {
    const clock = new AudioFrameClock(SAMPLE_RATE_HZ, 0);
    expect(clock.advanceTo(NS_PER_SEC)).toBe(SAMPLE_RATE_HZ);
    expect(clock.fracFp).toBe(0);
  });

  it("repeated small steps sum to a single large step", () => {
    const clockSingle = new AudioFrameClock(SAMPLE_RATE_HZ, 0);
    const single = clockSingle.advanceTo(NS_PER_SEC);

    const clockSteps = new AudioFrameClock(SAMPLE_RATE_HZ, 0);
    let nowNs = 0;
    let total = 0;
    for (let tick = 0; tick < 60; tick++) {
      nowNs += tick60HzNs(tick);
      total += clockSteps.advanceTo(nowNs);
    }

    expect(nowNs).toBe(NS_PER_SEC);
    expect(total).toBe(single);
    expect(total).toBe(SAMPLE_RATE_HZ);
    expect(clockSteps.fracFp).toBe(0);
  });

  it("no drift over ten minutes at 60Hz", () => {
    const clock = new AudioFrameClock(SAMPLE_RATE_HZ, 0);
    let nowNs = 0;
    let totalFrames = 0;

    for (let second = 0; second < 600; second++) {
      for (let tick = 0; tick < 60; tick++) {
        nowNs += tick60HzNs(tick);
        totalFrames += clock.advanceTo(nowNs);
      }
    }

    expect(nowNs).toBe(NS_PER_SEC * 600);
    expect(totalFrames).toBe(SAMPLE_RATE_HZ * 600);
    expect(clock.fracFp).toBe(0);
  });

  it("time going backwards is ignored", () => {
    const clock = new AudioFrameClock(SAMPLE_RATE_HZ, 1_000);
    clock.advanceTo(2_000);
    expect(clock.lastTimeNs).toBe(2_000);
    expect(clock.advanceTo(1_500)).toBe(0);
    expect(clock.lastTimeNs).toBe(2_000);
  });

  it("jittery tick sequence totals exactly over long runs", () => {
    const sampleRateHz = 44_100;
    const clock = new AudioFrameClock(sampleRateHz, 0);

    let nowNs = 0;
    let totalFrames = 0;
    const tickDeltasNs = [7_000_000, 9_000_000, 8_000_000];

    for (let i = 0; i < 1000; i++) {
      for (const delta of tickDeltasNs) {
        nowNs += delta;
        totalFrames += clock.advanceTo(nowNs);
      }
    }

    // 24ms per cycle * 1000 = 24s.
    expect(nowNs).toBe(24 * NS_PER_SEC);
    expect(totalFrames).toBe(sampleRateHz * 24);
    expect(clock.fracFp).toBe(0);
  });
});

