//! MIDI routing types: port bitmask and tagged output messages.
//!
//! The controller uses these to express routing decisions.
//! Firmware maps port bits to physical hardware (UART, USB, BLE).

use bitflags::bitflags;

bitflags! {
    /// Bitmask of MIDI ports. Extensible — new ports are new bits.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct MidiPort: u8 {
        /// DIN 5-pin MIDI (UART)
        const DIN = 0x01;
        /// USB MIDI
        const USB = 0x02;
        /// Bluetooth LE MIDI (future)
        const BLE = 0x04;
    }
}

impl MidiPort {
    /// All currently defined ports.
    pub const ALL: Self = Self::DIN.union(Self::USB);
}

/// A MIDI message tagged with destination port(s).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MidiOut {
    /// Raw MIDI bytes (fits MIDI 1.0 and future MIDI 2.0 UMP).
    pub data: [u8; 8],
    /// Number of valid bytes in `data`.
    pub len: u8,
    /// Destination ports (bitmask).
    pub dest: MidiPort,
}

impl MidiOut {
    /// Create a MIDI 1.0 channel message (up to 3 bytes) for the given destination(s).
    pub fn new(data: &[u8], dest: MidiPort) -> Self {
        let mut buf = [0u8; 8];
        let len = data.len().min(8);
        buf[..len].copy_from_slice(&data[..len]);
        Self {
            data: buf,
            len: len as u8,
            dest,
        }
    }

    /// The message bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.data[..self.len as usize]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_single_bit() {
        assert_eq!(MidiPort::DIN.bits(), 0x01);
        assert_eq!(MidiPort::USB.bits(), 0x02);
        assert_eq!(MidiPort::BLE.bits(), 0x04);
    }

    #[test]
    fn port_union() {
        let both = MidiPort::DIN | MidiPort::USB;
        assert!(both.contains(MidiPort::DIN));
        assert!(both.contains(MidiPort::USB));
        assert!(!both.contains(MidiPort::BLE));
    }

    #[test]
    fn port_all_includes_din_and_usb() {
        assert!(MidiPort::ALL.contains(MidiPort::DIN));
        assert!(MidiPort::ALL.contains(MidiPort::USB));
    }

    #[test]
    fn port_empty() {
        let empty = MidiPort::empty();
        assert!(!empty.contains(MidiPort::DIN));
        assert!(!empty.contains(MidiPort::USB));
        assert!(empty.is_empty());
    }

    #[test]
    fn midi_out_new_3byte() {
        let msg = MidiOut::new(&[0x90, 60, 127], MidiPort::USB);
        assert_eq!(msg.len, 3);
        assert_eq!(msg.bytes(), &[0x90, 60, 127]);
        assert_eq!(msg.dest, MidiPort::USB);
    }

    #[test]
    fn midi_out_new_multi_dest() {
        let msg = MidiOut::new(&[0xB0, 7, 100], MidiPort::DIN | MidiPort::USB);
        assert!(msg.dest.contains(MidiPort::DIN));
        assert!(msg.dest.contains(MidiPort::USB));
        assert!(!msg.dest.contains(MidiPort::BLE));
    }

    #[test]
    fn midi_out_8byte_ump() {
        let data = [0x40, 0x90, 0x3C, 0x00, 0x7F, 0x00, 0x00, 0x00];
        let msg = MidiOut::new(&data, MidiPort::ALL);
        assert_eq!(msg.len, 8);
        assert_eq!(msg.bytes(), &data);
    }

    #[test]
    fn midi_out_truncates_oversize() {
        let data = [0u8; 16]; // larger than 8
        let msg = MidiOut::new(&data, MidiPort::DIN);
        assert_eq!(msg.len, 8); // capped at 8
    }
}
