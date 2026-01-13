//! Minimal HD Audio codec model.
//!
//! The purpose of this codec is not to model a particular piece of hardware
//! perfectly, but to provide enough of the verb surface for Windows 7 to build
//! an audio endpoint and send PCM.

use std::collections::BTreeMap;

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct CodecAddr(pub u8);

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct Nid(pub u8);

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct HdaVerbResponse {
    pub data: u32,
    pub ext: u32,
}

impl HdaVerbResponse {
    pub fn encode(self) -> u64 {
        ((self.ext as u64) << 32) | (self.data as u64)
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct CodecCmd {
    pub codec: CodecAddr,
    pub nid: Nid,
    pub verb: u32,
}

impl CodecCmd {
    pub fn decode(cmd: u32) -> Self {
        let codec = ((cmd >> 28) & 0xF) as u8;
        let nid = ((cmd >> 20) & 0x7F) as u8;
        let verb = cmd & 0xFFFFF;
        Self {
            codec: CodecAddr(codec),
            nid: Nid(nid),
            verb,
        }
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
enum NodeKind {
    Root,
    AudioFunctionGroup,
    AudioOutputConverter,
    AudioInputConverter,
    PinComplex,
}

#[derive(Debug, Clone)]
struct NodeState {
    kind: NodeKind,
    // Raw verb-backed state.
    converter_format: u16,
    stream_channel: u8,
    amp_gain_mute: u16,
    pin_widget_ctl: u8,
    conn_select: u8,
    config_default: u32,
}

impl NodeState {
    fn new(kind: NodeKind) -> Self {
        Self {
            kind,
            converter_format: match kind {
                NodeKind::AudioInputConverter => 0x0010, // 48kHz, 16-bit, mono
                _ => 0x0011,                             // 48kHz, 16-bit, 2ch (typical default)
            },
            stream_channel: 0,
            amp_gain_mute: 0,
            pin_widget_ctl: 0,
            conn_select: 0,
            config_default: 0,
        }
    }
}

#[derive(Debug)]
pub struct HdaCodec {
    vendor_id: u32,
    revision_id: u32,
    subsystem_id: u32,

    nodes: BTreeMap<u8, NodeState>,
}

impl HdaCodec {
    pub fn new_minimal() -> Self {
        // Use a Realtek-ish vendor/device ID. Windows' generic "High Definition
        // Audio Device" driver is happy as long as the topology is coherent.
        let vendor_id = 0x10ec_0662; // Realtek ALC662
        let revision_id = 0x0001_0000;
        let subsystem_id = 0x0000_0000;

        let mut codec = Self {
            vendor_id,
            revision_id,
            subsystem_id,
            nodes: BTreeMap::new(),
        };

        codec.nodes.insert(0, NodeState::new(NodeKind::Root));
        codec
            .nodes
            .insert(1, NodeState::new(NodeKind::AudioFunctionGroup));
        codec
            .nodes
            .insert(2, NodeState::new(NodeKind::AudioOutputConverter));
        let mut pin = NodeState::new(NodeKind::PinComplex);
        // Config default: sequence=0, association=1, line-out, jack.
        // This is a "plausible" line-out jack; Windows uses it to build endpoints.
        pin.config_default = 0x0101_0010;
        codec.nodes.insert(3, pin);

        // Microphone input path: input converter + input pin.
        codec
            .nodes
            .insert(4, NodeState::new(NodeKind::AudioInputConverter));
        let mut mic_pin = NodeState::new(NodeKind::PinComplex);
        // Config default: microphone input jack.
        mic_pin.config_default = 0x01A1_0010;
        codec.nodes.insert(5, mic_pin);

        codec
    }

    pub fn vendor_id(&self) -> u32 {
        self.vendor_id
    }

    pub fn reset(&mut self) {
        *self = Self::new_minimal();
    }

    pub fn execute_verb(&mut self, nid: Nid, verb: u32) -> u32 {
        let verb_id = ((verb >> 8) & 0xFFF) as u16;
        let payload = (verb & 0xFF) as u8;

        // Some verbs use a 16-bit payload split across verb_id low 8 bits and payload.
        let payload16 = ((verb_id & 0xFF) << 8) | u16::from(payload);

        let node_id = nid.0;
        let kind = match self.nodes.get(&node_id) {
            Some(node) => node.kind,
            None => return 0,
        };

        match verb_id {
            0xF00 => self.get_parameter(node_id, kind, payload),
            0xF01 => self
                .nodes
                .get(&node_id)
                .map(|n| n.conn_select as u32)
                .unwrap_or(0), // GET_CONNECTION_SELECT
            0xF02 => self.get_conn_list_entry(node_id, payload),
            0xA00 => self
                .nodes
                .get(&node_id)
                .map(|n| n.converter_format as u32)
                .unwrap_or(0), // GET_CONVERTER_FORMAT
            0xF06 => self
                .nodes
                .get(&node_id)
                .map(|n| n.stream_channel as u32)
                .unwrap_or(0), // GET_STREAM_CHANNEL
            0xF07 => self
                .nodes
                .get(&node_id)
                .map(|n| n.pin_widget_ctl as u32)
                .unwrap_or(0), // GET_PIN_WIDGET_CONTROL
            0xF1C => self
                .nodes
                .get(&node_id)
                .map(|n| n.config_default)
                .unwrap_or(0), // GET_CONFIGURATION_DEFAULT
            0xB00 => self
                .nodes
                .get(&node_id)
                .map(|n| n.amp_gain_mute as u32)
                .unwrap_or(0), // GET_AMP_GAIN_MUTE (raw)
            0x701 => {
                if let Some(node) = self.nodes.get_mut(&node_id) {
                    node.conn_select = payload;
                }
                0
            }
            0x200..=0x2FF => {
                if let Some(node) = self.nodes.get_mut(&node_id) {
                    node.converter_format = payload16;
                }
                0
            }
            0x706 => {
                if let Some(node) = self.nodes.get_mut(&node_id) {
                    node.stream_channel = payload;
                }
                0
            }
            0x707 => {
                if let Some(node) = self.nodes.get_mut(&node_id) {
                    node.pin_widget_ctl = payload;
                }
                0
            }
            0x300..=0x3FF => {
                if let Some(node) = self.nodes.get_mut(&node_id) {
                    node.amp_gain_mute = payload16;
                }
                0
            }
            _ => 0,
        }
    }

    fn get_parameter(&self, nid: u8, kind: NodeKind, param: u8) -> u32 {
        match param {
            0x00 => self.vendor_id,
            0x01 => self.subsystem_id,
            0x02 => self.revision_id,
            0x04 => match nid {
                0 => (1u32 << 16) | 1, // start=1, count=1 (one function group)
                1 => (2u32 << 16) | 4, // start=2, count=4 (out converter+pin, in converter+pin)
                _ => 0,
            },
            0x05 => {
                if nid == 1 {
                    0x01 // Audio Function Group
                } else {
                    0
                }
            }
            0x08 => 0, // Audio Function Group Capabilities (minimal)
            0x09 => audio_widget_caps(kind),
            0x0A => match nid {
                2 => supported_pcm(),
                3 => supported_pcm(),
                4 => supported_pcm(),
                5 => supported_pcm(),
                _ => 0,
            },
            0x0B => match nid {
                2 => 1, // PCM stream format supported
                3 => 1,
                4 => 1,
                5 => 1,
                _ => 0,
            },
            0x0C => match nid {
                3 => pin_caps_output(),
                5 => pin_caps_input(),
                _ => 0,
            },
            0x0D => 0, // AMP IN caps (none)
            0x0E => match nid {
                3 => 1, // one connection (to NID 2)
                5 => 1, // one connection (to NID 4)
                _ => 0,
            },
            0x12 => amp_out_caps(), // AMP OUT caps (basic)
            _ => 0,
        }
    }

    fn get_conn_list_entry(&self, nid: u8, index: u8) -> u32 {
        if nid != 3 && nid != 5 {
            return 0;
        }
        // Short-form connection list with a single entry to the converter.
        // The GET verb returns up to 4 entries per request; index selects the 4-entry block.
        if index != 0 {
            return 0;
        }
        match nid {
            3 => 2, // output pin -> output converter
            5 => 4, // mic pin -> input converter
            _ => 0,
        }
    }
}

fn audio_widget_caps(kind: NodeKind) -> u32 {
    // Only the widget type bits really matter for basic enumeration.
    // Widget Type is bits 23:20.
    match kind {
        NodeKind::AudioOutputConverter => 0x0000_0001, // stereo + output widget (type 0)
        NodeKind::AudioInputConverter => (0x1u32 << 20) | 0x0000_0001, // stereo + input widget (type 1)
        NodeKind::PinComplex => (0x4u32 << 20) | 0x0000_0001,
        _ => 0,
    }
}

fn supported_pcm() -> u32 {
    // PCM Size, Rate Capabilities (HDA spec).
    // Advertise just enough for the common initial format Windows/Linux use:
    // - 16-bit samples
    // - 44.1 kHz and 48 kHz
    //
    // Bits:
    // - sample sizes: 8/16/20/24/32 at bits 0..4
    // - sample rates: 44.1k (bit 13) and 48k (bit 14)
    (1 << 1) | (1 << 13) | (1 << 14)
}

fn pin_caps_output() -> u32 {
    // Output capable + presence detect.
    0x0000_0010 | 0x0000_0004
}

fn pin_caps_input() -> u32 {
    // Input capable + presence detect.
    0x0000_0020 | 0x0000_0004
}

fn amp_out_caps() -> u32 {
    // Minimal non-zero amp caps: 0 steps, 0 offset.
    0x0000_0000
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: Nid = Nid(0);
    const AFG: Nid = Nid(1);
    const OUT_CONV: Nid = Nid(2);
    const OUT_PIN: Nid = Nid(3);
    const IN_CONV: Nid = Nid(4);
    const MIC_PIN: Nid = Nid(5);

    fn verb_get_parameter(param: u8) -> u32 {
        (0xF00u32 << 8) | (param as u32)
    }

    fn verb_get_configuration_default() -> u32 {
        0xF1Cu32 << 8
    }

    fn verb_get_converter_format() -> u32 {
        0xA00u32 << 8
    }

    fn verb_set_converter_format(format: u16) -> u32 {
        // SET verbs with a 16-bit payload encode the high payload bits in the low 8 bits of
        // the 12-bit verb ID (HDA spec §7.3.3.1).
        let verb_id = 0x200u32 | u32::from((format >> 8) as u8);
        (verb_id << 8) | u32::from(format as u8)
    }

    fn verb_get_stream_channel() -> u32 {
        0xF06u32 << 8
    }

    fn verb_set_stream_channel(stream_chan: u8) -> u32 {
        (0x706u32 << 8) | u32::from(stream_chan)
    }

    #[test]
    fn get_parameter_exposes_basic_ids_and_topology() {
        let mut codec = HdaCodec::new_minimal();

        // IDs.
        assert_eq!(
            codec.execute_verb(ROOT, verb_get_parameter(0x00)),
            codec.vendor_id()
        );
        assert_eq!(
            codec.execute_verb(ROOT, verb_get_parameter(0x01)),
            0x0000_0000
        );
        assert_eq!(
            codec.execute_verb(ROOT, verb_get_parameter(0x02)),
            0x0001_0000
        );

        // Root: one function group at NID 1.
        let root_subnodes = codec.execute_verb(ROOT, verb_get_parameter(0x04));
        assert_eq!(root_subnodes >> 16, 1);
        assert_eq!(root_subnodes & 0xFF, 1);

        // Audio Function Group: 4 nodes at NID 2..=5.
        let afg_subnodes = codec.execute_verb(AFG, verb_get_parameter(0x04));
        assert_eq!(afg_subnodes >> 16, 2);
        assert_eq!(afg_subnodes & 0xFF, 4);

        // Function group type.
        assert_eq!(codec.execute_verb(AFG, verb_get_parameter(0x05)), 0x01);

        // Ensure the enumerated NIDs exist by asking for widget caps (param 0x09).
        for nid in 2..=5 {
            let caps = codec.execute_verb(Nid(nid), verb_get_parameter(0x09));
            assert_ne!(caps, 0, "expected non-zero widget caps for NID {nid}");
        }
    }

    #[test]
    fn pin_configuration_defaults_are_non_zero_and_plausible() {
        let mut codec = HdaCodec::new_minimal();

        let out_cfg = codec.execute_verb(OUT_PIN, verb_get_configuration_default());
        let mic_cfg = codec.execute_verb(MIC_PIN, verb_get_configuration_default());

        assert_ne!(out_cfg, 0);
        assert_ne!(mic_cfg, 0);

        // Bits 23:20 are "Default Device" (HDA spec §7.3.4.13). Ensure the
        // line-out and microphone pins don't claim the same device type.
        let out_dev = (out_cfg >> 20) & 0xF;
        let mic_dev = (mic_cfg >> 20) & 0xF;
        assert_ne!(out_dev, mic_dev);

        // Default Device values: 0x0 = Line Out, 0xA = Mic In. We don't care about
        // exact bitfields beyond basic plausibility.
        assert_eq!(out_dev, 0x0);
        assert_eq!(mic_dev, 0xA);
    }

    #[test]
    fn converter_state_round_trips_and_reset_restores_defaults() {
        let mut codec = HdaCodec::new_minimal();

        // Converter format (output + input).
        let out_default_fmt = codec.execute_verb(OUT_CONV, verb_get_converter_format()) as u16;
        let in_default_fmt = codec.execute_verb(IN_CONV, verb_get_converter_format()) as u16;
        assert_eq!(out_default_fmt, 0x0011);
        assert_eq!(in_default_fmt, 0x0010);

        let new_fmt = 0x1234;
        codec.execute_verb(OUT_CONV, verb_set_converter_format(new_fmt));
        assert_eq!(
            codec.execute_verb(OUT_CONV, verb_get_converter_format()) as u16,
            new_fmt
        );

        // Stream/channel ID.
        assert_eq!(
            codec.execute_verb(OUT_CONV, verb_get_stream_channel()) as u8,
            0
        );
        codec.execute_verb(OUT_CONV, verb_set_stream_channel(0x3A));
        assert_eq!(
            codec.execute_verb(OUT_CONV, verb_get_stream_channel()) as u8,
            0x3A
        );

        // Reset must restore defaults.
        codec.reset();
        assert_eq!(
            codec.execute_verb(OUT_CONV, verb_get_converter_format()) as u16,
            out_default_fmt
        );
        assert_eq!(
            codec.execute_verb(IN_CONV, verb_get_converter_format()) as u16,
            in_default_fmt
        );
        assert_eq!(
            codec.execute_verb(OUT_CONV, verb_get_stream_channel()) as u8,
            0
        );
    }
}
