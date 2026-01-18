import { expect, test } from '@playwright/test';

import dgram from 'node:dgram';
import { spawn, type ChildProcessWithoutNullStreams } from 'node:child_process';
import { once } from 'node:events';
import net from 'node:net';
import { fileURLToPath } from 'node:url';

import { unrefBestEffort } from '../../src/unref_safe.js';
import { L2_TUNNEL_SUBPROTOCOL } from '../../web/src/shared/l2TunnelProtocol.ts';
import { startRustL2Proxy } from '../../tools/rust_l2_proxy.js';

const REPO_ROOT = fileURLToPath(new URL('../..', import.meta.url));
const TS_STRIP_LOADER_URL = new URL('../../scripts/register-ts-strip-loader.mjs', import.meta.url);
const GATEWAY_ENTRY_PATH = fileURLToPath(new URL('../../backend/aero-gateway/src/index.ts', import.meta.url));

type UdpEchoServer = {
  port: number;
  close: () => Promise<void>;
};

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => {
    const timeout = setTimeout(resolve, ms);
    unrefBestEffort(timeout);
  });
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

async function waitForHttpOk(url: string, timeoutMs: number): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  let lastErr: unknown = null;

  while (Date.now() < deadline) {
    try {
      const res = await fetch(url);
      if (res.ok) return;
      lastErr = new Error(`unexpected status ${res.status} for ${url}`);
    } catch (err) {
      lastErr = err;
    }
    await sleep(200);
  }

  throw lastErr instanceof Error ? lastErr : new Error(`server failed to become ready: ${url}`);
}

async function killProcess(
  proc: ChildProcessWithoutNullStreams,
  opts: { killGroup?: boolean } = {},
): Promise<void> {
  if (proc.exitCode !== null) return;
  const pid = proc.pid;
  const killGroup = opts.killGroup && pid && process.platform !== 'win32';
  try {
    if (killGroup) {
      // Kill the entire process group (e.g. `cargo run --locked` + its spawned binary).
      process.kill(-pid, 'SIGTERM');
    } else {
      proc.kill('SIGTERM');
    }
  } catch {
    proc.kill('SIGTERM');
  }
  await Promise.race([
    once(proc, 'close'),
    sleep(5_000).then(() => {
      throw new Error(`timeout waiting for process ${proc.pid ?? 'unknown'} to exit`);
    }),
  ]).catch(() => {
    // Best-effort: ensure the process is gone so Playwright doesn't hang.
    if (proc.exitCode === null) {
      try {
        if (killGroup && pid) {
          process.kill(-pid, 'SIGKILL');
        } else {
          proc.kill('SIGKILL');
        }
      } catch {
        proc.kill('SIGKILL');
      }
    }
    return Promise.race([once(proc, 'close'), sleep(5_000)]);
  });
}

type ServerProcess = {
  origin: string;
  proc: ChildProcessWithoutNullStreams;
  close: () => Promise<void>;
};

async function startGateway(opts: {
  port: number;
  sessionSecret: string;
  allowedOrigin: string;
}): Promise<ServerProcess> {
  const proc = spawn(
    'node',
    [
      '--experimental-strip-types',
      '--import',
      TS_STRIP_LOADER_URL.href,
      GATEWAY_ENTRY_PATH,
    ],
    {
      stdio: ['ignore', 'pipe', 'pipe'],
      cwd: REPO_ROOT,
      env: {
        ...process.env,
        HOST: '127.0.0.1',
        PORT: String(opts.port),
        LOG_LEVEL: 'error',
        // Allow the Playwright test page origin to POST /session (CORS + cookies).
        ALLOWED_ORIGINS: opts.allowedOrigin,
        PUBLIC_BASE_URL: `http://127.0.0.1:${opts.port}`,
        SESSION_SECRET: opts.sessionSecret,
        RATE_LIMIT_REQUESTS_PER_MINUTE: '0',
        TLS_ENABLED: '0',
        TRUST_PROXY: '0',
      },
    },
  );
  proc.stdout.resume();
  proc.stderr.resume();

  const origin = `http://127.0.0.1:${opts.port}`;
  await waitForHttpOk(`${origin}/readyz`, 30_000);

  return {
    origin,
    proc,
    close: async () => {
      await killProcess(proc);
    },
  };
}

async function startL2Proxy(opts: {
  port: number;
  allowedOrigin: string;
  sessionSecret: string;
  udpEchoPort: number;
}): Promise<ServerProcess> {
  const proxy = await startRustL2Proxy({
    AERO_L2_PROXY_LISTEN_ADDR: `127.0.0.1:${opts.port}`,
    AERO_L2_ALLOWED_ORIGINS: '',
    ALLOWED_ORIGINS: opts.allowedOrigin,
    AERO_L2_ALLOWED_ORIGINS_EXTRA: '',
    AERO_L2_ALLOWED_HOSTS: '',
    AERO_L2_TRUST_PROXY_HOST: '',
    AERO_L2_AUTH_MODE: 'session',
    AERO_L2_API_KEY: '',
    AERO_L2_JWT_SECRET: '',
    AERO_L2_SESSION_SECRET: opts.sessionSecret,
    SESSION_SECRET: '',
    AERO_GATEWAY_SESSION_SECRET: '',
    AERO_L2_TOKEN: '',
    AERO_L2_OPEN: '0',
    AERO_L2_MAX_CONNECTIONS: '0',
    AERO_L2_MAX_BYTES_PER_CONNECTION: '0',
    AERO_L2_MAX_FRAMES_PER_SECOND: '0',
    AERO_L2_PING_INTERVAL_MS: '0',
    AERO_L2_ALLOWED_UDP_PORTS: String(opts.udpEchoPort),
    AERO_L2_UDP_FORWARD: `203.0.113.11:${opts.udpEchoPort}=127.0.0.1:${opts.udpEchoPort}`,
  });

  const origin = `http://127.0.0.1:${proxy.port}`;
  await waitForHttpOk(`${origin}/readyz`, 300_000);

  return {
    origin,
    proc: proxy.proc as ChildProcessWithoutNullStreams,
    close: async () => {
      await proxy.close();
    },
  };
}

test.describe.serial('l2 tunnel (session auth)', () => {
  test.skip(({ browserName }) => browserName !== 'chromium', 'l2 tunnel regression test runs on Chromium only');
  // This spec may need to compile `aero-l2-proxy` on first run. Give it ample time so CI isn't flaky on cold caches.
  test.describe.configure({ timeout: 600_000 });

  test('session-authenticated WebSocket /l2 negotiates subprotocol and relays frames', async ({ page }, testInfo) => {
    const sessionSecret = 'aero-e2e-session-secret';
    const webOrigin = testInfo.project.use.baseURL ?? 'http://127.0.0.1:5173';

    const udpEcho = await startUdpEchoServer();
    const gatewayPort = await getFreeTcpPort();
    const l2Port = await getFreeTcpPort();

    let gateway: ServerProcess | null = null;
    let l2Proxy: ServerProcess | null = null;

    try {
      gateway = await startGateway({ port: gatewayPort, sessionSecret, allowedOrigin: webOrigin });
      l2Proxy = await startL2Proxy({
        port: l2Port,
        allowedOrigin: webOrigin,
        sessionSecret,
        udpEchoPort: udpEcho.port,
      });

      await page.goto('/tests/e2e/fixtures/l2_tunnel.html', { waitUntil: 'load' });

      const result = await page.evaluate(
        async ({ gatewayOrigin, l2ProxyOrigin, udpEchoPort }) => {
          const { decodeL2Message, encodeL2Frame, L2_TUNNEL_SUBPROTOCOL, L2_TUNNEL_TYPE_FRAME } = await import(
            '/web/src/shared/l2TunnelProtocol.ts'
          );

          function withTimeout<T>(promise: Promise<T>, ms: number, label: string): Promise<T> {
            return Promise.race([
              promise,
              new Promise<T>((_, reject) => setTimeout(() => reject(new Error(`${label} timed out after ${ms}ms`)), ms)),
            ]);
          }

          function checksum16(buf: Uint8Array): number {
            let sum = 0;
            let i = 0;
            while (i + 1 < buf.length) {
              sum += (buf[i] << 8) | buf[i + 1];
              i += 2;
            }
            if (i < buf.length) sum += buf[i] << 8;
            while (sum >>> 16) sum = (sum & 0xffff) + (sum >>> 16);
            return (~sum) & 0xffff;
          }

          function buildUdpPacket(srcIp: Uint8Array, dstIp: Uint8Array, srcPort: number, dstPort: number, payload: Uint8Array) {
            const len = 8 + payload.length;
            const out = new Uint8Array(len);
            out[0] = (srcPort >>> 8) & 0xff;
            out[1] = srcPort & 0xff;
            out[2] = (dstPort >>> 8) & 0xff;
            out[3] = dstPort & 0xff;
            out[4] = (len >>> 8) & 0xff;
            out[5] = len & 0xff;
            out[6] = 0;
            out[7] = 0;
            out.set(payload, 8);

            const pseudo = new Uint8Array(12 + len + (len % 2));
            pseudo.set(srcIp, 0);
            pseudo.set(dstIp, 4);
            pseudo[8] = 0;
            pseudo[9] = 17; // UDP
            pseudo[10] = (len >>> 8) & 0xff;
            pseudo[11] = len & 0xff;
            pseudo.set(out, 12);

            let csum = checksum16(pseudo);
            if (csum === 0) csum = 0xffff;
            out[6] = (csum >>> 8) & 0xff;
            out[7] = csum & 0xff;
            return out;
          }

          function buildIpv4Packet(
            srcIp: Uint8Array,
            dstIp: Uint8Array,
            protocol: number,
            payload: Uint8Array,
            identification: number,
          ) {
            const headerLen = 20;
            const totalLen = headerLen + payload.length;
            const out = new Uint8Array(totalLen);
            out[0] = 0x45; // v4, ihl=5
            out[1] = 0;
            out[2] = (totalLen >>> 8) & 0xff;
            out[3] = totalLen & 0xff;
            out[4] = (identification >>> 8) & 0xff;
            out[5] = identification & 0xff;
            // DF flag.
            out[6] = 0x40;
            out[7] = 0x00;
            out[8] = 64; // ttl
            out[9] = protocol & 0xff;
            out[10] = 0;
            out[11] = 0;
            out.set(srcIp, 12);
            out.set(dstIp, 16);
            out.set(payload, 20);
            const csum = checksum16(out.subarray(0, 20));
            out[10] = (csum >>> 8) & 0xff;
            out[11] = csum & 0xff;
            return out;
          }

          function buildEthernetFrame(dstMac: Uint8Array, srcMac: Uint8Array, ethertype: number, payload: Uint8Array) {
            const out = new Uint8Array(14 + payload.length);
            out.set(dstMac, 0);
            out.set(srcMac, 6);
            out[12] = (ethertype >>> 8) & 0xff;
            out[13] = ethertype & 0xff;
            out.set(payload, 14);
            return out;
          }

          function buildDhcpDiscover(xid: number, mac: Uint8Array): Uint8Array {
            const out = new Uint8Array(240 + 4);
            out[0] = 1; // BOOTREQUEST
            out[1] = 1; // Ethernet
            out[2] = 6; // MAC len
            out[4] = (xid >>> 24) & 0xff;
            out[5] = (xid >>> 16) & 0xff;
            out[6] = (xid >>> 8) & 0xff;
            out[7] = xid & 0xff;
            // flags: broadcast
            out[10] = 0x80;
            out[11] = 0x00;
            out.set(mac, 28);
            // magic cookie
            out[236] = 99;
            out[237] = 130;
            out[238] = 83;
            out[239] = 99;
            // options: message type = discover
            out[240] = 53;
            out[241] = 1;
            out[242] = 1;
            out[243] = 255;
            return out;
          }

          function buildDhcpRequest(xid: number, mac: Uint8Array, requestedIp: Uint8Array, serverId: Uint8Array): Uint8Array {
            const optionsLen = 3 + 6 + 6 + 1; // 53 + 50 + 54 + end
            const out = new Uint8Array(240 + optionsLen);
            out[0] = 1;
            out[1] = 1;
            out[2] = 6;
            out[4] = (xid >>> 24) & 0xff;
            out[5] = (xid >>> 16) & 0xff;
            out[6] = (xid >>> 8) & 0xff;
            out[7] = xid & 0xff;
            out[10] = 0x80;
            out[11] = 0x00;
            out.set(mac, 28);
            out[236] = 99;
            out[237] = 130;
            out[238] = 83;
            out[239] = 99;

            let o = 240;
            out[o++] = 53;
            out[o++] = 1;
            out[o++] = 3; // DHCPREQUEST
            out[o++] = 50;
            out[o++] = 4;
            out.set(requestedIp, o);
            o += 4;
            out[o++] = 54;
            out[o++] = 4;
            out.set(serverId, o);
            o += 4;
            out[o++] = 255;
            return out;
          }

          function parseDhcp(frame: Uint8Array): { type: number | null; yiaddr: Uint8Array | null; serverId: Uint8Array | null } {
            // Ethernet
            if (frame.length < 14) return { type: null, yiaddr: null, serverId: null };
            const ethertype = (frame[12] << 8) | frame[13];
            if (ethertype !== 0x0800) return { type: null, yiaddr: null, serverId: null };
            const ip = frame.subarray(14);
            if (ip.length < 20) return { type: null, yiaddr: null, serverId: null };
            const ihl = (ip[0] & 0x0f) * 4;
            if (ip[9] !== 17 || ip.length < ihl + 8) return { type: null, yiaddr: null, serverId: null };
            const udp = ip.subarray(ihl);
            const srcPort = (udp[0] << 8) | udp[1];
            const dstPort = (udp[2] << 8) | udp[3];
            if (srcPort !== 67 || dstPort !== 68) return { type: null, yiaddr: null, serverId: null };
            const udpLen = (udp[4] << 8) | udp[5];
            if (udpLen < 8 || udp.length < udpLen) return { type: null, yiaddr: null, serverId: null };
            const dhcp = udp.subarray(8, udpLen);
            if (dhcp.length < 240) return { type: null, yiaddr: null, serverId: null };
            const yiaddr = dhcp.subarray(16, 20);
            // options after 240
            let msgType: number | null = null;
            let serverId: Uint8Array | null = null;
            let i = 240;
            while (i < dhcp.length) {
              const code = dhcp[i++];
              if (code === 0) continue;
              if (code === 255) break;
              if (i >= dhcp.length) break;
              const len = dhcp[i++];
              if (i + len > dhcp.length) break;
              if (code === 53 && len === 1) msgType = dhcp[i]!;
              if (code === 54 && len === 4) serverId = dhcp.subarray(i, i + 4);
              i += len;
            }
            return { type: msgType, yiaddr: yiaddr.slice(), serverId: serverId ? serverId.slice() : null };
          }

          function parseArpReply(frame: Uint8Array): { senderMac: Uint8Array; senderIp: Uint8Array } | null {
            if (frame.length < 14 + 28) return null;
            const ethertype = (frame[12] << 8) | frame[13];
            if (ethertype !== 0x0806) return null;
            const arp = frame.subarray(14);
            const opcode = (arp[6] << 8) | arp[7];
            if (opcode !== 2) return null; // reply
            const senderMac = arp.subarray(8, 14);
            const senderIp = arp.subarray(14, 18);
            return { senderMac: senderMac.slice(), senderIp: senderIp.slice() };
          }

          function parseUdp(frame: Uint8Array): { srcIp: Uint8Array; dstIp: Uint8Array; srcPort: number; dstPort: number; payload: Uint8Array } | null {
            if (frame.length < 14 + 20 + 8) return null;
            const ethertype = (frame[12] << 8) | frame[13];
            if (ethertype !== 0x0800) return null;
            const ip = frame.subarray(14);
            const ihl = (ip[0] & 0x0f) * 4;
            if (ip.length < ihl + 8) return null;
            if (ip[9] !== 17) return null;
            const totalLen = (ip[2] << 8) | ip[3];
            if (totalLen < ihl + 8 || ip.length < totalLen) return null;
            const srcIp = ip.subarray(12, 16);
            const dstIp = ip.subarray(16, 20);
            const udp = ip.subarray(ihl, totalLen);
            const udpLen = (udp[4] << 8) | udp[5];
            if (udpLen < 8 || udp.length < udpLen) return null;
            const srcPort = (udp[0] << 8) | udp[1];
            const dstPort = (udp[2] << 8) | udp[3];
            const payload = udp.subarray(8, udpLen);
            return { srcIp: srcIp.slice(), dstIp: dstIp.slice(), srcPort, dstPort, payload: payload.slice() };
          }

          const wsBase = new URL(l2ProxyOrigin);
          wsBase.protocol = wsBase.protocol === 'https:' ? 'wss:' : 'ws:';
          wsBase.search = '';

          // Use the gateway as the canonical endpoint discovery API: even though
          // this test connects directly to the L2 proxy (different port), the
          // advertised L2 path should stay in sync.
          const discoveryRes = await withTimeout(
            fetch(`${gatewayOrigin}/session`, {
              method: 'POST',
              // Do NOT store cookies yet; we want the initial WS upgrade to be unauthenticated.
              credentials: 'omit',
              headers: { 'content-type': 'application/json' },
              body: '{}',
            }),
            10_000,
            'fetch /session (discovery)',
          );
          if (!discoveryRes.ok) throw new Error(`session discovery endpoint failed: ${discoveryRes.status}`);
          const discoveryJson = await discoveryRes.json().catch(() => null);
          const l2Path = discoveryJson?.endpoints?.l2;
          if (typeof l2Path !== 'string' || l2Path.length === 0) {
            throw new Error('session response missing endpoints.l2');
          }

          wsBase.pathname = l2Path;
          const wsUrl = wsBase.toString();

          // First, prove that the proxy rejects WebSocket upgrades without a session cookie.
          const unauth = await withTimeout(
            new Promise<{ opened: boolean; code?: number; reason?: string }>((resolve) => {
              const ws = new WebSocket(wsUrl, L2_TUNNEL_SUBPROTOCOL);
              ws.binaryType = 'arraybuffer';
              let opened = false;
              ws.onopen = () => {
                opened = true;
                ws.close(1000, 'unexpected');
              };
              ws.onerror = () => {
                // `WebSocket` only exposes an error event (no HTTP status). We assert via `opened` below.
              };
              ws.onclose = (evt) => resolve({ opened, code: evt.code, reason: evt.reason });
            }),
            10_000,
            'WebSocket without cookie',
          );
          if (unauth.opened) {
            throw new Error('expected WebSocket /l2 to be rejected without session cookie');
          }

          // Establish a session cookie via the gateway, then retry the WebSocket upgrade.
          const sessionRes = await withTimeout(
            fetch(`${gatewayOrigin}/session`, {
              method: 'POST',
              credentials: 'include',
              headers: { 'content-type': 'application/json' },
              body: '{}',
            }),
            10_000,
            'fetch /session (cookie)',
          );
          if (!sessionRes.ok) throw new Error(`session cookie endpoint failed: ${sessionRes.status}`);

          const ws = new WebSocket(wsBase.toString(), L2_TUNNEL_SUBPROTOCOL);
          ws.binaryType = 'arraybuffer';

          await withTimeout(
            new Promise<void>((resolve, reject) => {
              ws.onopen = () => resolve();
              ws.onerror = () => reject(new Error('websocket error'));
              ws.onclose = (evt) => reject(new Error(`websocket closed: ${evt.code} ${evt.reason}`));
            }),
            10_000,
            'WebSocket open',
          );

          const negotiatedProtocol = ws.protocol;
          if (negotiatedProtocol !== L2_TUNNEL_SUBPROTOCOL) {
            throw new Error(`subprotocol not negotiated: ${negotiatedProtocol || 'none'}`);
          }

          async function waitForL2Frame(pred: (frame: Uint8Array) => boolean, timeoutMs = 10_000): Promise<Uint8Array> {
            return await withTimeout(
              new Promise<Uint8Array>((resolve, reject) => {
                const onMessage = (evt: MessageEvent) => {
                  if (!(evt.data instanceof ArrayBuffer)) return;
                  let decoded;
                  try {
                    decoded = decodeL2Message(new Uint8Array(evt.data));
                  } catch {
                    return;
                  }
                  if (decoded.type !== L2_TUNNEL_TYPE_FRAME) return;
                  const frame = decoded.payload;
                  if (!pred(frame)) return;
                  ws.removeEventListener('message', onMessage);
                  ws.removeEventListener('close', onClose);
                  resolve(frame);
                };
                const onClose = (evt: CloseEvent) => {
                  ws.removeEventListener('message', onMessage);
                  ws.removeEventListener('close', onClose);
                  reject(new Error(`websocket closed: ${evt.code} ${evt.reason}`));
                };
                ws.addEventListener('message', onMessage);
                ws.addEventListener('close', onClose);
              }),
              timeoutMs,
              'wait for l2 frame',
            );
          }

          const guestMac = new Uint8Array([0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee]);
          const broadcastMac = new Uint8Array([0xff, 0xff, 0xff, 0xff, 0xff, 0xff]);
          const ipUnspecified = new Uint8Array([0, 0, 0, 0]);
          const ipBroadcast = new Uint8Array([255, 255, 255, 255]);
          const xid = 0x1020_3040;

          // DHCP discover -> offer.
          const dhcpDiscover = buildDhcpDiscover(xid, guestMac);
          const dhcpDiscoverUdp = buildUdpPacket(ipUnspecified, ipBroadcast, 68, 67, dhcpDiscover);
          const dhcpDiscoverIp = buildIpv4Packet(ipUnspecified, ipBroadcast, 17, dhcpDiscoverUdp, 1);
          const dhcpDiscoverEth = buildEthernetFrame(broadcastMac, guestMac, 0x0800, dhcpDiscoverIp);
          ws.send(encodeL2Frame(dhcpDiscoverEth));

          const offerFrame = await waitForL2Frame((f) => parseDhcp(f).type === 2);
          const offer = parseDhcp(offerFrame);
          if (!offer.yiaddr || !offer.serverId) {
            throw new Error('failed to parse DHCP offer');
          }

          // DHCP request -> ack.
          const dhcpReq = buildDhcpRequest(xid, guestMac, offer.yiaddr, offer.serverId);
          const dhcpReqUdp = buildUdpPacket(ipUnspecified, ipBroadcast, 68, 67, dhcpReq);
          const dhcpReqIp = buildIpv4Packet(ipUnspecified, ipBroadcast, 17, dhcpReqUdp, 2);
          const dhcpReqEth = buildEthernetFrame(broadcastMac, guestMac, 0x0800, dhcpReqIp);
          ws.send(encodeL2Frame(dhcpReqEth));

          await waitForL2Frame((f) => parseDhcp(f).type === 5);

          const guestIp = offer.yiaddr;
          const gatewayIp = offer.serverId;

          // ARP for gateway MAC.
          const arpReq = new Uint8Array(28);
          arpReq[0] = 0x00;
          arpReq[1] = 0x01; // ethernet
          arpReq[2] = 0x08;
          arpReq[3] = 0x00; // ipv4
          arpReq[4] = 6;
          arpReq[5] = 4;
          arpReq[6] = 0x00;
          arpReq[7] = 0x01; // request
          arpReq.set(guestMac, 8);
          arpReq.set(guestIp, 14);
          arpReq.set(new Uint8Array(6), 18);
          arpReq.set(gatewayIp, 24);
          const arpEth = buildEthernetFrame(broadcastMac, guestMac, 0x0806, arpReq);
          ws.send(encodeL2Frame(arpEth));

          const arpReplyFrame = await waitForL2Frame((f) => {
            const arp = parseArpReply(f);
            if (!arp) return false;
            return arp.senderIp[0] === gatewayIp[0] && arp.senderIp[1] === gatewayIp[1] && arp.senderIp[2] === gatewayIp[2] && arp.senderIp[3] === gatewayIp[3];
          });
          const arp = parseArpReply(arpReplyFrame);
          if (!arp) throw new Error('failed to parse ARP reply');
          const gatewayMac = arp.senderMac;

          // UDP echo probe via forward map (203.0.113.11 -> 127.0.0.1).
          const remoteIp = new Uint8Array([203, 0, 113, 11]);
          const guestPort = 50_000;
          const payload = new TextEncoder().encode('hi-udp');
          const udpPkt = buildUdpPacket(guestIp, remoteIp, guestPort, udpEchoPort, payload);
          const ipPkt = buildIpv4Packet(guestIp, remoteIp, 17, udpPkt, 3);
          const ethPkt = buildEthernetFrame(gatewayMac, guestMac, 0x0800, ipPkt);
          ws.send(encodeL2Frame(ethPkt));

          const udpRespFrame = await waitForL2Frame((f) => {
            const udp = parseUdp(f);
            if (!udp) return false;
            return udp.srcPort === udpEchoPort && udp.dstPort === guestPort;
          });
          const udpResp = parseUdp(udpRespFrame);
          if (!udpResp) throw new Error('failed to parse UDP response frame');

          const echoed = new TextDecoder().decode(udpResp.payload);
          if (echoed !== 'hi-udp') {
            throw new Error(`unexpected udp echo payload: ${JSON.stringify(echoed)}`);
          }

          ws.close(1000, 'done');
          return { negotiatedProtocol, echoed };
        },
        { gatewayOrigin: gateway!.origin, l2ProxyOrigin: l2Proxy!.origin, udpEchoPort: udpEcho.port },
      );

      expect(result.negotiatedProtocol).toBe(L2_TUNNEL_SUBPROTOCOL);
      expect(result.echoed).toBe('hi-udp');
    } finally {
      if (l2Proxy) await l2Proxy.close();
      if (gateway) await gateway.close();
      await udpEcho.close();
    }
  });
});
