import { test, expect } from '@playwright/test';

import { spawn, type ChildProcess } from 'node:child_process';
import http from 'node:http';
import https from 'node:https';
import * as dgram from 'node:dgram';
import net, { type AddressInfo } from 'node:net';
import { pipeline } from 'node:stream';
import { setTimeout as delay } from 'node:timers/promises';

const TLS_KEY = `-----BEGIN PRIVATE KEY-----
MIIEvgIBADANBgkqhkiG9w0BAQEFAASCBKgwggSkAgEAAoIBAQDdSPoccw61QJnc
bwkEWea0JYDyJH2LaI+oAloHaRKa3DBTf0qBz/p074eh2+JY1nz0+4MU2meDrW6f
kDHpZ+L60fWrNW0c26Ct+++qj1Eb7ZM5d2EZHXk/7x+bMsZWoKf048VgQIC3I0Xv
wdGyXBTeMYJBRtZAoh+wUiPBCYl3/DFvIb/g6v4tdq9m13AQ+Oc5rugnDgqF8r5A
Pa4gsJeOVs0D/2SZWF1ePjA2bv0CH2M+2cc5FMiMbdsOU1tuekRkH7bVW11tdKHH
3N4q65zERoly4UII1y36jkop227mP8qdZDfc4n1HVnZ/NBgj66wM6WECTLxh13u0
ZFLJJGLbAgMBAAECggEAElKRFxr1zE9Bji2JdxlEj4UNdL9Nv+XUA0rSjouGNVln
DPrcvfvtFpKgzeepicaUySosM+VTreUF5GNpppRqCG+rIlaFpt6OoulZ8mr0gdX9
m0QFv7EfkYoouU6OeqzJy26ysKIWplNe3pfTV6vlNHKwANyvL+HcstpSSJEUF2Gc
d/kWTCtQ3+KK+Gr2zTyvu6MMMVsLLUz+/EaeowugbZ/JprhyWKMUmO6QaKiRbaef
Hnk19Bhuow6EkDwjKiCiA7t8b6gb7TgkLFvulTrdJC8ciD320YwNUawPL/OoseHa
/k4nHMbzZa2KxgSRR7EIbZZ6kZ2EPMsQZ2J4CUoHWQKBgQD40C5QF29krwdF74uc
ydnWDWR4MdmUE1xdUOnrn4UnxHuRW2su+8xDHuB2JApQ91U5FhHk3Qc0x339QGNu
T/EVXC8aqBwAMSNBdnhjCrbtqMyNyy8nrFvZWImZzepbR6OPiFa1fd94L9HznIY/
8tUSUFq+3+BKmovf/AYnea4NOQKBgQDjrT4adRRsgI/gNaduN7IFe1IciCWNtnEY
n/73GRBJxseeU5ZCKQa5p3/RpjhSlgzz7RZ1iK/T58rsCiXUVuWkSwYjTiyVkl0O
rhaSnJx0FojhYo9CvkiTXcKySvcz3MMcDnb/mlUfDl5NFBpz9E+bYcuNufje1aVs
ARJojl5EswKBgQDRjaQz2Ej9J1yczi9rkaVh3k2r3XA+gj/cZ/VbeTKQV68qsTAI
lhFmxm6NkbUOlAC235uagX08Ongl/0C++500PDt/2+4ZS0lCLSEfaTq/1tbQ5TuF
0mhZGXRqkT68Og3LKSy+FpFLjBrrbfyzhzVlA0AqWitxKdB8iKo2PQkWIQKBgQCU
NWRmCK0g7Je8FnFFmE/0rZCILkBz/b2lkBGDfPdTb2jmsfbwXpCYLmdQbGnhqPgJ
md6y6CW9RficqwZxMZgP2R7HwM3ZGAwn0D+1dOmL0FeOkIA9rGzGMZTaR16gjicc
jnX8cdTTgKD2gA2wSevAdGrzeYp+VIl4w0Heej73bQKBgG8ZNCn+4ueVwzbZDF9x
1Q3Q/vnM5uK5h4cHUl2sV35TzG9dfQFA8J4o5iYAA0wSFvRtd8GIMCoHL0eMIKMJ
p79rneark8FrU1K079VY4g+jezj3wmVJlOq1ANqPmSsDWvnn9ClePY3RvuCOrW6R
VTpOtsdYdd3Mqr7nELBtUC/s
-----END PRIVATE KEY-----`;

const TLS_CERT = `-----BEGIN CERTIFICATE-----
MIIDCTCCAfGgAwIBAgIUVyaUNGALt2pLMZ64fhtoHdgyalIwDQYJKoZIhvcNAQEL
BQAwFDESMBAGA1UEAwwJbG9jYWxob3N0MB4XDTI2MDExMDEyMDYyMFoXDTM2MDEw
ODEyMDYyMFowFDESMBAGA1UEAwwJbG9jYWxob3N0MIIBIjANBgkqhkiG9w0BAQEF
AAOCAQ8AMIIBCgKCAQEA3Uj6HHMOtUCZ3G8JBFnmtCWA8iR9i2iPqAJaB2kSmtww
U39Kgc/6dO+HodviWNZ89PuDFNpng61un5Ax6Wfi+tH1qzVtHNugrfvvqo9RG+2T
OXdhGR15P+8fmzLGVqCn9OPFYECAtyNF78HRslwU3jGCQUbWQKIfsFIjwQmJd/wx
byG/4Or+LXavZtdwEPjnOa7oJw4KhfK+QD2uILCXjlbNA/9kmVhdXj4wNm79Ah9j
PtnHORTIjG3bDlNbbnpEZB+21VtdbXShx9zeKuucxEaJcuFCCNct+o5KKdtu5j/K
nWQ33OJ9R1Z2fzQYI+usDOlhAky8Ydd7tGRSySRi2wIDAQABo1MwUTAdBgNVHQ4E
FgQUCiVve1DqaQ69DcEDjzFP+tWnDG0wHwYDVR0jBBgwFoAUCiVve1DqaQ69DcED
jzFP+tWnDG0wDwYDVR0TAQH/BAUwAwEB/zANBgkqhkiG9w0BAQsFAAOCAQEAfkLM
UuJoQmdgI0o+k6ejXwTVvwdYTFXJT80rxR3sNM5swD6QV+obDjE9O87jFBlWDpv7
57BFQ51u1iX5o5OKgs45E6CaZxCft0Hwujw7dhh8BgWxsUPO5b0WSE1Y+LNt9sFp
jaYa8uRpAj8v5oHTRr7BX3P2vLSzasaH329H7qrpu5Ve9lfEFJ8ktlDnu/jc/Dqt
Nnv4vQc88IU6fEnQLt/PIbawmlRRD4iwQXxPgHfoVwuJV04PFbTu5LEVpJZZxdcN
XnZWPt6+otkxENLnTxI3O1dsK4GksDQTjUq7/f6CJ7rV2IJYmBzjLqut8h2QPU3E
zWV2L5WdusMOjUkE7g==
-----END CERTIFICATE-----`;

declare global {
  interface Window {
    __aeroGatewayE2E?: {
      crossOriginIsolated: boolean;
      sharedArrayBuffer: { ok: boolean; error: string | null };
      websocket: { ok: boolean; echo: unknown; error: string | null };
      dnsQuery: { ok: boolean; meta: unknown; error: string | null };
      dnsJson: { ok: boolean; answer: unknown; error: string | null };
    };
  }
}

type StartedProcess = {
  baseUrl: string;
  port: number;
  stop: () => Promise<void>;
};

type StartedHttpsProxy = {
  baseUrl: string;
  port: number;
  close: () => Promise<void>;
};

function waitForProcessClose(proc: ChildProcess): Promise<void> {
  if (proc.exitCode !== null) return Promise.resolve();
  return new Promise((resolve) => {
    const onDone = () => {
      cleanup();
      resolve();
    };
    const cleanup = () => {
      proc.off('close', onDone);
      proc.off('exit', onDone);
      proc.off('error', onDone);
    };
    proc.once('close', onDone);
    proc.once('exit', onDone);
    proc.once('error', onDone);
    // Handle races where the process exits between the first `exitCode` check
    // and installing the event listeners.
    if (proc.exitCode !== null) onDone();
  });
}

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
      try {
        socket.write(data);
      } catch {
        try {
          socket.destroy();
        } catch {
          // ignore
        }
      }
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

async function startHttpsProxy(targetPort: number, listenPort: number): Promise<StartedHttpsProxy> {
  const server = https.createServer({ key: TLS_KEY, cert: TLS_CERT }, (req, res) => {
    const upstreamReq = http.request(
      {
        host: '127.0.0.1',
        port: targetPort,
        method: req.method,
        path: req.url,
        headers: { ...req.headers, host: `localhost:${targetPort}` },
      },
      (upstreamRes) => {
        res.writeHead(upstreamRes.statusCode ?? 502, upstreamRes.headers);
        pipeline(upstreamRes, res, (err) => {
          if (!err) return;
          // If we fail mid-stream, it's too late to send a clean 502; just terminate.
          try {
            res.destroy();
          } catch {
            // ignore
          }
        });
      },
    );

    upstreamReq.on('error', () => {
      if (!res.headersSent) res.writeHead(502, { 'content-type': 'text/plain; charset=utf-8' });
      res.end('Bad gateway\n');
    });

    pipeline(req, upstreamReq, () => {});
  });

  server.on('upgrade', (req, socket, head) => {
    const upstream = net.connect({ host: '127.0.0.1', port: targetPort }, () => {
      const method = typeof req.method === 'string' && req.method ? req.method : 'GET';
      const url = typeof req.url === 'string' && req.url ? req.url : '/';
      let requestHeader = `${method} ${url} HTTP/1.1\r\n`;
      for (let i = 0; i < req.rawHeaders.length; i += 2) {
        const key = req.rawHeaders[i];
        const value = req.rawHeaders[i + 1] ?? '';
        if (key.toLowerCase() === 'host') continue;
        requestHeader += `${key}: ${value}\r\n`;
      }
      requestHeader += `Host: localhost:${targetPort}\r\n\r\n`;
      try {
        upstream.write(requestHeader);
        if (head.length > 0) upstream.write(head);
      } catch {
        try {
          upstream.destroy();
        } catch {
          // ignore
        }
        try {
          socket.destroy();
        } catch {
          // ignore
        }
        return;
      }

      try {
        socket.pipe(upstream);
        upstream.pipe(socket);
      } catch {
        try {
          upstream.destroy();
        } catch {
          // ignore
        }
        try {
          socket.destroy();
        } catch {
          // ignore
        }
      }
    });

    upstream.on('error', () => {
      try {
        socket.destroy();
      } catch {
        // ignore
      }
    });
    socket.on('error', () => {
      try {
        upstream.destroy();
      } catch {
        // ignore
      }
    });
  });

  await new Promise<void>((resolve, reject) => {
    server.once('error', reject);
    server.listen(listenPort, '127.0.0.1', () => resolve());
  });

  const address = server.address() as AddressInfo | null;
  if (!address) throw new Error('HTTPS proxy did not bind');

  return {
    baseUrl: `https://localhost:${address.port}`,
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

  const address = socket.address() as AddressInfo;
  return {
    port: address.port,
    close: () =>
      new Promise<void>((resolve) => {
        socket.close(() => resolve());
      }),
  };
}

async function waitForHealthy(baseUrl: string, proc: ChildProcess): Promise<void> {
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

async function startGatewayProcess(opts: {
  crossOriginIsolation: boolean;
  dnsUpstreams: string;
  allowedOrigins: string;
}): Promise<StartedProcess> {
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
      ALLOWED_ORIGINS: opts.allowedOrigins,
      TCP_ALLOW_PRIVATE_IPS: '1',
      CROSS_ORIGIN_ISOLATION: opts.crossOriginIsolation ? '1' : '0',
      DNS_UPSTREAMS: opts.dnsUpstreams,
      AERO_GATEWAY_E2E: '1',
    },
    stdio: ['pipe', 'pipe', 'pipe'],
  });

  const output: string[] = [];
  proc.stdout.on('data', (chunk) => output.push(String(chunk)));
  proc.stderr.on('data', (chunk) => output.push(String(chunk)));

  try {
    await waitForHealthy(baseUrl, proc);
  } catch (err) {
    proc.kill('SIGKILL');
    await Promise.race([waitForProcessClose(proc), delay(2_000, undefined, { ref: false })]).catch(() => {});
    throw new Error(`${err instanceof Error ? err.message : String(err)}\nGateway output:\n${output.join('')}`);
  }

  return {
    baseUrl,
    port,
    stop: async () => {
      if (proc.exitCode !== null) return;
      proc.kill('SIGTERM');
      await Promise.race([waitForProcessClose(proc), delay(5_000, undefined, { ref: false })]).catch(() => {});
      if (proc.exitCode === null) {
        proc.kill('SIGKILL');
        await Promise.race([waitForProcessClose(proc), delay(5_000, undefined, { ref: false })]).catch(() => {});
      }
    },
  };
}

test.describe('aero-gateway browser e2e', () => {
  test.describe.configure({ mode: 'serial' });

  test('CROSS_ORIGIN_ISOLATION=1 enables crossOriginIsolated + SharedArrayBuffer', async ({ page }) => {
    const dns = await startUdpDnsServer();
    const echo = await startTcpEchoServer();
    const proxyPort = await getFreePort();
    const proxyOrigin = `https://localhost:${proxyPort}`;
    const gateway = await startGatewayProcess({
      crossOriginIsolation: true,
      dnsUpstreams: `127.0.0.1:${dns.port}`,
      allowedOrigins: proxyOrigin,
    });
    const proxy = await startHttpsProxy(gateway.port, proxyPort);
    try {
      const response = await page.goto(`${proxy.baseUrl}/e2e?echoPort=${echo.port}`);
      expect(response).not.toBeNull();
      const headers = response!.headers();
      expect(headers['cross-origin-opener-policy']).toBe('same-origin');
      expect(headers['cross-origin-embedder-policy']).toBe('require-corp');

      await page.waitForFunction(() => Boolean(window.__aeroGatewayE2E), null, { timeout: 10_000 });
      const results = await page.evaluate(() => window.__aeroGatewayE2E!);

      expect(results.crossOriginIsolated).toBe(true);
      expect(results.sharedArrayBuffer.ok).toBe(true);
      expect(results.sharedArrayBuffer.error).toBeNull();

      expect(results.websocket.ok).toBe(true);
      expect(results.websocket.error).toBeNull();

      expect(results.dnsQuery.ok).toBe(true);
      expect(results.dnsQuery.error).toBeNull();

      expect(results.dnsJson.ok).toBe(true);
      expect(results.dnsJson.error).toBeNull();
    } finally {
      await proxy.close();
      await gateway.stop();
      await echo.close();
      await dns.close();
    }
  });

  test('CROSS_ORIGIN_ISOLATION unset disables crossOriginIsolated (and reports why)', async ({ page }) => {
    const dns = await startUdpDnsServer();
    const echo = await startTcpEchoServer();
    const proxyPort = await getFreePort();
    const proxyOrigin = `https://localhost:${proxyPort}`;
    const gateway = await startGatewayProcess({
      crossOriginIsolation: false,
      dnsUpstreams: `127.0.0.1:${dns.port}`,
      allowedOrigins: proxyOrigin,
    });
    const proxy = await startHttpsProxy(gateway.port, proxyPort);
    try {
      const response = await page.goto(`${proxy.baseUrl}/e2e?echoPort=${echo.port}`);
      expect(response).not.toBeNull();
      const headers = response!.headers();
      expect(headers['cross-origin-opener-policy']).toBeUndefined();
      expect(headers['cross-origin-embedder-policy']).toBeUndefined();

      await page.waitForFunction(() => Boolean(window.__aeroGatewayE2E), null, { timeout: 10_000 });
      const results = await page.evaluate(() => window.__aeroGatewayE2E!);

      expect(results.crossOriginIsolated).toBe(false);
      expect(results.sharedArrayBuffer.ok).toBe(false);
      expect(results.sharedArrayBuffer.error).not.toBeNull();

      expect(results.websocket.ok).toBe(true);
      expect(results.websocket.error).toBeNull();

      expect(results.dnsQuery.ok).toBe(true);
      expect(results.dnsQuery.error).toBeNull();

      expect(results.dnsJson.ok).toBe(true);
      expect(results.dnsJson.error).toBeNull();
    } finally {
      await proxy.close();
      await gateway.stop();
      await echo.close();
      await dns.close();
    }
  });
});
