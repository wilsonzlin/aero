import { describe, expect, it } from "vitest";

import type { WasmApi } from "./wasm_loader";

describe("runtime/wasm_loader (UhciRuntime WebHID drain typings)", () => {
  it("requires null-handling for UhciRuntime.webhid_drain_output_reports()", () => {
    type Runtime = InstanceType<NonNullable<WasmApi["UhciRuntime"]>>;

    // Note: Vitest runs these tests at runtime without TypeScript typechecking, so we must provide
    // a concrete implementation to avoid `undefined is not a function` crashes. The compile-time
    // checks are encoded via `@ts-expect-error` comments and validated in CI by `tsc`.
    const runtime = {
      webhid_drain_output_reports: () => null,
      webhid_drain_feature_report_requests: () => null,
    } as unknown as Runtime;

    function assertStrictNullChecksEnforced() {
      type OutputReports = NonNullable<
        ReturnType<Runtime["webhid_drain_output_reports"]>
      >;
      const _nullIsAllowed: ReturnType<
        Runtime["webhid_drain_output_reports"]
      > = null;
      void _nullIsAllowed;

      // @ts-expect-error webhid_drain_output_reports can return null
      const _reports: OutputReports = runtime.webhid_drain_output_reports();
      void _reports;

      // `webhid_drain_feature_report_requests` is optional; when present its return type must also
      // include `null`.
      const drain = runtime.webhid_drain_feature_report_requests;
      if (drain) {
        type FeatureRequests = NonNullable<ReturnType<typeof drain>>;
        const _nullOk: ReturnType<typeof drain> = null;
        void _nullOk;

        // @ts-expect-error webhid_drain_feature_report_requests can return null
        const _reqs: FeatureRequests = drain();
        void _reqs;
      }
    }
    void assertStrictNullChecksEnforced;

    const drained = runtime.webhid_drain_output_reports();
    expect(drained).toBeNull();

    const drainFeature = runtime.webhid_drain_feature_report_requests;
    if (drainFeature) {
      expect(drainFeature()).toBeNull();
    }
  });
});

