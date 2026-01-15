export type TcpProxyEgressMetricSink = Readonly<{
  blockedByHostPolicyTotal?: { inc: () => void };
  blockedByIpPolicyTotal?: { inc: () => void };
}>;

