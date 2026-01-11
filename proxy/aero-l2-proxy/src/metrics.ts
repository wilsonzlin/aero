const PING_RTT_MS_BUCKETS = [1, 5, 10, 25, 50, 100, 250, 500, 1000, 2500] as const;

function pushGauge(out: string[], name: string, val: number): void {
  out.push(`# TYPE ${name} gauge`);
  out.push(`${name} ${val}`);
}

function pushCounter(out: string[], name: string, val: bigint): void {
  out.push(`# TYPE ${name} counter`);
  out.push(`${name} ${val.toString()}`);
}

function pushZeroHistogram(out: string[], name: string, buckets: readonly number[]): void {
  out.push(`# TYPE ${name} histogram`);
  for (const bound of buckets) {
    out.push(`${name}_bucket{le="${bound}"} 0`);
  }
  out.push(`${name}_bucket{le="+Inf"} 0`);
  out.push(`${name}_sum 0`);
  out.push(`${name}_count 0`);
}

export class L2ProxyMetrics {
  private sessionsActive = 0;
  private sessionsTotal = 0n;

  private framesRxTotal = 0n;
  private framesTxTotal = 0n;
  private bytesRxTotal = 0n;
  private bytesTxTotal = 0n;
  private framesDroppedTotal = 0n;

  private policyDeniedTotal = 0n;

  // The quota/upgrade harness does not implement the full data plane, but we export the
  // production metric names so dashboards can be wired consistently.
  private tcpConnsActive = 0;
  private tcpConnectFailTotal = 0n;
  private udpFlowsActive = 0;
  private udpSendFailTotal = 0n;
  private dnsQueriesTotal = 0n;
  private dnsFailTotal = 0n;

  sessionOpened(): void {
    this.sessionsTotal += 1n;
    this.sessionsActive += 1;
  }

  sessionClosed(): void {
    if (this.sessionsActive > 0) this.sessionsActive -= 1;
  }

  frameRx(bytes: number): void {
    this.framesRxTotal += 1n;
    this.bytesRxTotal += BigInt(bytes);
  }

  frameTx(bytes: number): void {
    this.framesTxTotal += 1n;
    this.bytesTxTotal += BigInt(bytes);
  }

  frameDropped(): void {
    this.framesDroppedTotal += 1n;
  }

  policyDenied(): void {
    this.policyDeniedTotal += 1n;
  }

  renderPrometheus(): string {
    const out: string[] = [];

    pushGauge(out, "l2_sessions_active", this.sessionsActive);
    pushCounter(out, "l2_sessions_total", this.sessionsTotal);

    pushCounter(out, "l2_frames_rx_total", this.framesRxTotal);
    pushCounter(out, "l2_frames_tx_total", this.framesTxTotal);
    pushCounter(out, "l2_bytes_rx_total", this.bytesRxTotal);
    pushCounter(out, "l2_bytes_tx_total", this.bytesTxTotal);
    pushCounter(out, "l2_frames_dropped_total", this.framesDroppedTotal);

    pushCounter(out, "l2_policy_denied_total", this.policyDeniedTotal);

    pushGauge(out, "l2_tcp_conns_active", this.tcpConnsActive);
    pushCounter(out, "l2_tcp_connect_fail_total", this.tcpConnectFailTotal);
    pushGauge(out, "l2_udp_flows_active", this.udpFlowsActive);
    pushCounter(out, "l2_udp_send_fail_total", this.udpSendFailTotal);
    pushCounter(out, "l2_dns_queries_total", this.dnsQueriesTotal);
    pushCounter(out, "l2_dns_fail_total", this.dnsFailTotal);

    pushZeroHistogram(out, "l2_ping_rtt_ms", PING_RTT_MS_BUCKETS);

    return `${out.join("\n")}\n`;
  }
}

