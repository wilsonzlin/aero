import assert from "node:assert/strict";
import test from "node:test";

import { selectAllowedDnsAddress } from "../src/routes/tcpDns.js";

test("selectAllowedDnsAddress returns null for empty address lists", async () => {
  assert.equal(selectAllowedDnsAddress([], true), null);
  assert.equal(selectAllowedDnsAddress([], false), null);
});

test("selectAllowedDnsAddress returns first address when allowPrivateIps is true", async () => {
  const chosen = selectAllowedDnsAddress([{ address: "127.0.0.1", family: 4 }, { address: "8.8.8.8", family: 4 }], true);
  assert.deepEqual(chosen, { address: "127.0.0.1", family: 4 });
});

test("selectAllowedDnsAddress returns first public address when allowPrivateIps is false", async () => {
  const chosen = selectAllowedDnsAddress([{ address: "127.0.0.1", family: 4 }, { address: "8.8.8.8", family: 4 }], false);
  assert.deepEqual(chosen, { address: "8.8.8.8", family: 4 });
});

test("selectAllowedDnsAddress returns null when allowPrivateIps is false and only private addresses exist", async () => {
  const chosen = selectAllowedDnsAddress([{ address: "127.0.0.1", family: 4 }, { address: "::1", family: 6 }], false);
  assert.equal(chosen, null);
});

