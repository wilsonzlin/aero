import { expect, test, type Page } from '@playwright/test';

import dgram from 'node:dgram';
import { spawn, spawnSync, type ChildProcessWithoutNullStreams } from 'node:child_process';
import crypto from 'node:crypto';
import { once } from 'node:events';
import fs from 'node:fs/promises';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';

type UdpEchoServer = {
  port: number;
  close: () => Promise<void>;
};

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => {
    const timeout = setTimeout(resolve, ms);
    timeout.unref?.();
  });
}

function waitForProcessClose(proc: ChildProcessWithoutNullStreams): Promise<void> {
  if (proc.exitCode !== null) return Promise.resolve();
  return new Promise((resolve) => {
    const onDone = () => {
      cleanup();
      resolve();
    };
    const cleanup = () => {
      proc.off('exit', onDone);
      proc.off('close', onDone);
    };
    proc.once('exit', onDone);
    proc.once('close', onDone);
    // Handle races where the process exits between the first `exitCode` check and
    // installing the event listeners.
    if (proc.exitCode !== null) onDone();
  });
}

async function stopProcess(proc: ChildProcessWithoutNullStreams, timeoutMs = 5_000): Promise<void> {
  if (proc.exitCode !== null) return;

  try {
    proc.kill('SIGTERM');
  } catch {
    // ignore
  }

  try {
    await Promise.race([
      waitForProcessClose(proc),
      sleep(timeoutMs).then(() => {
        throw new Error('timeout');
      }),
    ]);
    return;
  } catch {
    // fall through
  }

  try {
    proc.kill('SIGKILL');
  } catch {
    // ignore
  }

  await Promise.race([waitForProcessClose(proc), sleep(timeoutMs)]);
}

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

async function startUdpEchoServer6(): Promise<UdpEchoServer | null> {
  const sock = dgram.createSocket('udp6');
  sock.on('message', (msg, rinfo) => {
    sock.send(msg, rinfo.port, rinfo.address);
  });

  const bound = await new Promise<boolean>((resolve) => {
    const onError = () => resolve(false);
    sock.once('error', onError);
    sock.bind(0, '::1', () => {
      sock.off('error', onError);
      resolve(true);
    });
  });
  if (!bound) {
    sock.close();
    await once(sock, 'close');
    return null;
  }

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
    await sleep(200);
  }

  throw lastErr instanceof Error ? lastErr : new Error('relay failed to become ready');
}

type RelayProcess = {
  origin: string;
  proc: ChildProcessWithoutNullStreams;
  close: () => Promise<void>;
};

type RelayAuthConfig =
  | { authMode: 'none' }
  | { authMode: 'api_key'; apiKey: string }
  | { authMode: 'jwt'; jwtSecret: string; token: string };

function makeJWT(secret: string): string {
  const header = Buffer.from(JSON.stringify({ alg: 'HS256', typ: 'JWT' })).toString('base64url');
  const now = Math.floor(Date.now() / 1000);
  const payload = Buffer.from(
    JSON.stringify({
      sid: crypto.randomUUID(),
      iat: now,
      exp: now + 60,
    }),
  ).toString('base64url');
  const unsigned = `${header}.${payload}`;
  const sig = crypto.createHmac('sha256', secret).update(unsigned).digest('base64url');
  return `${unsigned}.${sig}`;
}

async function buildRelayBinary(): Promise<{ tmpDir: string; binPath: string }> {
  const tmpDir = await fs.mkdtemp(path.join(os.tmpdir(), 'aero-webrtc-udp-relay-e2e-'));
  const binPath = path.join(tmpDir, 'aero-webrtc-udp-relay');

  const build = spawnSync('go', ['build', '-o', binPath, './cmd/aero-webrtc-udp-relay'], {
    cwd: 'proxy/webrtc-udp-relay',
    stdio: 'inherit',
  });
  if (build.status !== 0) {
    await fs.rm(tmpDir, { recursive: true, force: true });
    throw new Error(`failed to build aero-webrtc-udp-relay (exit ${build.status ?? 'unknown'})`);
  }
  return { tmpDir, binPath };
}

test.describe.serial('udp relay (webrtc)', () => {
  test.skip(({ browserName }) => browserName !== 'chromium', 'WebRTC UDP relay test is Chromium-only');
  test.describe.configure({ timeout: 60_000 });

  let echo: UdpEchoServer;
  let relayBinPath: string;
  let relayTmpDir: string;

  test.beforeAll(async () => {
    echo = await startUdpEchoServer();
    const relayBuild = await buildRelayBinary();
    relayBinPath = relayBuild.binPath;
    relayTmpDir = relayBuild.tmpDir;
  });

  test.afterAll(async () => {
    await echo.close();
    if (relayTmpDir) {
      await fs.rm(relayTmpDir, { recursive: true, force: true });
    }
  });

  async function startRelay(auth: RelayAuthConfig): Promise<RelayProcess> {
    const port = await getFreeTcpPort();

    const proc = spawn(relayBinPath, ['--listen-addr', `127.0.0.1:${port}`], {
      cwd: 'proxy/webrtc-udp-relay',
      stdio: ['ignore', 'pipe', 'pipe'],
      env: {
        ...process.env,
        // Allow the Vite dev server origin to fetch /webrtc/ice and connect to WS.
        ALLOWED_ORIGINS: '*',
        // Allow loopback destinations for local UDP echo tests.
        DESTINATION_POLICY_PRESET: 'dev',
        // Keep tests explicit about auth requirements.
        AUTH_MODE: auth.authMode,
        ...(auth.authMode === 'api_key' ? { API_KEY: auth.apiKey } : {}),
        ...(auth.authMode === 'jwt' ? { JWT_SECRET: auth.jwtSecret } : {}),
        AERO_WEBRTC_UDP_RELAY_LOG_LEVEL: 'error',
      },
    });
    // Drain output to avoid the child process blocking on a full pipe buffer.
    proc.stdout.resume();
    proc.stderr.resume();

    const origin = `http://127.0.0.1:${port}`;
    await waitForReady(origin, 30_000);

    return {
      origin,
      proc,
      close: async () => {
        await stopProcess(proc);
      },
    };
  }

  async function runRoundTrip(
    page: Page,
    relayOrigin: string,
    echoPort: number,
    authToken?: string,
    dstIp: string = '127.0.0.1',
    mode?: 'ws-trickle' | 'http-offer' | 'legacy-offer',
  ) {
    return await page.evaluate(
      async ({ relayOrigin, echoPort, authToken, dstIp, mode }) => {
        const { connectUdpRelay } = await import('/web/src/net/udpRelaySignalingClient.ts');

        const payload = new Uint8Array([1, 2, 3, 4, 5]);
        const guestPort = 45_000;

        let resolveEvent: ((evt: unknown) => void) | null = null;
        const eventPromise = new Promise((resolve) => {
          resolveEvent = resolve as (evt: unknown) => void;
        });

        const conn = await connectUdpRelay({
          baseUrl: relayOrigin,
          authToken,
          mode,
          sink: (evt) => resolveEvent?.(evt),
        });

        try {
          conn.udp.send(guestPort, dstIp, echoPort, payload);

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
      { relayOrigin, echoPort, authToken, dstIp, mode },
    );
  }

  async function runRoundTripWebSocket(
    page: Page,
    relayOrigin: string,
    echoPort: number,
    authToken?: string,
    dstIp: string = '127.0.0.1',
  ) {
    return await page.evaluate(
      async ({ relayOrigin, echoPort, authToken, dstIp }) => {
        const { WebSocketUdpProxyClient } = await import('/web/src/net/udpProxy.ts');

        const payload = new Uint8Array([1, 2, 3, 4, 5]);
        const guestPort = 45_000;

        let resolveEvent: ((evt: unknown) => void) | null = null;
        const eventPromise = new Promise((resolve) => {
          resolveEvent = resolve as (evt: unknown) => void;
        });

        const udp = new WebSocketUdpProxyClient(relayOrigin, (evt) => resolveEvent?.(evt), authToken);
        try {
          await udp.connect();
          udp.send(guestPort, dstIp, echoPort, payload);

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
          udp.close();
        }
      },
      { relayOrigin, echoPort, authToken, dstIp },
    );
  }

  function expectEcho(result: { srcIp: string; srcPort: number; dstPort: number; data: number[] }, srcIp: string, srcPort: number) {
    expect(result.srcIp).toBe(srcIp);
    expect(result.srcPort).toBe(srcPort);
    expect(result.dstPort).toBe(45_000);
    expect(result.data).toEqual([1, 2, 3, 4, 5]);
  }

  test('connectUdpRelay authenticates with api_key (query-string fallback)', async ({ page }) => {
    const apiKey = 'secret';
    const relay = await startRelay({ authMode: 'api_key', apiKey });
    try {
      await page.goto('/', { waitUntil: 'load' });
      expectEcho(await runRoundTrip(page, relay.origin, echo.port, apiKey), '127.0.0.1', echo.port);
      expectEcho(await runRoundTrip(page, relay.origin, echo.port, apiKey, '127.0.0.1', 'http-offer'), '127.0.0.1', echo.port);
      expectEcho(await runRoundTripWebSocket(page, relay.origin, echo.port, apiKey), '127.0.0.1', echo.port);
    } finally {
      await relay.close();
    }
  });

  test('connectUdpRelay authenticates with jwt', async ({ page }) => {
    const jwtSecret = 'supersecret';
    const token = makeJWT(jwtSecret);
    const relay = await startRelay({ authMode: 'jwt', jwtSecret, token });
    try {
      await page.goto('/', { waitUntil: 'load' });
      expectEcho(await runRoundTrip(page, relay.origin, echo.port, token), '127.0.0.1', echo.port);
      expectEcho(await runRoundTrip(page, relay.origin, echo.port, token, '127.0.0.1', 'http-offer'), '127.0.0.1', echo.port);
      expectEcho(await runRoundTripWebSocket(page, relay.origin, echo.port, token), '127.0.0.1', echo.port);
    } finally {
      await relay.close();
    }
  });

  test('connectUdpRelay establishes DataChannel and relays UDP', async ({ page }) => {
    const relay = await startRelay({ authMode: 'none' });
    await page.goto('/', { waitUntil: 'load' });

    try {
      expectEcho(await runRoundTrip(page, relay.origin, echo.port), '127.0.0.1', echo.port);
      expectEcho(await runRoundTrip(page, relay.origin, echo.port, undefined, '127.0.0.1', 'http-offer'), '127.0.0.1', echo.port);
      expectEcho(await runRoundTrip(page, relay.origin, echo.port, undefined, '127.0.0.1', 'legacy-offer'), '127.0.0.1', echo.port);
      expectEcho(await runRoundTripWebSocket(page, relay.origin, echo.port), '127.0.0.1', echo.port);
    } finally {
      await relay.close();
    }
  });

  test('relays an IPv6 datagram via v2 framing', async ({ page }) => {
    const echo6 = await startUdpEchoServer6();
    if (!echo6) {
      test.skip(true, 'ipv6 not supported in test environment');
      return;
    }

    const relay = await startRelay({ authMode: 'none' });
    try {
      await page.goto('/', { waitUntil: 'load' });
      const expectedIp = '0000:0000:0000:0000:0000:0000:0000:0001';
      expectEcho(await runRoundTrip(page, relay.origin, echo6.port, undefined, '::1'), expectedIp, echo6.port);
      expectEcho(await runRoundTrip(page, relay.origin, echo6.port, undefined, '::1', 'legacy-offer'), expectedIp, echo6.port);
      expectEcho(await runRoundTripWebSocket(page, relay.origin, echo6.port, undefined, '::1'), expectedIp, echo6.port);
    } finally {
      await relay.close();
      await echo6.close();
    }
  });
});
