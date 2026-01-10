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
        let payload16 = ((verb_id as u16 & 0xFF) << 8) | payload as u16;

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
            0x08 => {
                if nid == 1 {
                    0 // Audio Function Group Capabilities (minimal)
                } else {
                    0
                }
            }
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
    // Supported PCM sizes/rates. This is intentionally conservative:
    // 44.1/48 kHz, 16-bit.
    // Bits are codec specific in spec; drivers generally only need non-zero.
    0x0000_0011
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
