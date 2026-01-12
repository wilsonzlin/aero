import { describe, expect, it } from "vitest";

import { AudioFrameClock } from "./audio_frame_clock";

describe("AudioFrameClock", () => {
  function tick60HzNs(tickIndex: number): bigint {
    // 1 second / 60 = 16_666_666.666... ns. Distribute the extra 40ns across the
    // second: 40 ticks of 16_666_667ns and 20 ticks of 16_666_666ns.
    return tickIndex < 40 ? 16_666_667n : 16_666_666n;
  }

  it("splitting an interval into many steps yields the same total frames as one step", () => {
    const sampleRate = 44_100;
    const totalNs = 1_000_000_000n; // 1 second

    const oneShot = new AudioFrameClock(sampleRate, 0n);
    const framesOneShot = oneShot.advanceTo(totalNs);
    expect(framesOneShot).toBe(sampleRate);

    const clock = new AudioFrameClock(sampleRate, 0n);
    let framesSplit = 0;
    const stepNs = 1_000_000n; // 1ms (44.1 frames @ 44.1kHz)
    for (let i = 1; i <= 1000; i++) {
      framesSplit += clock.advanceTo(BigInt(i) * stepNs);
    }
    expect(framesSplit).toBe(framesOneShot);
  });

  it("matches Rust's 60Hz jitter pattern (1s split into 60 uneven steps)", () => {
    const sampleRate = 48_000;
    const totalNs = 1_000_000_000n;

    const oneShot = new AudioFrameClock(sampleRate, 0n);
    const framesOneShot = oneShot.advanceTo(totalNs);

    const clock = new AudioFrameClock(sampleRate, 0n);
    let nowNs = 0n;
    let framesSplit = 0;
    for (let tick = 0; tick < 60; tick++) {
      nowNs += tick60HzNs(tick);
      framesSplit += clock.advanceTo(nowNs);
    }

    expect(nowNs).toBe(totalNs);
    expect(framesSplit).toBe(framesOneShot);
    expect(framesSplit).toBe(sampleRate);
    expect(clock.fracNsTimesRate).toBe(0n);
  });

  it("does not drift over ten minutes at 60Hz with jittery step sizes", () => {
    const sampleRate = 48_000;
    const clock = new AudioFrameClock(sampleRate, 0n);

    let nowNs = 0n;
    let totalFrames = 0;
    for (let second = 0; second < 600; second++) {
      for (let tick = 0; tick < 60; tick++) {
        nowNs += tick60HzNs(tick);
        totalFrames += clock.advanceTo(nowNs);
      }
    }

    expect(nowNs).toBe(1_000_000_000n * 600n);
    expect(totalFrames).toBe(sampleRate * 600);
    expect(clock.fracNsTimesRate).toBe(0n);
  });

  it("jittery step sizes don't accumulate drift", () => {
    // Deterministic PRNG (LCG) so the test is stable.
    let state = 0x1234_5678;
    const rand = () => {
      state = (state * 1664525 + 1013904223) >>> 0;
      return state / 0x1_0000_0000;
    };

    const sampleRate = 48_000;
    const totalNs = 10_000_000_123n; // 10s + a bit

    const expectedClock = new AudioFrameClock(sampleRate, 0n);
    const expected = expectedClock.advanceTo(totalNs);

    const clock = new AudioFrameClock(sampleRate, 0n);
    let now = 0n;
    let totalFrames = 0;

    const maxStepNs = 50_000_000; // 50ms (fits safely in a JS number)
    while (now < totalNs) {
      const remaining = totalNs - now;
      const step = Math.max(1, Math.floor(rand() * maxStepNs));
      const stepNs = BigInt(step) > remaining ? remaining : BigInt(step);
      now += stepNs;
      totalFrames += clock.advanceTo(now);
    }

    expect(totalFrames).toBe(expected);
  });

  it("backwards time doesn't produce frames (and does not move the clock backwards)", () => {
    const clock = new AudioFrameClock(1000, 0n);

    expect(clock.advanceTo(1_000_000_000n)).toBe(1000);
    const last = clock.lastTimeNs;

    expect(clock.advanceTo(500_000_000n)).toBe(0);
    expect(clock.lastTimeNs).toBe(last);

    // Only 0.5s elapsed since the last accepted time.
    expect(clock.advanceTo(1_500_000_000n)).toBe(500);
  });
});
