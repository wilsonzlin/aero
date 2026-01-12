# 05 - Storage Subsystem

## Overview

Windows 7 requires significant storage (15-40GB installed). The storage subsystem must efficiently emulate disk controllers while using browser storage APIs that have their own constraints.

While AHCI provides out-of-the-box Windows compatibility, **virtio-blk** is the preferred high-performance path once virtio drivers are available. Under Aero’s Windows 7 virtio contract ([`AERO-W7-VIRTIO` v1](./windows7-virtio-driver-contract.md)), virtio devices are exposed as **virtio-pci modern-only** (virtio 1.0+) via PCI vendor-specific capabilities and a single **BAR0 MMIO** register region.

Compatibility note: adding virtio-pci legacy/transitional (I/O port BAR) support may be desirable for some **upstream virtio-win** driver bundles, but it is **not required** by the Aero contract and is treated as an optional mode. See: [`16-virtio-pci-legacy-transitional.md`](./16-virtio-pci-legacy-transitional.md)

---

## Canonical Windows 7 storage topology (PCI + media attachment)

For Windows 7 boot/install, Aero uses a **deterministic, compatibility-first storage topology**:

- **ICH9 AHCI** for the primary HDD (Windows installs/boots from this disk)
- **PIIX3 IDE + ATAPI** for the CD-ROM (Windows install ISO / driver ISOs)

The canonical PCI BDF assignments, port/drive mapping, BIOS boot flows, and INTx→GSI routing are
defined in:

- [`05-storage-topology-win7.md`](./05-storage-topology-win7.md)

This topology is treated as part of the platform ABI: drift should be caught by unit tests.

---

## Storage Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                    Storage Stack                                 │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│  Windows 7                                                       │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │  File System (NTFS)                                      │    │
│  └─────────────────────────────────────────────────────────┘    │
│       │                                                          │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │  Volume Manager (volmgr.sys)                             │    │
│  └─────────────────────────────────────────────────────────┘    │
│       │                                                          │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │  Disk Class Driver (disk.sys)                            │    │
│  └─────────────────────────────────────────────────────────┘    │
│       │                                                          │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │  Storage miniport (AHCI: msahci.sys on Win7)             │    │
│  └─────────────────────────────────────────────────────────┘    │
│       │                                                          │
└───────┼─────────────────────────────────────────────────────────┘
        │  ◄── Emulation Boundary
        ▼
┌─────────────────────────────────────────────────────────────────┐
│                    Aero Storage Emulation                        │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │  AHCI Controller Emulation                               │    │
│  │    - HBA Memory Registers                                │    │
│  │    - Port Registers                                      │    │
│  │    - Command List Processing                             │    │
│  └─────────────────────────────────────────────────────────┘    │
│       │                                                          │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │  Virtual Disk Layer                                      │    │
│  │    - Sector Read/Write                                   │    │
│  │    - DMA Transfers                                       │    │
│  └─────────────────────────────────────────────────────────┘    │
│       │                                                          │
│  ┌────────────────┐  ┌────────────────┐  ┌────────────────┐    │
│  │  OPFS Backend  │  │IndexedDB Cache │  │  Remote API    │    │
│  │  (large files) │  │ (async cache)  │  │  (streaming)   │    │
│  └────────────────┘  └────────────────┘  └────────────────┘    │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

> Important: IndexedDB is **async-only** in the browser. The canonical Rust disk/controller stack
> (`aero-storage::{StorageBackend, VirtualDisk}` + `aero-devices-storage` AHCI/IDE) is
> **synchronous**, so IndexedDB cannot be used directly from that path without a cross-worker
> design. See: [`19-indexeddb-storage-story.md`](./19-indexeddb-storage-story.md) and the canonical
> trait guidance in [`20-storage-trait-consolidation.md`](./20-storage-trait-consolidation.md).

---

## AHCI Controller Emulation

### AHCI Overview

AHCI (Advanced Host Controller Interface) is the standard SATA controller interface used by Windows 7.

```rust
pub struct AhciController {
    // HBA Memory Registers
    hba: HbaMemory,
    
    // Ports (up to 32)
    ports: [AhciPort; 32],
    
    // Connected drives
    drives: Vec<VirtualDrive>,
    
    // IRQ state
    irq_pending: bool,
}

#[repr(C)]
pub struct HbaMemory {
    cap: u32,        // Host Capabilities
    ghc: u32,        // Global Host Control
    is: u32,         // Interrupt Status
    pi: u32,         // Ports Implemented
    vs: u32,         // Version
    ccc_ctl: u32,    // Command Completion Coalescing Control
    ccc_ports: u32,  // Command Completion Coalescing Ports
    em_loc: u32,     // Enclosure Management Location
    em_ctl: u32,     // Enclosure Management Control
    cap2: u32,       // Extended Capabilities
    bohc: u32,       // BIOS/OS Handoff Control
}

#[repr(C)]
pub struct PortRegisters {
    clb: u64,        // Command List Base Address
    fb: u64,         // FIS Base Address
    is: u32,         // Interrupt Status
    ie: u32,         // Interrupt Enable
    cmd: u32,        // Command and Status
    reserved: u32,
    tfd: u32,        // Task File Data
    sig: u32,        // Signature
    ssts: u32,       // SATA Status
    sctl: u32,       // SATA Control
    serr: u32,       // SATA Error
    sact: u32,       // SATA Active
    ci: u32,         // Command Issue
    sntf: u32,       // SATA Notification
    fbs: u32,        // FIS-based Switching Control
}
```

### Command Processing

```rust
impl AhciController {
    pub fn process_port(&mut self, port_num: usize, memory: &mut MemoryBus) {
        let port = &mut self.ports[port_num];
        
        // Check if commands are issued
        while port.ci != 0 {
            // Find the lowest set bit (next command slot)
            let slot = port.ci.trailing_zeros() as usize;
            
            // Read command header from guest memory
            let cmd_header_addr = port.clb + (slot * 32) as u64;
            let cmd_header = self.read_command_header(memory, cmd_header_addr);
            
            // Read command table
            let cmd_table_addr = cmd_header.ctba;
            let cmd_table = self.read_command_table(memory, cmd_table_addr);
            
            // Process the command FIS
            self.process_command_fis(port_num, &cmd_header, &cmd_table, memory);
            
            // Clear command slot
            port.ci &= !(1 << slot);
            
            // Signal completion
            port.is |= AHCI_PORT_IS_DHRS;  // Device to Host Register FIS
            
            if port.ie & AHCI_PORT_IE_DHRE != 0 {
                self.raise_irq();
            }
        }
    }
    
    fn process_command_fis(
        &mut self,
        port: usize,
        header: &CommandHeader,
        table: &CommandTable,
        memory: &mut MemoryBus,
    ) {
        let fis = &table.cfis;
        
        match fis.command {
            ATA_CMD_READ_DMA_EXT => {
                let lba = self.extract_lba48(fis);
                let count = self.extract_sector_count(fis);
                self.do_read_dma(port, lba, count, header, table, memory);
            }
            ATA_CMD_WRITE_DMA_EXT => {
                let lba = self.extract_lba48(fis);
                let count = self.extract_sector_count(fis);
                self.do_write_dma(port, lba, count, header, table, memory);
            }
            ATA_CMD_IDENTIFY => {
                self.do_identify(port, header, table, memory);
            }
            ATA_CMD_SET_FEATURES => {
                self.do_set_features(port, fis);
            }
            ATA_CMD_FLUSH_CACHE_EXT => {
                self.do_flush(port);
            }
            _ => {
                log::warn!("Unknown ATA command: 0x{:02x}", fis.command);
            }
        }
    }
    
    fn do_read_dma(
        &mut self,
        port: usize,
        lba: u64,
        sector_count: u32,
        header: &CommandHeader,
        table: &CommandTable,
        memory: &mut MemoryBus,
    ) {
        let drive = &self.drives[port];
        let sector_size = drive.sector_size;
        
        // Read from virtual disk
        let mut data = vec![0u8; (sector_count as usize) * sector_size];
        drive.read_sectors(lba, &mut data);
        
        // DMA transfer to guest memory using PRD table
        let mut offset = 0;
        for prd in &table.prdt {
            let bytes_to_copy = ((prd.dbc & 0x3FFFFF) + 1) as usize;
            let dest_addr = prd.dba;
            
            memory.write_physical_bulk(dest_addr, &data[offset..offset + bytes_to_copy]);
            offset += bytes_to_copy;
            
            if offset >= data.len() {
                break;
            }
        }
    }
}
```

---

## Virtual Disk Implementation

### Disk Image Formats

```rust
pub enum DiskFormat {
    Raw,              // Direct sector mapping
    Qcow2,            // QEMU Copy-on-Write v2
    Vhd,              // Microsoft Virtual Hard Disk
    Sparse,           // Custom sparse format for OPFS
}

pub struct VirtualDrive {
    format: DiskFormat,
    sector_size: usize,
    total_sectors: u64,
    backend: Box<dyn DiskBackend>,
    write_cache: WriteCache,
}

pub trait DiskBackend: Send + Sync {
    fn read_sectors(&self, lba: u64, buffer: &mut [u8]) -> Result<()>;
    fn write_sectors(&mut self, lba: u64, buffer: &[u8]) -> Result<()>;
    fn flush(&mut self) -> Result<()>;
    fn capacity(&self) -> u64;
}
```

Note: in the current codebase, the canonical traits live in `crates/aero-storage/`:
`aero_storage::VirtualDisk` (byte-addressed with sector helpers) and
`aero_storage::StorageBackend` (resizable byte storage). Device models that use their own
disk traits typically consume an `aero_storage::VirtualDisk` via `crates/aero-storage-adapters/`.

**Implementation status (reference implementation):**
The canonical Rust disk image formats live in `crates/aero-storage/` and currently support:

- **QCOW2 v2/v3** (common unencrypted, uncompressed images; no backing files).
- **VHD fixed and dynamic** (unallocated blocks read as zeros; writes allocate blocks and update BAT/bitmap).

### OPFS Backend

In the repo, the OPFS backend is implemented in Rust/wasm32 in `crates/aero-opfs`
(e.g. `aero_opfs::OpfsBackend` / `aero_opfs::OpfsByteStorage`).

For the Rust controller path, **OPFS SyncAccessHandle is the expected backend** because it is
actually synchronous inside a Worker and can implement `aero_storage::StorageBackend` /
`aero_storage::VirtualDisk`. IndexedDB is async-only; see:
[`19-indexeddb-storage-story.md`](./19-indexeddb-storage-story.md) and
[`20-storage-trait-consolidation.md`](./20-storage-trait-consolidation.md).

The snippet below is illustrative; see `crates/aero-opfs` for the current implementation.

```rust
pub struct OpfsBackend {
    file_handle: FileSystemFileHandle,
    sync_handle: FileSystemSyncAccessHandle,
    sector_size: usize,
}

impl OpfsBackend {
    pub async fn open(path: &str) -> Result<Self> {
        let root = navigator_storage_get_directory().await?;
        
        let file_handle = root
            .get_file_handle(path, GetFileHandleOptions { create: true })
            .await?;
        
        // Get synchronous access handle for performance
        let sync_handle = file_handle.create_sync_access_handle().await?;
        
        Ok(Self {
            file_handle,
            sync_handle,
            sector_size: 512,
        })
    }
}

impl DiskBackend for OpfsBackend {
    fn read_sectors(&self, lba: u64, buffer: &mut [u8]) -> Result<()> {
        let offset = lba * self.sector_size as u64;
        self.sync_handle.read(buffer, offset)?;
        Ok(())
    }
    
    fn write_sectors(&mut self, lba: u64, buffer: &[u8]) -> Result<()> {
        let offset = lba * self.sector_size as u64;
        self.sync_handle.write(buffer, offset)?;
        Ok(())
    }
    
    fn flush(&mut self) -> Result<()> {
        self.sync_handle.flush()?;
        Ok(())
    }
}
```

### Sector Cache (IndexedDB)

IndexedDB can still be useful for *async* host-layer caching and disk management, but it is not a
drop-in backend for the synchronous Rust controller stack. See:
[`19-indexeddb-storage-story.md`](./19-indexeddb-storage-story.md) and
[`20-storage-trait-consolidation.md`](./20-storage-trait-consolidation.md).

In this repo, the Rust async IndexedDB block store lives in `crates/st-idb`, and the TypeScript
host-side storage utilities (disk manager/import/export, remote disk caching) live under
`web/src/storage/`.

```rust
pub struct SectorCache {
    db: IdbDatabase,
    cache: LruCache<u64, Vec<u8>>,
    dirty_sectors: HashSet<u64>,
    max_cached: usize,
}

impl SectorCache {
    pub async fn new(db_name: &str, max_sectors: usize) -> Result<Self> {
        let db = IdbDatabase::open(db_name, 1, |db| {
            db.create_object_store("sectors", ObjectStoreOptions {
                key_path: Some("lba"),
            });
        }).await?;
        
        Ok(Self {
            db,
            cache: LruCache::new(max_sectors),
            dirty_sectors: HashSet::new(),
            max_cached: max_sectors,
        })
    }
    
    pub fn get(&mut self, lba: u64) -> Option<&[u8]> {
        self.cache.get(&lba).map(|v| v.as_slice())
    }
    
    pub fn put(&mut self, lba: u64, data: Vec<u8>, dirty: bool) {
        if dirty {
            self.dirty_sectors.insert(lba);
        }
        self.cache.put(lba, data);
    }
    
    pub async fn flush_dirty(&mut self, backend: &mut dyn DiskBackend) -> Result<()> {
        let tx = self.db.transaction(&["sectors"], TransactionMode::Readwrite);
        let store = tx.object_store("sectors")?;
        
        for lba in self.dirty_sectors.drain() {
            if let Some(data) = self.cache.get(&lba) {
                // Write to backend
                backend.write_sectors(lba, data)?;
                
                // Optionally persist in IndexedDB for faster loads
                store.put(&SectorRecord { lba, data: data.clone() })?;
            }
        }
        
        tx.done().await?;
        backend.flush()?;
        Ok(())
    }
}
```

---

## Snapshot/Restore (Save States)

Storage snapshots must be **durable** and **deterministic**:

### What must be captured

- **IDE / AHCI / NVMe controller state**
  - full register sets (MMIO + PCI config where relevant)
  - command list / queue base pointers, head/tail indices
  - in-flight commands (slot/cid, PRDT/SG lists, transfer progress)
  - pending interrupts
- **Disk layer state**
  - backend identity (OPFS file path / IDB database name + object store)
  - optional read cache contents (hot sectors)
  - write-back cache contents (if any) and dirty tracking
  - flush-in-progress status

### Snapshot protocol requirements

1. **Force flush**: before accepting a snapshot, the I/O worker must flush any dirty write-back cache to the backing store.
2. **Capture disk metadata**: record enough information to reopen OPFS/IDB handles on restore.
3. **Versioned encoding**: snapshots must include a version header and be forward-compatible (unknown fields skipped).

### Restore semantics

- On restore, the I/O worker must **reopen OPFS/IDB handles** and then rehydrate any in-memory caches.
- If a snapshot is taken mid-command, the controller must resume from the captured in-flight command state (or abort the command in a guest-visible way if unsupported).

---

## Sparse Disk Format

For efficient storage of mostly-empty disk images:

```rust
pub struct SparseDisk {
    header: SparseHeader,
    allocation_table: Vec<u64>,  // Logical block -> Physical offset (0 = not allocated)
    data_file: OpfsBackend,
    block_size: usize,           // e.g., 1MB
}

#[repr(C)]
pub struct SparseHeader {
    magic: u32,                  // "Aero"
    version: u32,
    block_size: u32,
    total_blocks: u64,
    allocated_blocks: u64,
    table_offset: u64,
    data_offset: u64,
}

impl SparseDisk {
    pub fn read_block(&self, block_num: u64, buffer: &mut [u8]) -> Result<()> {
        let physical = self.allocation_table[block_num as usize];
        
        if physical == 0 {
            // Block not allocated - return zeros
            buffer.fill(0);
        } else {
            // Read from physical location
            self.data_file.read_at(physical, buffer)?;
        }
        
        Ok(())
    }
    
    pub fn write_block(&mut self, block_num: u64, buffer: &[u8]) -> Result<()> {
        let mut physical = self.allocation_table[block_num as usize];
        
        if physical == 0 {
            // Allocate new block
            physical = self.allocate_block()?;
            self.allocation_table[block_num as usize] = physical;
        }
        
        // Write to physical location
        self.data_file.write_at(physical, buffer)?;
        
        Ok(())
    }
    
    fn allocate_block(&mut self) -> Result<u64> {
        let offset = self.header.data_offset + 
                     self.header.allocated_blocks * self.block_size as u64;
        self.header.allocated_blocks += 1;
        Ok(offset)
    }
}
```

---

## NVMe Emulation (Optional Performance Path)

NVMe provides a simpler, higher-throughput datapath than AHCI (doorbells + DMA queues).
The reference implementation lives in `crates/aero-devices-nvme/` and is designed to plug
into Aero’s `DiskBackend` abstraction and `MemoryBus` DMA interface.

### Implemented (MVP)

- **PCI device model**
  - BAR0 register set: `CAP/VS/CC/CSTS/AQA/ASQ/ACQ` + doorbells.
  - Admin submission/completion queues.
  - I/O submission/completion queues created via admin commands.
- **Commands**
  - Admin: `IDENTIFY`, `CREATE IO CQ`, `CREATE IO SQ`.
  - I/O: `READ`, `WRITE`, `FLUSH`.
- **DMA**
  - PRP1/PRP2 + PRP list support for multi-page transfers.
  - SGL is not supported in the MVP (commands using SGL return an error).
- **Interrupts**
  - Legacy INTx signalling (sufficient to boot most guests).
  - **Limitation:** MSI/MSI-X is not implemented yet, so interrupt delivery is less efficient
    and may limit peak IOPS.

### Windows 7 Compatibility (Driver Requirements)

Windows 7 does **not** ship with an in-box NVMe driver. For Windows 7 guests, NVMe must be
treated as **experimental** unless the guest is provisioned with an NVMe driver.

Options (do not redistribute third-party binaries in-repo):

1. **Microsoft hotfixes (commonly referenced):**
   - KB2990941 (adds NVMe support)
   - KB3087873 (NVMe-related fixes)
2. **Vendor/third-party NVMe drivers** (e.g. SSD vendor drivers) installed inside the guest.

For maximum out-of-the-box compatibility, keep AHCI as the default controller and enable NVMe
only for performance experimentation.

---

## Virtio-blk (Paravirtualized)

For maximum performance, we provide virtio-blk drivers:

> For the exact Windows 7 driver ↔ Aero device-model interoperability contract (PCI transport, virtqueue rules, and virtio-blk requirements), see:  
> [`docs/windows7-virtio-driver-contract.md`](./windows7-virtio-driver-contract.md)  
>
> For the split-ring virtqueue implementation algorithms used by Windows 7 KMDF virtio drivers, see:  
> [`docs/virtio/virtqueue-split-ring-win7.md`](./virtio/virtqueue-split-ring-win7.md)  
>
> Windows 7 x64 enforces kernel-mode driver signatures. For test-signed virtio drivers (and the required `boot.wim`/`install.wim` + BCD servicing steps), see [16 - Windows 7 Install Media Servicing (WinPE/Setup) for Test-Signed Virtio Drivers](./16-win7-image-servicing.md).

```rust
pub struct VirtioBlkDevice {
    // Virtio common config
    device_features: u64,
    driver_features: u64,
    
    // Device-specific config
    capacity: u64,
    size_max: u32,
    seg_max: u32,
    blk_size: u32,
    
    // Virtqueues
    request_vq: Virtqueue,
    
    // Backend
    disk: Box<dyn DiskBackend>,
}

#[repr(C)]
pub struct VirtioBlkRequest {
    req_type: u32,   // VIRTIO_BLK_T_IN, OUT, FLUSH, etc.
    reserved: u32,
    sector: u64,
}

impl VirtioBlkDevice {
    pub fn process_queue(&mut self, memory: &mut MemoryBus) {
        while let Some(desc_chain) = self.request_vq.pop_available(memory) {
            // First descriptor: request header
            let header_addr = desc_chain[0].addr;
            let request: VirtioBlkRequest = memory.read_struct(header_addr);
            
            // Middle descriptors: data buffers
            let mut data_bufs: Vec<(u64, u32)> = desc_chain[1..desc_chain.len()-1]
                .iter()
                .map(|d| (d.addr, d.len))
                .collect();
            
            // Last descriptor: status byte
            let status_addr = desc_chain.last().unwrap().addr;
            
            let status = match request.req_type {
                VIRTIO_BLK_T_IN => self.do_read(request.sector, &data_bufs, memory),
                VIRTIO_BLK_T_OUT => self.do_write(request.sector, &data_bufs, memory),
                VIRTIO_BLK_T_FLUSH => self.do_flush(),
                _ => VIRTIO_BLK_S_UNSUPP,
            };
            
            // Write status
            memory.write_u8(status_addr, status);
            
            // Push completion
            self.request_vq.push_used(desc_chain.head_id, 1, memory);
        }
        
        // Signal guest
        if self.request_vq.should_notify() {
            self.raise_irq();
        }
    }
}
```

---

## CD-ROM/DVD Emulation

For Windows 7 installation:

> Note: If Aero uses custom/paravirtual storage devices, Windows Setup may require a
> "Load Driver" step. CI can optionally produce a small FAT32 driver disk image for
> this scenario; see [Driver Install Media (FAT Image)](./16-driver-install-media.md).

```rust
pub struct CdromDrive {
    // ATAPI interface
    atapi_state: AtapiState,
    
    // Disc image
    image: Option<IsoImage>,
    
    // State
    tray_open: bool,
    media_changed: bool,
}

impl CdromDrive {
    pub fn execute_packet_command(&mut self, packet: &[u8; 12], memory: &mut MemoryBus) -> AtapiResult {
        let opcode = packet[0];
        
        match opcode {
            ATAPI_READ_10 => {
                let lba = u32::from_be_bytes([packet[2], packet[3], packet[4], packet[5]]);
                let length = u16::from_be_bytes([packet[7], packet[8]]) as u32;
                self.read_sectors(lba, length, memory)
            }
            ATAPI_READ_12 => {
                let lba = u32::from_be_bytes([packet[2], packet[3], packet[4], packet[5]]);
                let length = u32::from_be_bytes([packet[6], packet[7], packet[8], packet[9]]);
                self.read_sectors(lba, length, memory)
            }
            ATAPI_READ_CAPACITY => {
                self.get_capacity(memory)
            }
            ATAPI_READ_TOC => {
                self.read_toc(packet, memory)
            }
            ATAPI_START_STOP_UNIT => {
                let loej = (packet[4] >> 1) & 1;
                let start = packet[4] & 1;
                self.start_stop(loej != 0, start != 0)
            }
            ATAPI_TEST_UNIT_READY => {
                self.test_unit_ready()
            }
            ATAPI_INQUIRY => {
                self.inquiry(memory)
            }
            ATAPI_MODE_SENSE => {
                self.mode_sense(packet, memory)
            }
            _ => {
                AtapiResult::Error(ATAPI_SENSE_ILLEGAL_REQUEST)
            }
        }
    }
}
```

---

## Disk Image Management

### Image Download and Streaming

Remote disk images can be streamed on-demand using HTTP `Range` requests while opportunistically caching fetched data into a local sparse file (OPFS).
To maximize cache hit-rate (especially when a CDN sits in front of the disk server), the client should:

- **Align reads to a fixed `CHUNK_SIZE`** (default: **1 MiB**).
- **Reuse the same chunk boundaries** for all requests (always fetch whole chunks like `bytes=N..N+CHUNK_SIZE-1`), rather than issuing variable-sized ranges.

Protocol-level requirements for authenticated disk streaming (HTTP `Range`, auth styles, CORS/COEP/CORP) are specified in [16 - Disk Image Streaming (HTTP Range + Auth + COOP/COEP)](./16-disk-image-streaming-auth.md).
Operational details for the backend **disk image streaming service** (deployment, troubleshooting) are documented in [backend/disk-image-streaming-service.md](./backend/disk-image-streaming-service.md).

The example below uses HTTP `Range` requests for random-access reads. For a CDN-friendly alternative that avoids `Range` (and therefore avoids CORS preflight on cross-origin fetches), see [18 - Chunked Disk Image Format](./18-chunked-disk-image-format.md).
For how disk/ISO images are uploaded/imported into a hosted service and kept private over time (including lease scopes and writeback options), see [Disk Image Lifecycle and Access Control](./17-disk-image-lifecycle-and-access-control.md).

Useful tooling in this repo:

- Correctness + CORS conformance checks: [`tools/disk-streaming-conformance/`](../tools/disk-streaming-conformance/README.md)
- Range throughput + CDN cache probing (`X-Cache`): [`tools/range-harness/`](../tools/range-harness/README.md)

```rust
pub struct DiskAccessLease {
    // Prefer a same-origin URL such as `/disk/<lease_id>` to avoid CORS preflight.
    remote_url: String,

    // Short-lived auth for private images. Public images may omit this, and signed-URL schemes
    // may embed auth material directly in `remote_url` instead.
    bearer_token: Option<String>,
    expires_at: Option<SystemTime>,
}

pub struct StreamingDisk {
    // Remote image access (may be unauthenticated for public images)
    lease: DiskAccessLease,
    total_size: u64,
    
    // Local cache
    local_cache: SparseDisk,
    
    // Download state
    downloaded_ranges: RangeSet,
    pending_fetches: HashMap<u64, oneshot::Sender<Vec<u8>>>,
}

impl StreamingDisk {
    pub async fn read_sectors(&mut self, lba: u64, buffer: &mut [u8]) -> Result<()> {
        let byte_offset = lba * 512;
        let byte_end = byte_offset + buffer.len() as u64;
        
        // Check if we have this range cached
        if self.downloaded_ranges.contains_range(byte_offset, byte_end) {
            // Read from local cache
            return self.local_cache.read_at(byte_offset, buffer);
        }
        
        // Need to fetch from remote
        let chunk_start = (byte_offset / CHUNK_SIZE) * CHUNK_SIZE;
        let chunk_end = ((byte_end + CHUNK_SIZE - 1) / CHUNK_SIZE) * CHUNK_SIZE;
         
        // Fetch chunk
        let data = self.fetch_range(chunk_start, chunk_end).await?;
        
        // Store in local cache
        self.local_cache.write_at(chunk_start, &data)?;
        self.downloaded_ranges.insert(chunk_start, chunk_end);
        
        // Return requested portion
        let offset_in_chunk = (byte_offset - chunk_start) as usize;
        buffer.copy_from_slice(&data[offset_in_chunk..offset_in_chunk + buffer.len()]);
        
        Ok(())
    }
    
    async fn fetch_range(&mut self, start: u64, end: u64) -> Result<Vec<u8>> {
        // Proactively refresh shortly before expiry so long-running sessions don't stall on auth.
        if let Some(expires_at) = self.lease.expires_at {
            if SystemTime::now() + LEASE_REFRESH_SKEW >= expires_at {
                self.refresh_lease().await?;
            }
        }

        // NOTE: Cross-origin requests with `Range` (and `Authorization` when present) trigger a
        // CORS preflight; prefer a same-origin `/disk/...` endpoint to avoid preflight entirely.
        let mut headers = vec![
            ("Range".to_string(), format!("bytes={}-{}", start, end - 1)),
        ];
        if let Some(token) = &self.lease.bearer_token {
            headers.push(("Authorization".to_string(), format!("Bearer {}", token)));
        }

        let mut response = fetch(&self.lease.remote_url, FetchOptions { headers }).await?;
        
        // If the lease expires (or is revoked) mid-run, refresh the lease and retry once.
        if response.status() == 401 || response.status() == 403 {
            self.refresh_lease().await?;

            let mut headers = vec![
                ("Range".to_string(), format!("bytes={}-{}", start, end - 1)),
            ];
            if let Some(token) = &self.lease.bearer_token {
                headers.push(("Authorization".to_string(), format!("Bearer {}", token)));
            }

            response = fetch(&self.lease.remote_url, FetchOptions { headers }).await?;
        }

        Ok(response.bytes().await?)
    }

    async fn refresh_lease(&mut self) -> Result<()> {
        // Fetch a new short-lived disk access lease (remote_url + optional auth) from the backend.
        // The previous token/URL should be treated as secret and discarded.
        self.lease = request_new_lease_from_backend().await?;
        Ok(())
    }
}
```

#### Production backend requirements (Range/CORS/no-transform)

`StreamingDisk.lease.remote_url` is expected to point at a production-grade object delivery path (typically **CDN → S3/object store**) that supports efficient random access via `HTTP Range`.

For full deployment guidance (S3 CORS, CloudFront caching policies, signed cookies/URLs, versioned keys), see: [16 - Remote Disk Image Delivery (Object Store + CDN + HTTP Range)](./16-remote-disk-image-delivery.md)

`StreamingDisk` reads remote disk images by issuing `Range` requests and writing returned bytes directly into a local sparse cache. The remote server **must** behave like a compliant byte-range server; returning a full `200 OK` body (ignoring `Range`) will corrupt the virtual disk because offsets no longer match.

Minimum contract:

- `HEAD` must return `Content-Length` (total size) plus `ETag`/`Last-Modified` for versioning.
- The server **must** support HTTP `Range` requests for the `bytes` unit (`Accept-Ranges: bytes` is recommended).
- For a satisfiable range request, respond with:
  - `206 Partial Content`
  - `Content-Range: bytes <start>-<end>/<total>` (where `<end>` is inclusive)
- For an unsatisfiable range request (past EOF), respond with:
  - `416 Range Not Satisfiable`
  - `Content-Range: bytes */<total>`
- The server should either:
  - implement open-ended ranges (`bytes=<start>-`) and suffix ranges (`bytes=-<suffix-length>`), or
  - explicitly reject them (do not silently ignore the header)
- Multi-range requests (e.g. `Range: bytes=0-0,100-199`) are not used by `StreamingDisk` and are not required for production deployments.
  - Reject multi-range requests explicitly (e.g. `416` or `400`) rather than silently ignoring the header.

Critical implementation constraints:

- Do **not** apply compression or transformations to the disk image response. The client interprets offsets in the raw on-disk byte stream.
  - Recommended: include `Cache-Control: no-transform`
  - Ensure `Content-Encoding` is absent or `identity`
- Cross-origin access (CORS):
  - `Range` is not a CORS-safelisted request header, so browsers will send an `OPTIONS` preflight.
  - The server must allow the request headers used by the client (at minimum `Range`, and `Authorization` if using bearer tokens).
  - The server must expose the response headers needed by the client (see the spec doc for the complete list; typically `Accept-Ranges`, `Content-Range`, `Content-Length`, `ETag`).
- If Aero is deployed with COOP/COEP to enable `SharedArrayBuffer` (`crossOriginIsolated`), disk image resources must be CORS-enabled (`Access-Control-Allow-Origin`) or served with a compatible `Cross-Origin-Resource-Policy` header; otherwise the browser will block the fetch.
  - See [16 - Disk Image Streaming (HTTP Range + Auth + COOP/COEP)](./16-disk-image-streaming-auth.md) for the required CORS/COEP/CORP headers when using authenticated and/or cross-origin streaming.

Concrete examples:

```bash
# Request the first byte (expect 206)
curl -i -H 'Range: bytes=0-0' https://example.com/windows7.img
# HTTP/1.1 206 Partial Content
# Content-Range: bytes 0-0/<total>

# Unsatisfiable range (expect 416)
curl -i -H 'Range: bytes=999999999999-999999999999' https://example.com/windows7.img
# HTTP/1.1 416 Range Not Satisfiable
# Content-Range: bytes */<total>

# Suffix range: last 512 bytes (expect 206)
curl -i -H 'Range: bytes=-512' https://example.com/windows7.img
# HTTP/1.1 206 Partial Content
# Content-Range: bytes <total-512>-<total-1>/<total>
```
### Authenticated / Private Remote Images

The `StreamingDisk` design supports both **public** and **private** remote disk images.

- **Public images**: `remote_url` can point directly at a CDN/object store URL and `bearer_token` can be omitted.
- **Private images**: access is controlled per-user/per-session and requires a short-lived credential (bearer token *or* a signed URL).

For private images, treat the following as **secrets**:

- `remote_url` (for private images it may embed auth via a signed query parameter, or it may be an unguessable lease URL)
- `bearer_token` / session cookies / any auth material (if used)

Do **not** persist these secrets to OPFS/IndexedDB/localStorage (or logs) “for convenience”. Persist stable identifiers instead (e.g., `image_id`, `snapshot_id`) and reacquire credentials each time a VM session starts.

#### Disk access lease acquisition

Before the emulator sets `StreamingDisk.lease.remote_url`, it should obtain a **disk access lease** from a trusted backend (after user authentication/authorization). The lease is a short-lived blob containing the information needed to stream ranges:

- where to fetch (`remote_url`, preferably same-origin like `/disk/<lease_id>`)
- how to authenticate (e.g., `bearer_token`, or a signed URL embedded into `remote_url`)
- when it expires (`expires_at`)

This keeps long-lived credentials out of the browser and enables fine-grained revocation.

#### Token refresh strategy

Disk streaming is continuous during boot and can run for hours, so the client must handle credential expiry:

- **Proactive refresh**: refresh the lease shortly before `expires_at` (e.g., 30-60s skew) to avoid stalling on the first expired request.
- **Reactive refresh**: if any `fetch_range` request returns `401`/`403`, treat the lease as expired/revoked, request a new lease, then retry the same range once. If it still fails, surface a fatal disk I/O error to the VM session.

If you prefer the **signed URL** style instead of an `Authorization` header, the refresh flow is the same, but it updates `remote_url` rather than `bearer_token`.

#### CORS/COEP constraints for Range streaming

- `Range` header → **CORS preflight on cross-origin** requests.
- If cross-origin fetches are unavoidable, configure the disk endpoint to cache preflights via
  `Access-Control-Max-Age` (see the spec doc for recommended values and caveats).
- If using bearer tokens via `Authorization`, cross-origin fetches also require allowing the
  `Authorization` request header in CORS preflight.
- Prefer a **same-origin** disk streaming endpoint (e.g., `/disk/...`) to avoid preflight and simplify COEP/cross-origin isolation.
- If cross-origin is unavoidable, the disk server must return the CORS/COEP/CORP headers defined in [16 - Disk Image Streaming (HTTP Range + Auth + COOP/COEP)](./16-disk-image-streaming-auth.md).

---

## Performance Metrics

| Metric | Target | Measurement |
|--------|--------|-------------|
| Sequential Read | ≥ 100 MB/s | Large file read |
| Sequential Write | ≥ 50 MB/s | Large file write |
| Random Read IOPS | ≥ 10,000 | 4KB random reads |
| Random Write IOPS | ≥ 5,000 | 4KB random writes |
| Boot Time Impact | < 10s | Additional boot delay |

---

## Next Steps

- See [Audio Subsystem](./06-audio-subsystem.md) for sound emulation
- See [Browser APIs](./11-browser-apis.md) for OPFS details
- See [16 - Disk Image Streaming (HTTP Range + Auth + COOP/COEP)](./16-disk-image-streaming-auth.md) for authenticated Range streaming requirements
- See [Task Breakdown](./15-agent-task-breakdown.md) for storage tasks
