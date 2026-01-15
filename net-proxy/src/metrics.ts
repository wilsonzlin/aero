export type MetricsProto = "tcp" | "tcp_mux" | "udp";
export type MetricsErrorKind = "denied" | "error";

export interface ProxyServerMetrics {
  connectionActiveInc: (proto: MetricsProto) => void;
  connectionActiveDec: (proto: MetricsProto) => void;
  tcpMuxStreamsActiveInc: () => void;
  tcpMuxStreamsActiveDec: (delta?: number) => void;
  udpBindingsActiveInc: () => void;
  udpBindingsActiveDec: (delta?: number) => void;
  addBytesIn: (proto: MetricsProto, bytes: number) => void;
  addBytesOut: (proto: MetricsProto, bytes: number) => void;
  incConnectionError: (kind: MetricsErrorKind) => void;
  prometheusText: () => string;
}

export function createProxyServerMetrics(): ProxyServerMetrics {
  const connectionsActive: Record<MetricsProto, number> = {
    tcp: 0,
    tcp_mux: 0,
    udp: 0
  };
  const bytesInTotal: Record<MetricsProto, bigint> = {
    tcp: 0n,
    tcp_mux: 0n,
    udp: 0n
  };
  const bytesOutTotal: Record<MetricsProto, bigint> = {
    tcp: 0n,
    tcp_mux: 0n,
    udp: 0n
  };
  const connectionErrorsTotal: Record<MetricsErrorKind, bigint> = {
    denied: 0n,
    error: 0n
  };
  let udpBindingsActive = 0;
  let tcpMuxStreamsActive = 0;

  const clampNonNegative = (n: number): number => (n < 0 ? 0 : n);

  const prometheusText = (): string => {
    const lines: string[] = [];

    lines.push("# HELP net_proxy_connections_active Active relay WebSocket connections.");
    lines.push("# TYPE net_proxy_connections_active gauge");
    lines.push(`net_proxy_connections_active{proto="tcp"} ${connectionsActive.tcp}`);
    lines.push(`net_proxy_connections_active{proto="tcp_mux"} ${connectionsActive.tcp_mux}`);
    lines.push(`net_proxy_connections_active{proto="udp"} ${connectionsActive.udp}`);

    lines.push("# HELP net_proxy_tcp_connections_active Active TCP relay connections (outbound TCP sockets).");
    lines.push("# TYPE net_proxy_tcp_connections_active gauge");
    lines.push(`net_proxy_tcp_connections_active{proto="tcp"} ${connectionsActive.tcp}`);
    lines.push(`net_proxy_tcp_connections_active{proto="tcp_mux"} ${tcpMuxStreamsActive}`);

    lines.push("# HELP net_proxy_udp_bindings_active Active UDP bindings in multiplexed /udp mode.");
    lines.push("# TYPE net_proxy_udp_bindings_active gauge");
    lines.push(`net_proxy_udp_bindings_active ${udpBindingsActive}`);

    lines.push("# HELP net_proxy_bytes_in_total Total bytes received from the client (towards target sockets), by protocol.");
    lines.push("# TYPE net_proxy_bytes_in_total counter");
    lines.push(`net_proxy_bytes_in_total{proto="tcp"} ${bytesInTotal.tcp}`);
    lines.push(`net_proxy_bytes_in_total{proto="tcp_mux"} ${bytesInTotal.tcp_mux}`);
    lines.push(`net_proxy_bytes_in_total{proto="udp"} ${bytesInTotal.udp}`);

    lines.push("# HELP net_proxy_bytes_out_total Total bytes sent to the client (from target sockets), by protocol.");
    lines.push("# TYPE net_proxy_bytes_out_total counter");
    lines.push(`net_proxy_bytes_out_total{proto="tcp"} ${bytesOutTotal.tcp}`);
    lines.push(`net_proxy_bytes_out_total{proto="tcp_mux"} ${bytesOutTotal.tcp_mux}`);
    lines.push(`net_proxy_bytes_out_total{proto="udp"} ${bytesOutTotal.udp}`);

    lines.push("# HELP net_proxy_connection_errors_total Total connection errors (denied by policy or failed to connect).");
    lines.push("# TYPE net_proxy_connection_errors_total counter");
    lines.push(`net_proxy_connection_errors_total{kind="denied"} ${connectionErrorsTotal.denied}`);
    lines.push(`net_proxy_connection_errors_total{kind="error"} ${connectionErrorsTotal.error}`);

    return `${lines.join("\n")}\n`;
  };

  return {
    connectionActiveInc: (proto) => {
      connectionsActive[proto] = clampNonNegative(connectionsActive[proto] + 1);
    },
    connectionActiveDec: (proto) => {
      connectionsActive[proto] = clampNonNegative(connectionsActive[proto] - 1);
    },
    tcpMuxStreamsActiveInc: () => {
      tcpMuxStreamsActive = clampNonNegative(tcpMuxStreamsActive + 1);
    },
    tcpMuxStreamsActiveDec: (delta = 1) => {
      tcpMuxStreamsActive = clampNonNegative(tcpMuxStreamsActive - delta);
    },
    udpBindingsActiveInc: () => {
      udpBindingsActive = clampNonNegative(udpBindingsActive + 1);
    },
    udpBindingsActiveDec: (delta = 1) => {
      udpBindingsActive = clampNonNegative(udpBindingsActive - delta);
    },
    addBytesIn: (proto, bytes) => {
      if (!Number.isFinite(bytes) || bytes <= 0) return;
      bytesInTotal[proto] += BigInt(bytes);
    },
    addBytesOut: (proto, bytes) => {
      if (!Number.isFinite(bytes) || bytes <= 0) return;
      bytesOutTotal[proto] += BigInt(bytes);
    },
    incConnectionError: (kind) => {
      connectionErrorsTotal[kind] += 1n;
    },
    prometheusText
  };
}

