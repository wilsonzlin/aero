use alloc::collections::VecDeque;
use alloc::vec::Vec;

use aero_io_snapshot::io::state::codec::{Decoder, Encoder};
use aero_io_snapshot::io::state::{
    IoSnapshot, SnapshotError, SnapshotReader, SnapshotResult, SnapshotVersion, SnapshotWriter,
};

use super::regs;
use super::context::{EndpointContext, SlotContext};
use super::ring::RingCursor;
use super::trb::{CompletionCode, Trb, TRB_LEN};
use super::{
    ActiveEndpoint, ControlTdState, SlotState, XhciController, DEFAULT_PORT_COUNT,
    MAX_CONTROL_DATA_LEN, MAX_PENDING_EVENTS,
};

const TAG_USBCMD: u16 = 1;
const TAG_USBSTS: u16 = 2;
const TAG_CRCR: u16 = 3;
const TAG_PORT_COUNT: u16 = 4;
const TAG_DCBAAP: u16 = 5;
const TAG_INTR0_IMAN: u16 = 6;
const TAG_INTR0_IMOD: u16 = 7;
const TAG_INTR0_ERSTSZ: u16 = 8;
const TAG_INTR0_ERSTBA: u16 = 9;
const TAG_INTR0_ERDP: u16 = 10;
const TAG_PORTS: u16 = 11;
const TAG_HOST_CONTROLLER_ERROR: u16 = 12;
const TAG_CONFIG: u16 = 13;
const TAG_MFINDEX: u16 = 14;

// New in snapshot v0.5.
const TAG_SLOTS: u16 = 15;
const TAG_EVENT_RING_PRODUCER: u16 = 16;
const TAG_PENDING_EVENTS: u16 = 17;
const TAG_DROPPED_EVENT_TRBS: u16 = 18;
const TAG_INTR0_ERST_GEN: u16 = 19;
const TAG_INTR0_ERDP_GEN: u16 = 20;
const TAG_COMMAND_RING: u16 = 21;
const TAG_ACTIVE_ENDPOINTS: u16 = 22;
const TAG_EP0_CONTROL_TD: u16 = 23;
const TAG_CMD_KICK: u16 = 24;
const TAG_DNCTRL: u16 = 25;
const TAG_EP0_CONTROL_TD_FULL: u16 = 26;

// New in snapshot v0.7.
const TAG_TIME_MS: u16 = 26;
const TAG_LAST_TICK_DMA_DWORD: u16 = 27;

const SLOT_CONTEXT_DWORDS: usize = 8;
const ENDPOINT_CONTEXT_DWORDS: usize = 8;

// Defensive bounds against malformed snapshots.
const MAX_SLOT_RECORD_BYTES: usize = 16 * 1024;
const MAX_PORT_RECORD_BYTES: usize = 5 * 1024 * 1024;
const MAX_EP_RECORD_BYTES: usize = 16 * 1024;

fn encode_slot_context(ctx: &SlotContext) -> [u32; SLOT_CONTEXT_DWORDS] {
    let mut out = [0u32; SLOT_CONTEXT_DWORDS];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = ctx.dword(i);
    }
    out
}

fn decode_slot_context(ctx: &mut SlotContext, dwords: [u32; SLOT_CONTEXT_DWORDS]) {
    for (i, dw) in dwords.iter().enumerate() {
        ctx.set_dword(i, *dw);
    }
}

fn encode_endpoint_context(ctx: &EndpointContext) -> [u32; ENDPOINT_CONTEXT_DWORDS] {
    let mut out = [0u32; ENDPOINT_CONTEXT_DWORDS];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = ctx.dword(i);
    }
    out
}

fn decode_endpoint_context(ctx: &mut EndpointContext, dwords: [u32; ENDPOINT_CONTEXT_DWORDS]) {
    for (i, dw) in dwords.iter().enumerate() {
        ctx.set_dword(i, *dw);
    }
}

fn encode_slot_state(slot: &SlotState) -> Vec<u8> {
    let port_id = slot.port_id.unwrap_or(0);

    let mut enc = Encoder::new()
        .bool(slot.enabled)
        .u8(port_id)
        .bool(slot.device_attached)
        .u64(slot.device_context_ptr);

    for dw in encode_slot_context(&slot.slot_context) {
        enc = enc.u32(dw);
    }

    for ep in slot.endpoint_contexts.iter() {
        for dw in encode_endpoint_context(ep) {
            enc = enc.u32(dw);
        }
    }

    for ring in slot.transfer_rings.iter() {
        enc = enc.bool(ring.is_some());
        if let Some(ring) = ring.as_ref() {
            enc = enc.u64(ring.dequeue_ptr()).bool(ring.cycle_state());
        }
    }

    enc.finish()
}

fn decode_slot_state(slot: &mut SlotState, buf: &[u8]) -> SnapshotResult<()> {
    let mut d = Decoder::new(buf);

    slot.enabled = d.bool()?;
    let port_id = d.u8()?;
    slot.port_id = if port_id == 0 { None } else { Some(port_id) };
    slot.device_attached = d.bool()?;
    slot.device_context_ptr = d.u64()?;

    let mut slot_ctx = [0u32; SLOT_CONTEXT_DWORDS];
    for dw in &mut slot_ctx {
        *dw = d.u32()?;
    }
    decode_slot_context(&mut slot.slot_context, slot_ctx);

    for ep in slot.endpoint_contexts.iter_mut() {
        let mut ep_ctx = [0u32; ENDPOINT_CONTEXT_DWORDS];
        for dw in &mut ep_ctx {
            *dw = d.u32()?;
        }
        decode_endpoint_context(ep, ep_ctx);
    }

    for ring in slot.transfer_rings.iter_mut() {
        let present = d.bool()?;
        if present {
            let dequeue_ptr = d.u64()?;
            let cycle = d.bool()?;
            *ring = Some(RingCursor::new(dequeue_ptr, cycle));
        } else {
            *ring = None;
        }
    }

    d.finish()?;
    Ok(())
}

fn encode_slots(slots: &[SlotState]) -> Vec<u8> {
    let mut records = Vec::with_capacity(slots.len());
    for slot in slots {
        records.push(encode_slot_state(slot));
    }
    Encoder::new().vec_bytes(&records).finish()
}

fn decode_slots(slots: &mut [SlotState], buf: &[u8]) -> SnapshotResult<()> {
    let mut d = Decoder::new(buf);
    let count = d.u32()? as usize;
    if count > slots.len() {
        return Err(SnapshotError::InvalidFieldEncoding("xhci slots"));
    }

    for slot in slots.iter_mut().take(count) {
        let len = d.u32()? as usize;
        if len > MAX_SLOT_RECORD_BYTES {
            return Err(SnapshotError::InvalidFieldEncoding("xhci slot record"));
        }
        let rec = d.bytes(len)?;
        decode_slot_state(slot, rec)?;
    }

    d.finish()?;
    Ok(())
}

fn encode_ports(ctrl: &XhciController) -> Vec<u8> {
    let mut records = Vec::with_capacity(ctrl.ports.len());
    for port in &ctrl.ports {
        records.push(port.save_snapshot());
    }
    Encoder::new().vec_bytes(&records).finish()
}

fn decode_ports(ctrl: &mut XhciController, buf: &[u8]) -> SnapshotResult<()> {
    let mut d = Decoder::new(buf);
    let count = d.u32()? as usize;
    if count > ctrl.ports.len() {
        return Err(SnapshotError::InvalidFieldEncoding("xhci ports"));
    }

    for port in ctrl.ports.iter_mut().take(count) {
        let len = d.u32()? as usize;
        if len > MAX_PORT_RECORD_BYTES {
            return Err(SnapshotError::InvalidFieldEncoding("xhci port record"));
        }
        let rec = d.bytes(len)?;
        port.load_snapshot(rec)?;
    }

    d.finish()?;
    Ok(())
}

fn encode_command_ring(cursor: Option<RingCursor>) -> Vec<u8> {
    let mut enc = Encoder::new().bool(cursor.is_some());
    if let Some(c) = cursor {
        enc = enc.u64(c.dequeue_ptr()).bool(c.cycle_state());
    }
    enc.finish()
}

fn decode_command_ring(buf: &[u8]) -> SnapshotResult<Option<RingCursor>> {
    let mut d = Decoder::new(buf);
    let present = d.bool()?;
    let out = if present {
        let ptr = d.u64()?;
        let cycle = d.bool()?;
        Some(RingCursor::new(ptr, cycle))
    } else {
        None
    };
    d.finish()?;
    Ok(out)
}

fn encode_active_endpoints(endpoints: &[ActiveEndpoint]) -> Vec<u8> {
    let mut enc = Encoder::new().u32(endpoints.len() as u32);
    for ep in endpoints {
        enc = enc.u8(ep.slot_id).u8(ep.endpoint_id);
    }
    enc.finish()
}

fn decode_active_endpoints(
    slots_len: usize,
    buf: &[u8],
) -> SnapshotResult<Vec<ActiveEndpoint>> {
    let mut d = Decoder::new(buf);
    let count = d.u32()? as usize;
    // At most 31 endpoints per slot; slot 0 is reserved.
    let max = slots_len.saturating_sub(1).saturating_mul(31);
    if count > max {
        return Err(SnapshotError::InvalidFieldEncoding("xhci active endpoints"));
    }

    let mut out = Vec::new();
    for _ in 0..count {
        let slot_id = d.u8()?;
        let endpoint_id = d.u8()?;
        if slot_id == 0 || usize::from(slot_id) >= slots_len {
            return Err(SnapshotError::InvalidFieldEncoding(
                "xhci active endpoint slot id",
            ));
        }
        if endpoint_id == 0 || endpoint_id > 31 {
            return Err(SnapshotError::InvalidFieldEncoding(
                "xhci active endpoint id",
            ));
        }
        out.push(ActiveEndpoint {
            slot_id,
            endpoint_id,
        });
    }
    d.finish()?;
    Ok(out)
}

fn encode_ep0_control_td(td: &[ControlTdState]) -> Vec<u8> {
    let mut enc = Encoder::new().u32(td.len() as u32);
    for state in td {
        enc = enc.bool(state.td_start.is_some());
        if let Some(cur) = state.td_start {
            enc = enc.u64(cur.dequeue_ptr()).bool(cur.cycle_state());
        }
        enc = enc.bool(state.td_cursor.is_some());
        if let Some(cur) = state.td_cursor {
            enc = enc.u64(cur.dequeue_ptr()).bool(cur.cycle_state());
        }
        enc = enc
            .u32(state.data_expected as u32)
            .u32(state.data_transferred as u32)
            .u8(state.completion_code.raw());
    }
    enc.finish()
}

fn decode_ep0_control_td(dst: &mut [ControlTdState], buf: &[u8]) -> SnapshotResult<()> {
    let mut d = Decoder::new(buf);
    let count = d.u32()? as usize;
    if count > dst.len() {
        return Err(SnapshotError::InvalidFieldEncoding("xhci ep0 td count"));
    }

    // Backwards compatibility: older snapshots stored only (expected, transferred) per entry.
    let legacy_len = 4usize.saturating_add(count.saturating_mul(8));
    let legacy_format = buf.len() == legacy_len;

    for st in dst.iter_mut().take(count) {
        if legacy_format {
            let expected = d.u32()? as usize;
            let transferred = d.u32()? as usize;

            let expected = expected.min(MAX_CONTROL_DATA_LEN);
            let transferred = transferred.min(expected);
            *st = ControlTdState {
                td_start: None,
                td_cursor: None,
                data_expected: expected,
                data_transferred: transferred,
                completion_code: CompletionCode::Success,
            };
            continue;
        }

        let td_start = if d.bool()? {
            let ptr = d.u64()?;
            let cycle = d.bool()?;
            Some(RingCursor::new(ptr, cycle))
        } else {
            None
        };
        let td_cursor = if d.bool()? {
            let ptr = d.u64()?;
            let cycle = d.bool()?;
            Some(RingCursor::new(ptr, cycle))
        } else {
            None
        };

        let expected = d.u32()? as usize;
        let transferred = d.u32()? as usize;
        let cc_raw = d.u8()?;

        let expected = expected.min(MAX_CONTROL_DATA_LEN);
        let transferred = transferred.min(expected);

        let completion_code = match cc_raw {
            0 => CompletionCode::Invalid,
            1 => CompletionCode::Success,
            4 => CompletionCode::UsbTransactionError,
            5 => CompletionCode::TrbError,
            6 => CompletionCode::StallError,
            9 => CompletionCode::NoSlotsAvailableError,
            11 => CompletionCode::SlotNotEnabledError,
            12 => CompletionCode::EndpointNotEnabledError,
            13 => CompletionCode::ShortPacket,
            17 => CompletionCode::ParameterError,
            19 => CompletionCode::ContextStateError,
            _ => {
                return Err(SnapshotError::InvalidFieldEncoding(
                    "xhci completion code",
                ))
            }
        };

        *st = ControlTdState {
            td_start,
            td_cursor,
            data_expected: expected,
            data_transferred: transferred,
            completion_code,
        };
    }

    d.finish()?;
    Ok(())
}

fn encode_ep0_control_td_full(td: &[ControlTdState]) -> Vec<u8> {
    let mut enc = Encoder::new().u32(td.len() as u32);
    for state in td {
        enc = enc.bool(state.td_start.is_some());
        if let Some(cursor) = state.td_start {
            enc = enc.u64(cursor.dequeue_ptr()).bool(cursor.cycle_state());
        }

        enc = enc.bool(state.td_cursor.is_some());
        if let Some(cursor) = state.td_cursor {
            enc = enc.u64(cursor.dequeue_ptr()).bool(cursor.cycle_state());
        }

        enc = enc
            .u32(state.data_expected as u32)
            .u32(state.data_transferred as u32)
            .u8(state.completion_code.raw());
    }
    enc.finish()
}

fn completion_code_from_raw(raw: u8) -> CompletionCode {
    match raw {
        0 => CompletionCode::Invalid,
        1 => CompletionCode::Success,
        4 => CompletionCode::UsbTransactionError,
        5 => CompletionCode::TrbError,
        6 => CompletionCode::StallError,
        9 => CompletionCode::NoSlotsAvailableError,
        11 => CompletionCode::SlotNotEnabledError,
        12 => CompletionCode::EndpointNotEnabledError,
        13 => CompletionCode::ShortPacket,
        17 => CompletionCode::ParameterError,
        19 => CompletionCode::ContextStateError,
        _ => CompletionCode::Invalid,
    }
}

fn decode_ep0_control_td_full(dst: &mut [ControlTdState], buf: &[u8]) -> SnapshotResult<()> {
    let mut d = Decoder::new(buf);
    let count = d.u32()? as usize;
    if count > dst.len() {
        return Err(SnapshotError::InvalidFieldEncoding("xhci ep0 td count"));
    }

    for st in dst.iter_mut().take(count) {
        let start_present = d.bool()?;
        st.td_start = if start_present {
            let ptr = d.u64()?;
            let cycle = d.bool()?;
            if ptr == 0 {
                None
            } else {
                Some(RingCursor::new(ptr, cycle))
            }
        } else {
            None
        };

        let cursor_present = d.bool()?;
        st.td_cursor = if cursor_present {
            let ptr = d.u64()?;
            let cycle = d.bool()?;
            if ptr == 0 {
                None
            } else {
                Some(RingCursor::new(ptr, cycle))
            }
        } else {
            None
        };

        let expected = d.u32()? as usize;
        let transferred = d.u32()? as usize;
        let completion_code_raw = d.u8()?;

        let expected = expected.min(MAX_CONTROL_DATA_LEN);
        let transferred = transferred.min(expected);
        st.data_expected = expected;
        st.data_transferred = transferred;
        st.completion_code = completion_code_from_raw(completion_code_raw);

        // Defensive invariant: if the TD has no start cursor, treat it as not in flight and clear
        // the internal cursor as well.
        if st.td_start.is_none() {
            st.td_cursor = None;
        }
    }

    d.finish()?;
    Ok(())
}

fn encode_pending_events(events: &VecDeque<Trb>) -> Vec<u8> {
    let mut enc = Encoder::new().u32(events.len() as u32);
    for trb in events {
        let bytes = trb.to_bytes();
        enc = enc.bytes(&bytes);
    }
    enc.finish()
}

fn decode_pending_events(buf: &[u8]) -> SnapshotResult<VecDeque<Trb>> {
    let mut d = Decoder::new(buf);
    let count = d.u32()? as usize;
    if count > MAX_PENDING_EVENTS {
        return Err(SnapshotError::InvalidFieldEncoding("xhci pending events"));
    }

    let mut out = VecDeque::new();
    for _ in 0..count {
        let bytes = d.bytes(TRB_LEN)?;
        let mut arr = [0u8; TRB_LEN];
        arr.copy_from_slice(bytes);
        out.push_back(Trb::from_bytes(arr));
    }
    d.finish()?;
    Ok(out)
}

impl IoSnapshot for XhciController {
    const DEVICE_ID: [u8; 4] = *b"XHCI";
    const DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(0, 7);

    fn save_state(&self) -> Vec<u8> {
        let mut w = SnapshotWriter::new(Self::DEVICE_ID, Self::DEVICE_VERSION);

        // Architectural registers.
        w.field_u32(TAG_USBCMD, self.usbcmd & regs::USBCMD_SNAPSHOT_MASK);
        // Store the derived USBSTS view so older snapshots that relied on USBSTS.EINT can still be
        // mapped back into `IMAN.IP` on restore.
        w.field_u32(TAG_USBSTS, self.usbsts_read() & regs::USBSTS_SNAPSHOT_MASK);
        w.field_u64(TAG_CRCR, self.crcr & regs::CRCR_SNAPSHOT_MASK);
        w.field_u8(TAG_PORT_COUNT, self.port_count);
        w.field_u64(TAG_DCBAAP, self.dcbaap & regs::DCBAAP_SNAPSHOT_MASK);
        w.field_u32(TAG_CONFIG, self.config & regs::CONFIG_SNAPSHOT_MASK);
        w.field_u32(TAG_MFINDEX, self.mfindex & regs::runtime::MFINDEX_MASK);
        w.field_u32(TAG_DNCTRL, self.dnctrl);
        w.field_u64(TAG_TIME_MS, self.time_ms);
        w.field_u32(TAG_LAST_TICK_DMA_DWORD, self.last_tick_dma_dword);

        // Interrupter 0 registers + internal generation counters.
        w.field_u32(TAG_INTR0_IMAN, self.interrupter0.iman_raw());
        w.field_u32(TAG_INTR0_IMOD, self.interrupter0.imod_raw());
        w.field_u32(TAG_INTR0_ERSTSZ, self.interrupter0.erstsz_raw());
        w.field_u64(TAG_INTR0_ERSTBA, self.interrupter0.erstba_raw());
        w.field_u64(TAG_INTR0_ERDP, self.interrupter0.erdp_raw());
        // Preserve backward-compatible port state encoding under tag 11.
        w.field_bytes(TAG_PORTS, encode_ports(self));
        w.field_bool(TAG_HOST_CONTROLLER_ERROR, self.host_controller_error);
        w.field_u64(TAG_INTR0_ERST_GEN, self.interrupter0.erst_gen);
        w.field_u64(TAG_INTR0_ERDP_GEN, self.interrupter0.erdp_gen);

        // New controller-local state.
        w.field_bytes(TAG_SLOTS, encode_slots(&self.slots));
        w.field_bytes(TAG_EVENT_RING_PRODUCER, self.event_ring.save_snapshot());
        w.field_bytes(TAG_COMMAND_RING, encode_command_ring(self.command_ring));
        w.field_bool(TAG_CMD_KICK, self.cmd_kick);
        w.field_bytes(
            TAG_ACTIVE_ENDPOINTS,
            encode_active_endpoints(&self.active_endpoints),
        );
        w.field_bytes(TAG_EP0_CONTROL_TD, encode_ep0_control_td(&self.ep0_control_td));
        w.field_bytes(
            TAG_EP0_CONTROL_TD_FULL,
            encode_ep0_control_td_full(&self.ep0_control_td),
        );
        w.field_bytes(TAG_PENDING_EVENTS, encode_pending_events(&self.pending_events));
        w.field_u64(TAG_DROPPED_EVENT_TRBS, self.dropped_event_trbs);

        w.finish()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        let r = SnapshotReader::parse(bytes, Self::DEVICE_ID)?;
        r.ensure_device_major(Self::DEVICE_VERSION.major)?;

        // Snapshot tags must remain stable within a major version, but early xHCI snapshots had a
        // brief period where the header version and the tag layout disagreed:
        // - v0.3 used:
        //   - 11: host_controller_error (bool)
        //   - 12: ports
        //
        // Version 0.4 swapped those tags to resolve a collision introduced in 0.3. Some historical
        // snapshots used the 0.4 header while still encoding fields using the 0.3 tag mapping, so:
        // - choose a default mapping based on snapshot minor version, and
        // - disambiguate based on field shape (host_controller_error is a bool; ports is a
        //   vec-bytes blob).
        let mut tag_host_controller_error = if r.header().device_version.minor <= 3 {
            11u16
        } else {
            TAG_HOST_CONTROLLER_ERROR
        };
        let mut tag_ports = if r.header().device_version.minor <= 3 {
            12u16
        } else {
            TAG_PORTS
        };

        // `host_controller_error` is a bool (single byte 0/1) while `ports` is a nested vec-bytes
        // encoding (>= 4 bytes, starting with a u32 port count). Use that to disambiguate.
        let bytes11 = r.bytes(11);
        let bytes12 = r.bytes(12);
        let is_bool = |bytes: &[u8]| bytes.len() == 1 && (bytes[0] == 0 || bytes[0] == 1);

        let bool_tag = match (bytes11, bytes12) {
            (Some(b11), Some(b12)) => {
                if is_bool(b11) && !is_bool(b12) {
                    Some(11u16)
                } else if is_bool(b12) && !is_bool(b11) {
                    Some(12u16)
                } else {
                    None
                }
            }
            (Some(b11), None) if is_bool(b11) => Some(11u16),
            (None, Some(b12)) if is_bool(b12) => Some(12u16),
            _ => None,
        };

        let ports_tag = match (bytes11, bytes12) {
            (Some(b11), Some(b12)) => {
                if b11.len() >= 4 && b12.len() < 4 {
                    Some(11u16)
                } else if b12.len() >= 4 && b11.len() < 4 {
                    Some(12u16)
                } else {
                    None
                }
            }
            (Some(b11), None) if b11.len() >= 4 => Some(11u16),
            (None, Some(b12)) if b12.len() >= 4 => Some(12u16),
            _ => None,
        };

        if let Some(tag) = bool_tag {
            tag_host_controller_error = tag;
            tag_ports = if tag == 11 { 12 } else { 11 };
        } else if let Some(tag) = ports_tag {
            tag_ports = tag;
            tag_host_controller_error = if tag == 11 { 12 } else { 11 };
        }

        let port_count = r.u8(TAG_PORT_COUNT)?.unwrap_or(DEFAULT_PORT_COUNT).max(1);
        // Preserve existing port/device instances when possible so snapshot restore can apply
        // device state to host-provided passthrough handles instead of reconstructing devices.
        let preserved_ports = if port_count == self.port_count {
            Some(core::mem::take(&mut self.ports))
        } else {
            None
        };

        *self = Self::with_port_count(port_count);
        if let Some(ports) = preserved_ports {
            if ports.len() == self.ports.len() {
                self.ports = ports;
            }
        }

        // Architectural registers.
        self.usbcmd = r.u32(TAG_USBCMD)?.unwrap_or(0) & regs::USBCMD_SNAPSHOT_MASK;
        let saved_usbsts = r.u32(TAG_USBSTS)?.unwrap_or(0) & regs::USBSTS_SNAPSHOT_MASK;
        // USBSTS.EINT/HCH/HCE are derived bits in the controller model.
        self.usbsts = saved_usbsts & !(regs::USBSTS_EINT | regs::USBSTS_HCH | regs::USBSTS_HCE);
        self.host_controller_error = r
            .bool(tag_host_controller_error)?
            .unwrap_or((saved_usbsts & regs::USBSTS_HCE) != 0);
        self.crcr = r.u64(TAG_CRCR)?.unwrap_or(0) & regs::CRCR_SNAPSHOT_MASK;
        // Keep the internal command ring cursor consistent with CRCR. If a newer snapshot provides
        // an explicit cursor image, it will be applied below.
        self.sync_command_ring_from_crcr();
        self.dcbaap = r.u64(TAG_DCBAAP)?.unwrap_or(0) & regs::DCBAAP_SNAPSHOT_MASK;
        self.config = r.u32(TAG_CONFIG)?.unwrap_or(0) & regs::CONFIG_SNAPSHOT_MASK;
        // Clamp to ensure `CONFIG.MaxSlotsEn` remains consistent with `HCSPARAMS1.MaxSlots`.
        let max_slots_en = (self.config & 0xff) as u8;
        self.config = (self.config & !0xff) | u32::from(max_slots_en.min(regs::MAX_SLOTS));
        self.mfindex = r.u32(TAG_MFINDEX)?.unwrap_or(0) & regs::runtime::MFINDEX_MASK;
        self.dnctrl = r.u32(TAG_DNCTRL)?.unwrap_or(0);
        self.time_ms = r.u64(TAG_TIME_MS)?.unwrap_or(0);
        self.last_tick_dma_dword = r.u32(TAG_LAST_TICK_DMA_DWORD)?.unwrap_or(0);
        self.dnctrl = r.u32(TAG_DNCTRL)?.unwrap_or(0);

        if let Some(v) = r.u32(TAG_INTR0_IMAN)? {
            self.interrupter0.restore_iman(v);
        }
        if let Some(v) = r.u32(TAG_INTR0_IMOD)? {
            self.interrupter0.restore_imod(v);
        }
        if let Some(v) = r.u32(TAG_INTR0_ERSTSZ)? {
            self.interrupter0.restore_erstsz(v);
        }
        if let Some(v) = r.u64(TAG_INTR0_ERSTBA)? {
            self.interrupter0.restore_erstba(v);
        }
        if let Some(v) = r.u64(TAG_INTR0_ERDP)? {
            self.interrupter0.restore_erdp(v);
        }

        // Restore generation counters *after* register image so they reflect the saved state.
        self.interrupter0.erst_gen = r.u64(TAG_INTR0_ERST_GEN)?.unwrap_or(0);
        self.interrupter0.erdp_gen = r.u64(TAG_INTR0_ERDP_GEN)?.unwrap_or(0);

        if let Some(buf) = r.bytes(TAG_EVENT_RING_PRODUCER) {
            self.event_ring.load_snapshot(buf)?;
        }

        if let Some(buf) = r.bytes(tag_ports) {
            decode_ports(self, buf)?;
        }

        if let Some(buf) = r.bytes(TAG_SLOTS) {
            decode_slots(&mut self.slots, buf)?;
        }

        // Ensure invalid snapshots cannot panic by indexing ports out of range.
        for slot in self.slots.iter_mut().skip(1) {
            if let Some(port_id) = slot.port_id {
                if port_id == 0 || usize::from(port_id) > self.ports.len() {
                    slot.port_id = None;
                    slot.device_attached = false;
                }
            }
        }

        if let Some(buf) = r.bytes(TAG_PENDING_EVENTS) {
            self.pending_events = decode_pending_events(buf)?;
        }

        if let Some(buf) = r.bytes(TAG_COMMAND_RING) {
            if buf.len() > MAX_EP_RECORD_BYTES {
                return Err(SnapshotError::InvalidFieldEncoding("xhci command ring"));
            }
            self.command_ring = decode_command_ring(buf)?;
            self.sync_crcr_from_command_ring();
        }
        self.cmd_kick = r.bool(TAG_CMD_KICK)?.unwrap_or(false);

        if let Some(buf) = r.bytes(TAG_ACTIVE_ENDPOINTS) {
            if buf.len() > MAX_EP_RECORD_BYTES {
                return Err(SnapshotError::InvalidFieldEncoding("xhci active endpoints"));
            }
            self.active_endpoints = decode_active_endpoints(self.slots.len(), buf)?;
        }

        if let Some(buf) = r.bytes(TAG_EP0_CONTROL_TD_FULL) {
            if buf.len() > MAX_EP_RECORD_BYTES {
                return Err(SnapshotError::InvalidFieldEncoding("xhci ep0 td"));
            }
            decode_ep0_control_td_full(&mut self.ep0_control_td, buf)?;
        } else if let Some(buf) = r.bytes(TAG_EP0_CONTROL_TD) {
            if buf.len() > MAX_EP_RECORD_BYTES {
                return Err(SnapshotError::InvalidFieldEncoding("xhci ep0 td"));
            }
            decode_ep0_control_td(&mut self.ep0_control_td, buf)?;
        }

        self.dropped_event_trbs = r.u64(TAG_DROPPED_EVENT_TRBS)?.unwrap_or(0);

        // Preserve older snapshot behaviour where pending interrupts were captured only in
        // USBSTS.EINT (e.g. the synthetic DMA-on-RUN interrupt).
        if (saved_usbsts & regs::USBSTS_EINT) != 0 {
            self.interrupter0.set_interrupt_pending(true);
        }

        Ok(())
    }
}
