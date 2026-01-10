import assert from "node:assert/strict";

function macToBuffer(mac) {
  const parts = mac.split(":");
  assert.equal(parts.length, 6, "MAC must have 6 octets");
  return Buffer.from(parts.map((p) => Number.parseInt(p, 16)));
}

function ipToBuffer(ip) {
  const parts = ip.split(".");
  assert.equal(parts.length, 4, "IPv4 must have 4 octets");
  return Buffer.from(parts.map((p) => Number.parseInt(p, 10)));
}

function bufferToIp(buf, offset = 0) {
  return `${buf[offset]}.${buf[offset + 1]}.${buf[offset + 2]}.${buf[offset + 3]}`;
}

function bufferToMac(buf, offset = 0) {
  const parts = [];
  for (let i = 0; i < 6; i++) parts.push(buf[offset + i].toString(16).padStart(2, "0"));
  return parts.join(":");
}

function checksum16(buf) {
  // Standard Internet checksum (one's complement sum of 16-bit words).
  // For odd-length inputs, pad with a trailing zero byte for the calculation.
  let sum = 0;
  const len = buf.length;
  for (let i = 0; i < len; i += 2) {
    const hi = buf[i];
    const lo = i + 1 < len ? buf[i + 1] : 0;
    sum += (hi << 8) | lo;
    while (sum > 0xffff) sum = (sum & 0xffff) + (sum >>> 16);
  }
  return (~sum) & 0xffff;
}

function encodeEthernetFrame({ dstMac, srcMac, ethertype, payload }) {
  assert.equal(dstMac.length, 6);
  assert.equal(srcMac.length, 6);
  const out = Buffer.allocUnsafe(14 + payload.length);
  dstMac.copy(out, 0);
  srcMac.copy(out, 6);
  out.writeUInt16BE(ethertype, 12);
  payload.copy(out, 14);
  return out;
}

function parseEthernetFrame(frame) {
  assert.ok(frame.length >= 14, "Ethernet frame too short");
  const dstMac = frame.subarray(0, 6);
  const srcMac = frame.subarray(6, 12);
  const ethertype = frame.readUInt16BE(12);
  const payload = frame.subarray(14);
  return { dstMac, srcMac, ethertype, payload };
}

function encodeArp({
  opcode,
  senderMac,
  senderIp,
  targetMac,
  targetIp,
}) {
  // Ethernet/IPv4 ARP packet.
  const out = Buffer.alloc(28);
  out.writeUInt16BE(1, 0); // HTYPE Ethernet
  out.writeUInt16BE(0x0800, 2); // PTYPE IPv4
  out.writeUInt8(6, 4); // HLEN
  out.writeUInt8(4, 5); // PLEN
  out.writeUInt16BE(opcode, 6); // OPER
  senderMac.copy(out, 8);
  senderIp.copy(out, 14);
  targetMac.copy(out, 18);
  targetIp.copy(out, 24);
  return out;
}

function parseArp(buf) {
  assert.ok(buf.length >= 28, "ARP packet too short");
  const htype = buf.readUInt16BE(0);
  const ptype = buf.readUInt16BE(2);
  const hlen = buf.readUInt8(4);
  const plen = buf.readUInt8(5);
  assert.equal(htype, 1, "ARP: unsupported htype");
  assert.equal(ptype, 0x0800, "ARP: unsupported ptype");
  assert.equal(hlen, 6, "ARP: unsupported hlen");
  assert.equal(plen, 4, "ARP: unsupported plen");
  const opcode = buf.readUInt16BE(6);
  const senderMac = buf.subarray(8, 14);
  const senderIp = buf.subarray(14, 18);
  const targetMac = buf.subarray(18, 24);
  const targetIp = buf.subarray(24, 28);
  return { opcode, senderMac, senderIp, targetMac, targetIp };
}

function encodeIPv4({ srcIp, dstIp, protocol, payload, ttl = 64, id = 0 }) {
  const ihl = 5;
  const headerLen = ihl * 4;
  const totalLen = headerLen + payload.length;
  const out = Buffer.allocUnsafe(totalLen);
  out.writeUInt8((4 << 4) | ihl, 0);
  out.writeUInt8(0, 1); // DSCP/ECN
  out.writeUInt16BE(totalLen, 2);
  out.writeUInt16BE(id & 0xffff, 4);
  out.writeUInt16BE(0, 6); // flags/fragment offset
  out.writeUInt8(ttl, 8);
  out.writeUInt8(protocol, 9);
  out.writeUInt16BE(0, 10); // checksum placeholder
  srcIp.copy(out, 12);
  dstIp.copy(out, 16);
  const csum = checksum16(out.subarray(0, headerLen));
  out.writeUInt16BE(csum, 10);
  payload.copy(out, headerLen);
  return out;
}

function parseIPv4(buf) {
  assert.ok(buf.length >= 20, "IPv4 packet too short");
  const verIhl = buf.readUInt8(0);
  const version = verIhl >>> 4;
  const ihl = verIhl & 0x0f;
  assert.equal(version, 4, "IPv4: unexpected version");
  const headerLen = ihl * 4;
  assert.ok(buf.length >= headerLen, "IPv4: truncated header");
  const totalLen = buf.readUInt16BE(2);
  const protocol = buf.readUInt8(9);
  const srcIp = buf.subarray(12, 16);
  const dstIp = buf.subarray(16, 20);
  const payload = buf.subarray(headerLen, totalLen);
  return { headerLen, totalLen, protocol, srcIp, dstIp, payload };
}

function encodeUDP({ srcPort, dstPort, payload, srcIp, dstIp }) {
  const len = 8 + payload.length;
  const out = Buffer.allocUnsafe(len);
  out.writeUInt16BE(srcPort, 0);
  out.writeUInt16BE(dstPort, 2);
  out.writeUInt16BE(len, 4);
  out.writeUInt16BE(0, 6); // checksum optional in IPv4
  payload.copy(out, 8);

  // If src/dst IP are provided, compute checksum for better realism.
  if (srcIp && dstIp) {
    const pseudo = Buffer.allocUnsafe(12 + len);
    srcIp.copy(pseudo, 0);
    dstIp.copy(pseudo, 4);
    pseudo.writeUInt8(0, 8);
    pseudo.writeUInt8(17, 9);
    pseudo.writeUInt16BE(len, 10);
    out.copy(pseudo, 12);
    let csum = checksum16(pseudo);
    if (csum === 0) csum = 0xffff; // per RFC768
    out.writeUInt16BE(csum, 6);
  }

  return out;
}

function parseUDP(buf) {
  assert.ok(buf.length >= 8, "UDP packet too short");
  const srcPort = buf.readUInt16BE(0);
  const dstPort = buf.readUInt16BE(2);
  const len = buf.readUInt16BE(4);
  const payload = buf.subarray(8, len);
  return { srcPort, dstPort, payload };
}

function encodeDnsQuery({ id, name }) {
  const qname = encodeDnsName(name);
  const out = Buffer.allocUnsafe(12 + qname.length + 4);
  out.writeUInt16BE(id, 0);
  out.writeUInt16BE(0x0100, 2); // standard query, recursion desired
  out.writeUInt16BE(1, 4); // QDCOUNT
  out.writeUInt16BE(0, 6); // ANCOUNT
  out.writeUInt16BE(0, 8); // NSCOUNT
  out.writeUInt16BE(0, 10); // ARCOUNT
  qname.copy(out, 12);
  let off = 12 + qname.length;
  out.writeUInt16BE(1, off); // QTYPE A
  out.writeUInt16BE(1, off + 2); // QCLASS IN
  return out;
}

function parseDnsQuery(buf) {
  assert.ok(buf.length >= 12, "DNS packet too short");
  const id = buf.readUInt16BE(0);
  const qdcount = buf.readUInt16BE(4);
  assert.equal(qdcount, 1, "DNS: only QDCOUNT=1 supported");
  let off = 12;
  const { name, nextOffset } = decodeDnsName(buf, off);
  off = nextOffset;
  const qtype = buf.readUInt16BE(off);
  const qclass = buf.readUInt16BE(off + 2);
  assert.equal(qtype, 1, "DNS: only A queries supported");
  assert.equal(qclass, 1, "DNS: only IN supported");
  return { id, name };
}

function encodeDnsResponseA({ id, name, ip }) {
  const qname = encodeDnsName(name);
  const answer = Buffer.allocUnsafe(16);
  // NAME: pointer to question at 0x0c
  answer.writeUInt16BE(0xc00c, 0);
  answer.writeUInt16BE(1, 2); // TYPE A
  answer.writeUInt16BE(1, 4); // CLASS IN
  answer.writeUInt32BE(60, 6); // TTL
  answer.writeUInt16BE(4, 10); // RDLENGTH
  ip.copy(answer, 12); // RDATA

  const out = Buffer.allocUnsafe(12 + qname.length + 4 + answer.length);
  out.writeUInt16BE(id, 0);
  out.writeUInt16BE(0x8180, 2); // response, recursion available, no error
  out.writeUInt16BE(1, 4); // QDCOUNT
  out.writeUInt16BE(1, 6); // ANCOUNT
  out.writeUInt16BE(0, 8); // NSCOUNT
  out.writeUInt16BE(0, 10); // ARCOUNT
  qname.copy(out, 12);
  let off = 12 + qname.length;
  out.writeUInt16BE(1, off); // QTYPE A
  out.writeUInt16BE(1, off + 2); // QCLASS IN
  answer.copy(out, off + 4);
  return out;
}

function parseDnsResponseA(buf) {
  assert.ok(buf.length >= 12, "DNS packet too short");
  const id = buf.readUInt16BE(0);
  const flags = buf.readUInt16BE(2);
  assert.ok((flags & 0x8000) !== 0, "DNS: not a response");
  const qdcount = buf.readUInt16BE(4);
  const ancount = buf.readUInt16BE(6);
  assert.equal(qdcount, 1, "DNS: only QDCOUNT=1 supported");
  assert.ok(ancount >= 1, "DNS: expected at least one answer");
  let off = 12;
  const q = decodeDnsName(buf, off);
  const qname = q.name;
  off = q.nextOffset + 4; // skip QTYPE/QCLASS

  // Support only first A answer.
  // NAME may be a pointer; we don't need to decode it fully for this probe.
  const nameOrPtr = buf.readUInt16BE(off);
  assert.ok((nameOrPtr & 0xc000) === 0xc000, "DNS: expected pointer NAME");
  const type = buf.readUInt16BE(off + 2);
  const klass = buf.readUInt16BE(off + 4);
  assert.equal(type, 1, "DNS: expected A answer");
  assert.equal(klass, 1, "DNS: expected IN answer");
  const rdlen = buf.readUInt16BE(off + 10);
  assert.equal(rdlen, 4, "DNS: expected IPv4 RDLENGTH=4");
  const ip = bufferToIp(buf, off + 12);
  return { id, name: qname, ip };
}

function encodeDnsName(name) {
  const parts = name.split(".");
  const chunks = [];
  for (const part of parts) {
    const b = Buffer.from(part, "ascii");
    assert.ok(b.length <= 63, "DNS label too long");
    chunks.push(Buffer.from([b.length]));
    chunks.push(b);
  }
  chunks.push(Buffer.from([0]));
  return Buffer.concat(chunks);
}

function decodeDnsName(buf, offset) {
  const parts = [];
  let off = offset;
  while (true) {
    assert.ok(off < buf.length, "DNS name truncated");
    const len = buf.readUInt8(off++);
    if (len === 0) break;
    const end = off + len;
    assert.ok(end <= buf.length, "DNS label truncated");
    parts.push(buf.subarray(off, end).toString("ascii"));
    off = end;
  }
  return { name: parts.join("."), nextOffset: off };
}

const TCP_FLAGS = {
  FIN: 0x01,
  SYN: 0x02,
  RST: 0x04,
  PSH: 0x08,
  ACK: 0x10,
  URG: 0x20,
  ECE: 0x40,
  CWR: 0x80,
};

function encodeTCP({
  srcPort,
  dstPort,
  seq,
  ack,
  flags,
  window = 65535,
  payload = Buffer.alloc(0),
  srcIp,
  dstIp,
}) {
  const headerLen = 20;
  const totalLen = headerLen + payload.length;
  const out = Buffer.allocUnsafe(totalLen);
  out.writeUInt16BE(srcPort, 0);
  out.writeUInt16BE(dstPort, 2);
  out.writeUInt32BE(seq >>> 0, 4);
  out.writeUInt32BE(ack >>> 0, 8);
  out.writeUInt8((headerLen / 4) << 4, 12); // data offset, no options
  out.writeUInt8(flags, 13);
  out.writeUInt16BE(window, 14);
  out.writeUInt16BE(0, 16); // checksum placeholder
  out.writeUInt16BE(0, 18); // urgent pointer
  payload.copy(out, headerLen);

  if (srcIp && dstIp) {
    const pseudo = Buffer.allocUnsafe(12 + totalLen);
    srcIp.copy(pseudo, 0);
    dstIp.copy(pseudo, 4);
    pseudo.writeUInt8(0, 8);
    pseudo.writeUInt8(6, 9);
    pseudo.writeUInt16BE(totalLen, 10);
    out.copy(pseudo, 12);
    const csum = checksum16(pseudo);
    out.writeUInt16BE(csum, 16);
  }

  return out;
}

function parseTCP(buf) {
  assert.ok(buf.length >= 20, "TCP segment too short");
  const srcPort = buf.readUInt16BE(0);
  const dstPort = buf.readUInt16BE(2);
  const seq = buf.readUInt32BE(4);
  const ack = buf.readUInt32BE(8);
  const dataOffsetWords = buf.readUInt8(12) >>> 4;
  const headerLen = dataOffsetWords * 4;
  assert.ok(buf.length >= headerLen, "TCP header truncated");
  const flags = buf.readUInt8(13);
  const window = buf.readUInt16BE(14);
  const payload = buf.subarray(headerLen);
  return { srcPort, dstPort, seq, ack, flags, window, payload };
}

export {
  TCP_FLAGS,
  bufferToIp,
  bufferToMac,
  checksum16,
  encodeArp,
  encodeDnsQuery,
  encodeDnsResponseA,
  encodeEthernetFrame,
  encodeIPv4,
  encodeTCP,
  encodeUDP,
  ipToBuffer,
  macToBuffer,
  parseArp,
  parseDnsQuery,
  parseDnsResponseA,
  parseEthernetFrame,
  parseIPv4,
  parseTCP,
  parseUDP,
};
