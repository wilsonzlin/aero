import { expect, test } from '@playwright/test';

import dgram from 'node:dgram';
import { spawn, type ChildProcessWithoutNullStreams } from 'node:child_process';
import { once } from 'node:events';
import net from 'node:net';

type UdpEchoServer = {
  port: number;
  close: () => Promise<void>;
};

async function startUdpEchoServer(): Promise<UdpEchoServer> {
  const sock = dgram.createSocket('udp4');
  sock.on('message', (msg, rinfo) => {
    sock.send(msg, rinfo.port, rinfo.address);
  });

  sock.bind(0, '127.0.0.1');
  await once(sock, 'listening');

  const addr = sock.address();
  if (typeof addr === 'string') throw new Error('unexpected udp address');

  return {
    port: addr.port,
    close: async () => {
      sock.close();
      await once(sock, 'close');
    },
  };
}

async function getFreeTcpPort(): Promise<number> {
  const srv = net.createServer();
  srv.listen(0, '127.0.0.1');
  await once(srv, 'listening');
  const addr = srv.address();
  if (!addr || typeof addr === 'string') throw new Error('unexpected server address');
  const port = addr.port;
  await new Promise<void>((resolve, reject) => srv.close((err) => (err ? reject(err) : resolve())));
  return port;
}

async function waitForReady(origin: string, timeoutMs: number): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  let lastErr: unknown = null;

  while (Date.now() < deadline) {
    try {
      const res = await fetch(`${origin}/readyz`);
      if (res.ok) {
        const body: unknown = await res.json();
        if (typeof body === 'object' && body !== null && (body as { ready?: unknown }).ready === true) return;
      }
      lastErr = new Error(`readyz status ${res.status}`);
    } catch (err) {
      lastErr = err;
    }
    await new Promise((resolve) => setTimeout(resolve, 200));
  }

  throw lastErr instanceof Error ? lastErr : new Error('relay failed to become ready');
}

type RelayProcess = {
  origin: string;
  proc: ChildProcessWithoutNullStreams;
  close: () => Promise<void>;
};

async function startRelay(): Promise<RelayProcess> {
  const port = await getFreeTcpPort();

  const proc = spawn(
    'go',
    ['run', './cmd/aero-webrtc-udp-relay', '--listen-addr', `127.0.0.1:${port}`],
    {
      cwd: 'proxy/webrtc-udp-relay',
      stdio: ['ignore', 'pipe', 'pipe'],
      env: {
        ...process.env,
        // Allow the Vite dev server origin to fetch /webrtc/ice and connect to WS.
        ALLOWED_ORIGINS: '*',
        // Default in relay may require auth; keep tests explicit.
        AUTH_MODE: 'none',
        AERO_WEBRTC_UDP_RELAY_LOG_LEVEL: 'error',
      },
    },
  );
  // Drain output to avoid the child process blocking on a full pipe buffer.
  proc.stdout.resume();
  proc.stderr.resume();

  const origin = `http://127.0.0.1:${port}`;
  await waitForReady(origin, 30_000);

  return {
    origin,
    proc,
    close: async () => {
      if (proc.exitCode === null) {
        proc.kill('SIGTERM');
        await once(proc, 'exit');
      }
    },
  };
}

test.describe('udp relay (webrtc)', () => {
  test.skip(({ browserName }) => browserName !== 'chromium', 'WebRTC UDP relay test is Chromium-only');
  test.describe.configure({ timeout: 60_000 });

  let relay: RelayProcess;
  let echo: UdpEchoServer;

  test.beforeAll(async () => {
    echo = await startUdpEchoServer();
    relay = await startRelay();
  });

  test.afterAll(async () => {
    await echo.close();
    await relay.close();
  });

  test('connectUdpRelay establishes DataChannel and relays UDP', async ({ page }) => {
    await page.goto('http://127.0.0.1:5173/', { waitUntil: 'load' });

    const result = await page.evaluate(
      async ({ relayOrigin, echoPort }) => {
        const { connectUdpRelay } = await import('/web/src/net/udpRelaySignalingClient.ts');

        const payload = new Uint8Array([1, 2, 3, 4, 5]);
        const guestPort = 45_000;

        let resolveEvent: ((evt: unknown) => void) | null = null;
        const eventPromise = new Promise((resolve) => {
          resolveEvent = resolve as (evt: unknown) => void;
        });

        const conn = await connectUdpRelay({
          baseUrl: relayOrigin,
          sink: (evt) => resolveEvent?.(evt),
        });

        try {
          conn.udp.send(guestPort, '127.0.0.1', echoPort, payload);

          const evt = (await Promise.race([
            eventPromise,
            new Promise((_, reject) => setTimeout(() => reject(new Error('timeout waiting for udp echo')), 10_000)),
          ])) as { srcIp: string; srcPort: number; dstPort: number; data: Uint8Array };

          return {
            srcIp: evt.srcIp,
            srcPort: evt.srcPort,
            dstPort: evt.dstPort,
            data: Array.from(evt.data),
          };
        } finally {
          conn.close();
        }
      },
      { relayOrigin: relay.origin, echoPort: echo.port },
    );

    expect(result.srcIp).toBe('127.0.0.1');
    expect(result.srcPort).toBe(echo.port);
    expect(result.dstPort).toBe(45_000);
    expect(result.data).toEqual([1, 2, 3, 4, 5]);
  });
});
