//! Minimal MIDI-CI Property Exchange (PE) framing.
//!
//! Handles Set Property Inquiry (sub-ID2 = 0x36) and replies with ACK (0x37).
//! Spec reference: MIDI-CI 1.2, §7 Property Exchange.
//!
//! SysEx7 message layout:
//!   F0 7E <device_id> 0D <sub_id2> <ci_version>
//!   <source_muid: 4 bytes> <dest_muid: 4 bytes>
//!   <request_id>
//!   <header_len_lo> <header_len_hi>
//!   <header_data...>
//!   <num_chunks_lo> <num_chunks_hi>
//!   <chunk_num_lo> <chunk_num_hi>
//!   <body_len_lo> <body_len_hi>
//!   <body_data...>
//!   F7

use heapless::Vec;

const UNIVERSAL_SYSEX: u8 = 0x7E;
const SUB_ID1_MIDI_CI: u8 = 0x0D;

/// Sub-ID2: Inquiry Set Property Data (MIDI-CI 1.2)
pub const PE_SET_INQUIRY: u8 = 0x36;
/// Sub-ID2: Reply to Set Property Data
pub const PE_SET_REPLY: u8 = 0x37;

const CI_VERSION: u8 = 0x02;

/// Check if a SysEx buffer is a MIDI-CI message.
pub fn is_ci_message(buf: &[u8]) -> bool {
    buf.len() >= 15 && buf[0] == 0xF0 && buf[1] == UNIVERSAL_SYSEX && buf[3] == SUB_ID1_MIDI_CI
}

/// Check if the message is a Set Property Inquiry.
pub fn is_set_property(buf: &[u8]) -> bool {
    is_ci_message(buf) && buf[4] == PE_SET_INQUIRY
}

/// Extract the request_id field.
pub fn request_id(buf: &[u8]) -> u8 {
    buf[14]
}

/// Extract the source MUID (4 bytes).
pub fn source_muid(buf: &[u8]) -> [u8; 4] {
    [buf[6], buf[7], buf[8], buf[9]]
}

/// Extract the body payload from a Set Property Inquiry.
pub fn extract_body(buf: &[u8]) -> Option<&[u8]> {
    if !is_set_property(buf) {
        return None;
    }
    if buf.len() < 16 {
        return None;
    }

    let mut pos = 15;

    // header_len (2 bytes, 7-bit LSB encoding)
    if pos + 2 > buf.len() {
        return None;
    }
    let header_len = (buf[pos] as usize) | ((buf[pos + 1] as usize) << 7);
    pos += 2 + header_len;

    // num_chunks + chunk_num (4 bytes)
    if pos + 4 > buf.len() {
        return None;
    }
    pos += 4;

    // body_len (2 bytes, 7-bit LSB encoding)
    if pos + 2 > buf.len() {
        return None;
    }
    let body_len = (buf[pos] as usize) | ((buf[pos + 1] as usize) << 7);
    pos += 2;

    if pos + body_len > buf.len() {
        return None;
    }
    Some(&buf[pos..pos + body_len])
}

/// Build a Set Property Reply (ACK).
pub fn build_set_reply(device_muid: [u8; 4], dest_muid: [u8; 4], req_id: u8) -> Vec<u8, 32> {
    let mut msg: Vec<u8, 32> = Vec::new();
    let _ = msg.push(0xF0);
    let _ = msg.push(UNIVERSAL_SYSEX);
    let _ = msg.push(0x7F); // device_id: function block
    let _ = msg.push(SUB_ID1_MIDI_CI);
    let _ = msg.push(PE_SET_REPLY);
    let _ = msg.push(CI_VERSION);
    for &b in &device_muid {
        let _ = msg.push(b);
    }
    for &b in &dest_muid {
        let _ = msg.push(b);
    }
    let _ = msg.push(req_id);
    // header_len = 0
    let _ = msg.push(0x00);
    let _ = msg.push(0x00);
    // num_chunks = 1, chunk_num = 1
    let _ = msg.push(0x01);
    let _ = msg.push(0x00);
    let _ = msg.push(0x01);
    let _ = msg.push(0x00);
    // body_len = 0
    let _ = msg.push(0x00);
    let _ = msg.push(0x00);
    let _ = msg.push(0xF7);
    msg
}

/// Build a Set Property Inquiry message (CLI → device).
/// `body` must contain only 7-bit safe bytes.
pub fn build_set_inquiry(
    source_muid: [u8; 4],
    dest_muid: [u8; 4],
    req_id: u8,
    body: &[u8],
) -> Vec<u8, 256> {
    let mut msg: Vec<u8, 256> = Vec::new();
    let _ = msg.push(0xF0);
    let _ = msg.push(UNIVERSAL_SYSEX);
    let _ = msg.push(0x7F);
    let _ = msg.push(SUB_ID1_MIDI_CI);
    let _ = msg.push(PE_SET_INQUIRY);
    let _ = msg.push(CI_VERSION);
    for &b in &source_muid {
        let _ = msg.push(b);
    }
    for &b in &dest_muid {
        let _ = msg.push(b);
    }
    let _ = msg.push(req_id);
    // header_len = 0
    let _ = msg.push(0x00);
    let _ = msg.push(0x00);
    // num_chunks = 1, chunk_num = 1
    let _ = msg.push(0x01);
    let _ = msg.push(0x00);
    let _ = msg.push(0x01);
    let _ = msg.push(0x00);
    // body_len
    let _ = msg.push((body.len() & 0x7F) as u8);
    let _ = msg.push(((body.len() >> 7) & 0x7F) as u8);
    for &b in body {
        let _ = msg.push(b);
    }
    let _ = msg.push(0xF7);
    msg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_set_property() {
        let payload = b"hello";
        let msg = build_set_inquiry([0x10, 0x20, 0x30, 0x40], [0x01, 0x02, 0x03, 0x04], 0x07, payload);

        assert!(is_ci_message(&msg));
        assert!(is_set_property(&msg));
        assert_eq!(request_id(&msg), 0x07);
        assert_eq!(source_muid(&msg), [0x10, 0x20, 0x30, 0x40]);
        assert_eq!(extract_body(&msg).unwrap(), b"hello");
    }

    #[test]
    fn not_ci_for_opendeck() {
        let buf = [0xF0, 0x00, 0x53, 0x43, 0x00, 0x00, 0x01, 0xF7];
        assert!(!is_ci_message(&buf));
    }

    #[test]
    fn reply_structure() {
        let reply = build_set_reply([0x01, 0x02, 0x03, 0x04], [0x10, 0x20, 0x30, 0x40], 0x07);
        assert_eq!(reply[0], 0xF0);
        assert_eq!(reply[4], PE_SET_REPLY);
        assert_eq!(reply[14], 0x07);
        assert_eq!(*reply.last().unwrap(), 0xF7);
    }

    #[test]
    fn extract_body_with_header() {
        let mut msg: Vec<u8, 128> = Vec::new();
        let _ = msg.push(0xF0);
        let _ = msg.push(UNIVERSAL_SYSEX);
        let _ = msg.push(0x7F);
        let _ = msg.push(SUB_ID1_MIDI_CI);
        let _ = msg.push(PE_SET_INQUIRY);
        let _ = msg.push(CI_VERSION);
        for &b in &[0x10, 0x20, 0x30, 0x40, 0x01, 0x02, 0x03, 0x04] {
            let _ = msg.push(b);
        }
        let _ = msg.push(0x01); // request_id
        // header_len = 3
        let _ = msg.push(0x03);
        let _ = msg.push(0x00);
        for &b in b"abc" {
            let _ = msg.push(b);
        }
        // chunks
        let _ = msg.push(0x01);
        let _ = msg.push(0x00);
        let _ = msg.push(0x01);
        let _ = msg.push(0x00);
        // body_len = 4
        let _ = msg.push(0x04);
        let _ = msg.push(0x00);
        for &b in b"data" {
            let _ = msg.push(b);
        }
        let _ = msg.push(0xF7);

        assert_eq!(extract_body(&msg).unwrap(), b"data");
    }
}
