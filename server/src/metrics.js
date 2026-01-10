export function createMetrics() {
  const counters = new Map([
    ["wsConnectionsTotal", 0],
    ["tcpConnectionsTotal", 0],
    ["tcpRejectedTotal", 0],
    ["dnsLookupsTotal", 0],
    ["bytesInTotal", 0],
    ["bytesOutTotal", 0],
  ]);
  const gauges = new Map([
    ["wsConnectionsCurrent", 0],
    ["tcpConnectionsCurrent", 0],
  ]);

  function increment(name, by = 1) {
    counters.set(name, (counters.get(name) ?? 0) + by);
  }

  function addGauge(name, by) {
    gauges.set(name, (gauges.get(name) ?? 0) + by);
  }

  function setGauge(name, value) {
    gauges.set(name, value);
  }

  function toPrometheus() {
    const lines = [];
    const emitCounter = (key, help) => {
      lines.push(`# TYPE aero_proxy_${key} counter`);
      lines.push(`# HELP aero_proxy_${key} ${help}`);
      lines.push(`aero_proxy_${key} ${counters.get(key) ?? 0}`);
    };
    const emitGauge = (key, help) => {
      lines.push(`# TYPE aero_proxy_${key} gauge`);
      lines.push(`# HELP aero_proxy_${key} ${help}`);
      lines.push(`aero_proxy_${key} ${gauges.get(key) ?? 0}`);
    };

    emitGauge("wsConnectionsCurrent", "Current active WebSocket connections");
    emitCounter("wsConnectionsTotal", "Total WebSocket connections accepted");
    emitGauge("tcpConnectionsCurrent", "Current active TCP sockets");
    emitCounter("tcpConnectionsTotal", "Total TCP sockets opened");
    emitCounter("tcpRejectedTotal", "Total TCP connect attempts rejected by policy/limits");
    emitCounter("dnsLookupsTotal", "Total DNS lookup API calls");
    emitCounter("bytesInTotal", "Total bytes received from WebSocket clients");
    emitCounter("bytesOutTotal", "Total bytes sent to WebSocket clients");

    return `${lines.join("\n")}\n`;
  }

  return {
    increment,
    addGauge,
    setGauge,
    toPrometheus,
  };
}

