import { defaultReadValue } from "../ipc/io_protocol.ts";

export interface PortIoHandler {
  portRead(port: number, size: number): number;
  portWrite(port: number, size: number, value: number): void;
}

export class PortIoBus {
  #ports: Array<PortIoHandler | null> = Array.from({ length: 0x1_0000 }, () => null);

  registerRange(startPort: number, endPort: number, handler: PortIoHandler): void {
    if (startPort < 0 || startPort > 0xffff) throw new RangeError(`startPort out of range: ${startPort}`);
    if (endPort < 0 || endPort > 0xffff) throw new RangeError(`endPort out of range: ${endPort}`);
    if (endPort < startPort) throw new RangeError(`endPort ${endPort} < startPort ${startPort}`);

    for (let port = startPort; port <= endPort; port++) {
      if (this.#ports[port] !== null) {
        throw new Error(`port 0x${port.toString(16)} already mapped`);
      }
      this.#ports[port] = handler;
    }
  }

  unregisterRange(startPort: number, endPort: number, handler: PortIoHandler): void {
    for (let port = startPort; port <= endPort; port++) {
      if (this.#ports[port] !== handler) continue;
      this.#ports[port] = null;
    }
  }

  read(port: number, size: number): number {
    const handler = this.#ports[port & 0xffff];
    if (!handler) return defaultReadValue(size);
    return handler.portRead(port & 0xffff, size) >>> 0;
  }

  write(port: number, size: number, value: number): void {
    const handler = this.#ports[port & 0xffff];
    if (!handler) return;
    handler.portWrite(port & 0xffff, size, value >>> 0);
  }
}

