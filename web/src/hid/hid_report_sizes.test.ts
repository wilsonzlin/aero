import { describe, expect, it } from "vitest";

import { computeInputReportPayloadByteLengths } from "./hid_report_sizes";

describe("hid/computeInputReportPayloadByteLengths", () => {
  it("aggregates bit lengths across collections per reportId and rounds up to whole bytes", () => {
    const collections = [
      {
        usagePage: 1,
        usage: 2,
        collectionType: 1,
        children: [
          {
            usagePage: 1,
            usage: 2,
            collectionType: 1,
            children: [],
            inputReports: [
              {
                reportId: 1,
                items: [{ reportSize: 1, reportCount: 1 }],
              },
            ],
            outputReports: [],
            featureReports: [],
          },
        ],
        inputReports: [
          {
            reportId: 1,
            items: [{ reportSize: 8, reportCount: 4 }], // 32 bits
          },
          {
            reportId: 2,
            items: [{ reportSize: 16, reportCount: 1 }], // 16 bits
          },
        ],
        outputReports: [],
        featureReports: [],
      },
    ] as any;

    const sizes = computeInputReportPayloadByteLengths(collections);
    // reportId 1: 32+1 bits => 5 bytes
    expect(sizes.get(1)).toBe(5);
    // reportId 2: 16 bits => 2 bytes
    expect(sizes.get(2)).toBe(2);
  });
});

