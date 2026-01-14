import { describe, expect, it } from "vitest";

import {
  computeFeatureReportPayloadByteLengths,
  computeInputReportPayloadByteLengths,
  computeOutputReportPayloadByteLengths,
} from "./hid_report_sizes";
import type { NormalizedHidCollectionInfo } from "./webhid_normalize";

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
    ] as unknown as NormalizedHidCollectionInfo[];

    const sizes = computeInputReportPayloadByteLengths(collections);
    // reportId 1: 32+1 bits => 5 bytes
    expect(sizes.get(1)).toBe(5);
    // reportId 2: 16 bits => 2 bytes
    expect(sizes.get(2)).toBe(2);
  });
});

describe("hid/computeFeatureReportPayloadByteLengths", () => {
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
            inputReports: [],
            outputReports: [],
            featureReports: [
              {
                reportId: 1,
                items: [{ reportSize: 1, reportCount: 1 }],
              },
            ],
          },
        ],
        inputReports: [],
        outputReports: [],
        featureReports: [
          {
            reportId: 1,
            items: [{ reportSize: 8, reportCount: 4 }], // 32 bits
          },
          {
            reportId: 2,
            items: [{ reportSize: 16, reportCount: 1 }], // 16 bits
          },
        ],
      },
    ] as unknown as NormalizedHidCollectionInfo[];

    const sizes = computeFeatureReportPayloadByteLengths(collections);
    // reportId 1: 32+1 bits => 5 bytes
    expect(sizes.get(1)).toBe(5);
    // reportId 2: 16 bits => 2 bytes
    expect(sizes.get(2)).toBe(2);
  });
});

describe("hid/computeOutputReportPayloadByteLengths", () => {
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
            inputReports: [],
            outputReports: [
              {
                reportId: 1,
                items: [{ reportSize: 1, reportCount: 1 }],
              },
            ],
            featureReports: [],
          },
        ],
        inputReports: [],
        outputReports: [
          {
            reportId: 1,
            items: [{ reportSize: 8, reportCount: 4 }], // 32 bits
          },
          {
            reportId: 2,
            items: [{ reportSize: 16, reportCount: 1 }], // 16 bits
          },
        ],
        featureReports: [],
      },
    ] as unknown as NormalizedHidCollectionInfo[];

    const sizes = computeOutputReportPayloadByteLengths(collections);
    // reportId 1: 32+1 bits => 5 bytes
    expect(sizes.get(1)).toBe(5);
    // reportId 2: 16 bits => 2 bytes
    expect(sizes.get(2)).toBe(2);
  });
});
