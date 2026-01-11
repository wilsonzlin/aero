//! USB descriptor "fixups" used by USB passthrough backends.
//!
//! # WebUSB high-speed → full-speed configuration translation
//!
//! When using WebUSB passthrough, the physical device is typically enumerated by the host OS at
//! high-speed. That means `GET_DESCRIPTOR(CONFIGURATION)` returns the *high-speed* configuration
//! descriptor (e.g. bulk endpoints with `wMaxPacketSize = 512`), which a UHCI-only (USB 1.1)
//! guest cannot use.
//!
//! The USB 2.0 spec requires high-speed capable devices to also expose an
//! `OTHER_SPEED_CONFIGURATION` descriptor (descriptor type `0x07`) describing the device's
//! behavior at the opposite speed (full-speed in our case).
//!
//! A WebUSB passthrough backend can therefore do the following when the guest requests a
//! `CONFIGURATION` descriptor (`0x02`) while we are emulating a full-speed connection:
//!
//! 1. Attempt to fetch `OTHER_SPEED_CONFIGURATION` (`0x07`) from the physical device.
//! 2. Convert it to a `CONFIGURATION` descriptor by rewriting only the first descriptor's
//!    `bDescriptorType` byte.
//! 3. Return the rewritten bytes to the guest unchanged otherwise.
//!
//! This is intentionally *not* a general descriptor parser/rewriter; it is the minimal,
//! spec-sanctioned transformation required for safe enumeration.

use thiserror::Error;

const USB_CONFIGURATION_DESCRIPTOR_LEN: usize = 9;
const USB_DESCRIPTOR_TYPE_CONFIGURATION: u8 = 0x02;
const USB_DESCRIPTOR_TYPE_OTHER_SPEED_CONFIGURATION: u8 = 0x07;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DescriptorFixupError {
    #[error("descriptor blob too short: expected at least {expected} bytes, got {actual}")]
    TooShort { expected: usize, actual: usize },

    #[error("unexpected descriptor type: expected {expected:#04x}, got {actual:#04x}")]
    UnexpectedDescriptorType { expected: u8, actual: u8 },

    #[error("wTotalLength ({w_total_length}) exceeds available bytes ({available})")]
    TotalLengthExceedsBuffer {
        w_total_length: usize,
        available: usize,
    },
}

/// Convert an `OTHER_SPEED_CONFIGURATION` descriptor blob (`bDescriptorType == 0x07`) into a
/// `CONFIGURATION` descriptor blob (`bDescriptorType == 0x02`).
///
/// This is a minimal fixup used by USB passthrough backends: the descriptor contents are left
/// unchanged aside from rewriting the first descriptor's `bDescriptorType`.
pub fn other_speed_config_to_config(bytes: &[u8]) -> Result<Vec<u8>, DescriptorFixupError> {
    if bytes.len() < USB_CONFIGURATION_DESCRIPTOR_LEN {
        return Err(DescriptorFixupError::TooShort {
            expected: USB_CONFIGURATION_DESCRIPTOR_LEN,
            actual: bytes.len(),
        });
    }

    let b_descriptor_type = bytes[1];
    if b_descriptor_type == USB_DESCRIPTOR_TYPE_CONFIGURATION {
        // Optional convenience: if the buffer is already a CONFIGURATION descriptor, treat it as
        // a no-op.
        let w_total_length = u16::from_le_bytes([bytes[2], bytes[3]]) as usize;
        if w_total_length > bytes.len() {
            return Err(DescriptorFixupError::TotalLengthExceedsBuffer {
                w_total_length,
                available: bytes.len(),
            });
        }
        return Ok(bytes.to_vec());
    }

    if b_descriptor_type != USB_DESCRIPTOR_TYPE_OTHER_SPEED_CONFIGURATION {
        return Err(DescriptorFixupError::UnexpectedDescriptorType {
            expected: USB_DESCRIPTOR_TYPE_OTHER_SPEED_CONFIGURATION,
            actual: b_descriptor_type,
        });
    }

    let w_total_length = u16::from_le_bytes([bytes[2], bytes[3]]) as usize;
    if w_total_length > bytes.len() {
        return Err(DescriptorFixupError::TotalLengthExceedsBuffer {
            w_total_length,
            available: bytes.len(),
        });
    }

    let mut out = bytes.to_vec();
    out[1] = USB_DESCRIPTOR_TYPE_CONFIGURATION;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_other_speed_config_descriptor() -> Vec<u8> {
        // Minimal descriptor chain:
        // - "Other Speed Configuration" descriptor (type 0x07, length 9)
        // - One interface descriptor (length 9)
        // - One endpoint descriptor (length 7)
        //
        // Total: 25 bytes.
        let w_total_length = 9u16 + 9 + 7;
        let mut bytes = Vec::new();

        bytes.extend_from_slice(&[
            9,    // bLength
            0x07, // bDescriptorType (OTHER_SPEED_CONFIGURATION)
        ]);
        bytes.extend_from_slice(&w_total_length.to_le_bytes()); // wTotalLength
        bytes.extend_from_slice(&[
            1,    // bNumInterfaces
            1,    // bConfigurationValue
            0,    // iConfiguration
            0x80, // bmAttributes
            50,   // bMaxPower
        ]);

        // Interface descriptor.
        bytes.extend_from_slice(&[
            9,    // bLength
            0x04, // bDescriptorType (INTERFACE)
            0,    // bInterfaceNumber
            0,    // bAlternateSetting
            1,    // bNumEndpoints
            0xff, // bInterfaceClass
            0x00, // bInterfaceSubClass
            0x00, // bInterfaceProtocol
            0,    // iInterface
        ]);

        // Endpoint descriptor (bulk IN, FS max packet size 64).
        bytes.extend_from_slice(&[
            7,    // bLength
            0x05, // bDescriptorType (ENDPOINT)
            0x81, // bEndpointAddress
            0x02, // bmAttributes (bulk)
            64, 0, // wMaxPacketSize (64)
            0, // bInterval
        ]);

        assert_eq!(bytes.len(), w_total_length as usize);
        bytes
    }

    #[test]
    fn other_speed_config_to_config_happy_path() {
        let input = synthetic_other_speed_config_descriptor();
        let output = other_speed_config_to_config(&input).unwrap();

        assert_eq!(output.len(), input.len());
        assert_eq!(output[0], 9);
        assert_eq!(output[1], USB_DESCRIPTOR_TYPE_CONFIGURATION);
        assert_eq!(&output[2..], &input[2..]);
    }

    #[test]
    fn other_speed_config_to_config_noop_for_configuration() {
        let mut input = synthetic_other_speed_config_descriptor();
        input[1] = USB_DESCRIPTOR_TYPE_CONFIGURATION;

        let output = other_speed_config_to_config(&input).unwrap();
        assert_eq!(output, input);
    }

    #[test]
    fn other_speed_config_to_config_errors_on_short_input() {
        let err = other_speed_config_to_config(&[0u8; 8]).unwrap_err();
        assert_eq!(
            err,
            DescriptorFixupError::TooShort {
                expected: 9,
                actual: 8
            }
        );
    }

    #[test]
    fn other_speed_config_to_config_errors_on_wrong_type() {
        let mut input = synthetic_other_speed_config_descriptor();
        input[1] = 0x01; // DEVICE descriptor type; anything other than 0x02/0x07 should error.

        let err = other_speed_config_to_config(&input).unwrap_err();
        assert_eq!(
            err,
            DescriptorFixupError::UnexpectedDescriptorType {
                expected: USB_DESCRIPTOR_TYPE_OTHER_SPEED_CONFIGURATION,
                actual: 0x01
            }
        );
    }

    #[test]
    fn other_speed_config_to_config_errors_on_total_length_exceeds_buffer() {
        let mut input = synthetic_other_speed_config_descriptor();
        // Claim a total length larger than the provided buffer.
        let too_long = (input.len() + 1) as u16;
        input[2..4].copy_from_slice(&too_long.to_le_bytes());

        let err = other_speed_config_to_config(&input).unwrap_err();
        assert_eq!(
            err,
            DescriptorFixupError::TotalLengthExceedsBuffer {
                w_total_length: input.len() + 1,
                available: input.len()
            }
        );
    }
}
