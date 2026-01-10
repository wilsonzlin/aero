import { test, expect } from '@playwright/test';

import { spawn, type ChildProcessWithoutNullStreams } from 'node:child_process';
import { once } from 'node:events';
import type { AddressInfo as DgramAddressInfo } from 'node:dgram';
import * as dgram from 'node:dgram';
import net, { type AddressInfo } from 'node:net';
import { setTimeout as delay } from 'node:timers/promises';

declare global {
  interface Window {
    __aeroGatewayE2E?: {
      crossOriginIsolated: boolean;
      sharedArrayBuffer: { ok: boolean; error: string | null };
      websocket: { ok: boolean; echo: unknown; error: string | null };
      dnsQuery: { ok: boolean; meta: unknown; error: string | null };
    };
  }
}

type StartedProcess = {
  baseUrl: string;
  stop: () => Promise<void>;
};

async function getFreePort(): Promise<number> {
  return await new Promise((resolve, reject) => {
    const server = net.createServer();
    server.once('error', reject);
    server.listen(0, '127.0.0.1', () => {
      const address = server.address() as AddressInfo;
      server.close((err) => {
        if (err) reject(err);
        else resolve(address.port);
      });
    });
  });
}

async function startTcpEchoServer(): Promise<{ port: number; close: () => Promise<void> }> {
  const server = net.createServer((socket) => {
    socket.on('data', (data) => {
      socket.write(data);
    });
  });

  await new Promise<void>((resolve, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', () => resolve());
  });

  const address = server.address() as AddressInfo;
  return {
    port: address.port,
    close: () =>
      new Promise<void>((resolve, reject) => {
        server.close((err) => {
          if (err) reject(err);
          else resolve();
        });
      }),
  };
}

async function startUdpDnsServer(): Promise<{ port: number; close: () => Promise<void> }> {
  const socket = dgram.createSocket('udp4');

  socket.on('message', (msg, rinfo) => {
    try {
      if (msg.length < 12) return;

      // Parse a single-question DNS query and respond with a single A record.
      let offset = 12;
      while (offset < msg.length) {
        const len = msg[offset];
        offset += 1;
        if (len === 0) break;
        offset += len;
      }
      const questionEnd = offset + 4; // QTYPE + QCLASS
      if (questionEnd > msg.length) return;

      const question = msg.subarray(12, questionEnd);
      const id = msg.readUInt16BE(0);

      const header = Buffer.alloc(12);
      header.writeUInt16BE(id, 0);
      header.writeUInt16BE(0x8180, 2); // standard response, recursion available
      header.writeUInt16BE(1, 4); // QDCOUNT
      header.writeUInt16BE(1, 6); // ANCOUNT
      header.writeUInt16BE(0, 8); // NSCOUNT
      header.writeUInt16BE(0, 10); // ARCOUNT

      const answer = Buffer.alloc(16);
      answer.writeUInt16BE(0xc00c, 0); // name: pointer to question
      answer.writeUInt16BE(1, 2); // TYPE=A
      answer.writeUInt16BE(1, 4); // CLASS=IN
      answer.writeUInt32BE(60, 6); // TTL
      answer.writeUInt16BE(4, 10); // RDLENGTH
      answer[12] = 93;
      answer[13] = 184;
      answer[14] = 216;
      answer[15] = 34;

      const response = Buffer.concat([header, question, answer]);
      socket.send(response, rinfo.port, rinfo.address);
    } catch {
      // ignore malformed packets
    }
  });

  await new Promise<void>((resolve, reject) => {
    socket.once('error', reject);
    socket.bind(0, '127.0.0.1', () => resolve());
  });

  const address = socket.address() as DgramAddressInfo;
  return {
    port: address.port,
    close: () =>
      new Promise<void>((resolve) => {
        socket.close(() => resolve());
      }),
  };
}

async function waitForHealthy(baseUrl: string, proc: ChildProcessWithoutNullStreams): Promise<void> {
  const deadline = Date.now() + 10_000;
  const url = `${baseUrl}/healthz`;

  while (Date.now() < deadline) {
    if (proc.exitCode !== null) {
      throw new Error(`Gateway exited early with code ${proc.exitCode}`);
    }
    try {
      const res = await fetch(url);
      if (res.ok) return;
    } catch {
      // ignore until ready
    }
    await delay(100);
  }

  throw new Error(`Gateway did not become healthy in time: ${url}`);
}

async function startGatewayProcess(opts: { crossOriginIsolation: boolean; dnsUpstreams: string }): Promise<StartedProcess> {
  const port = await getFreePort();
  const baseUrl = `http://localhost:${port}`;

  const proc = spawn(process.execPath, ['--import', 'tsx', 'src/index.ts'], {
    cwd: process.cwd(),
    env: {
      ...process.env,
      HOST: '127.0.0.1',
      PORT: String(port),
      LOG_LEVEL: 'silent',
      RATE_LIMIT_REQUESTS_PER_MINUTE: '0',
      CROSS_ORIGIN_ISOLATION: opts.crossOriginIsolation ? '1' : '0',
      DNS_UPSTREAMS: opts.dnsUpstreams,
      AERO_GATEWAY_E2E: '1',
    },
    stdio: ['ignore', 'pipe', 'pipe'],
  });

  const output: string[] = [];
  proc.stdout.on('data', (chunk) => output.push(String(chunk)));
  proc.stderr.on('data', (chunk) => output.push(String(chunk)));

  try {
    await waitForHealthy(baseUrl, proc);
  } catch (err) {
    proc.kill('SIGKILL');
    throw new Error(`${err instanceof Error ? err.message : String(err)}\nGateway output:\n${output.join('')}`);
  }

  return {
    baseUrl,
    stop: async () => {
      if (proc.exitCode !== null) return;
      proc.kill('SIGTERM');
      await Promise.race([once(proc, 'exit'), delay(5_000)]);
      if (proc.exitCode === null) proc.kill('SIGKILL');
    },
  };
}

test.describe('aero-gateway browser e2e', () => {
  test.describe.configure({ mode: 'serial' });

  test('CROSS_ORIGIN_ISOLATION=1 enables crossOriginIsolated + SharedArrayBuffer', async ({ page }) => {
    const dns = await startUdpDnsServer();
    const echo = await startTcpEchoServer();
    const gateway = await startGatewayProcess({ crossOriginIsolation: true, dnsUpstreams: `127.0.0.1:${dns.port}` });
    try {
      await page.goto(`${gateway.baseUrl}/e2e?echoPort=${echo.port}`);
      await page.waitForFunction(() => Boolean(window.__aeroGatewayE2E), null, { timeout: 10_000 });
      const results = await page.evaluate(() => window.__aeroGatewayE2E!);

      expect(results.crossOriginIsolated).toBe(true);
      expect(results.sharedArrayBuffer.ok).toBe(true);
      expect(results.sharedArrayBuffer.error).toBeNull();

      expect(results.websocket.ok).toBe(true);
      expect(results.websocket.error).toBeNull();

      expect(results.dnsQuery.ok).toBe(true);
      expect(results.dnsQuery.error).toBeNull();
    } finally {
      await gateway.stop();
      await echo.close();
      await dns.close();
    }
  });

  test('CROSS_ORIGIN_ISOLATION unset disables crossOriginIsolated (and reports why)', async ({ page }) => {
    const dns = await startUdpDnsServer();
    const echo = await startTcpEchoServer();
    const gateway = await startGatewayProcess({ crossOriginIsolation: false, dnsUpstreams: `127.0.0.1:${dns.port}` });
    try {
      await page.goto(`${gateway.baseUrl}/e2e?echoPort=${echo.port}`);
      await page.waitForFunction(() => Boolean(window.__aeroGatewayE2E), null, { timeout: 10_000 });
      const results = await page.evaluate(() => window.__aeroGatewayE2E!);

      expect(results.crossOriginIsolated).toBe(false);
      expect(results.sharedArrayBuffer.ok).toBe(false);
      expect(results.sharedArrayBuffer.error).not.toBeNull();

      expect(results.websocket.ok).toBe(true);
      expect(results.websocket.error).toBeNull();

      expect(results.dnsQuery.ok).toBe(true);
      expect(results.dnsQuery.error).toBeNull();
    } finally {
      await gateway.stop();
      await echo.close();
      await dns.close();
    }
  });
});
