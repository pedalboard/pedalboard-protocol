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
