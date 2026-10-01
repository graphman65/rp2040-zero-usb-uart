#![no_std]

/// CDC wire-format line coding. Unsupported settings are rejected, never coerced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineCoding {
    pub baud: u32,
    pub stop_bits: u8,
    pub parity: u8,
    pub data_bits: u8,
}

impl LineCoding {
    pub const DEFAULT: Self = Self {
        baud: 38_400,
        stop_bits: 0,
        parity: 0,
        data_bits: 8,
    };

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != 7 {
            return None;
        }

        let coding = Self {
            baud: u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            stop_bits: bytes[4],
            parity: bytes[5],
            data_bits: bytes[6],
        };

        if !(1_200..=921_600).contains(&coding.baud)
            || !matches!(coding.stop_bits, 0 | 2)
            || coding.parity > 2
            || !(5..=8).contains(&coding.data_bits)
        {
            return None;
        }

        Some(coding)
    }

    pub fn encode(self) -> [u8; 7] {
        let baud = self.baud.to_le_bytes();
        [
            baud[0],
            baud[1],
            baud[2],
            baud[3],
            self.stop_bits,
            self.parity,
            self.data_bits,
        ]
    }
}

/// A session ends on USB deconfiguration or DTR falling. Baud changes are not
/// session changes: previously accepted TX bytes still finish at the old baud.
#[derive(Clone, Copy, Debug)]
pub struct Session {
    pub coding: LineCoding,
    pub configured: bool,
    pub dtr: bool,
    pub epoch: u32,
}

impl Session {
    pub const NEW: Self = Self {
        coding: LineCoding::DEFAULT,
        configured: false,
        dtr: false,
        epoch: 0,
    };

    pub fn active(self) -> bool {
        self.configured && self.dtr
    }

    pub fn set_configured(&mut self, configured: bool) {
        if self.configured != configured {
            self.epoch = self.epoch.wrapping_add(1);
        }

        self.configured = configured;
        if !configured {
            self.dtr = false;
        }
    }

    pub fn set_dtr(&mut self, dtr: bool) {
        if self.dtr != dtr {
            self.epoch = self.epoch.wrapping_add(1);
        }

        self.dtr = dtr;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_cdc_wire_format_without_swapping_parity_or_stop_bits() {
        let wire = [0x00, 0xc2, 0x01, 0x00, 2, 1, 7];
        let expected = LineCoding {
            baud: 115_200,
            stop_bits: 2,
            parity: 1,
            data_bits: 7,
        };

        assert_eq!(LineCoding::decode(&wire), Some(expected));
        assert_eq!(expected.encode(), wire);
    }

    #[test]
    fn supported_framing_round_trips() {
        for baud in [1_200, 38_400, 115_200, 921_600] {
            for data_bits in 5..=8 {
                for parity in 0..=2 {
                    for stop_bits in [0, 2] {
                        let coding = LineCoding {
                            baud,
                            data_bits,
                            parity,
                            stop_bits,
                        };
                        assert_eq!(LineCoding::decode(&coding.encode()), Some(coding));
                    }
                }
            }
        }
    }

    #[test]
    fn invalid_requests_are_not_silently_coerced() {
        for baud in [0, 1_199, 921_601, u32::MAX] {
            assert_eq!(
                LineCoding::decode(
                    &LineCoding {
                        baud,
                        ..LineCoding::DEFAULT
                    }
                    .encode()
                ),
                None
            );
        }

        for (index, value) in [(4, 1), (4, 3), (5, 3), (5, 4), (6, 4), (6, 9)] {
            let mut bytes = LineCoding::DEFAULT.encode();
            bytes[index] = value;
            assert_eq!(LineCoding::decode(&bytes), None);
        }

        assert_eq!(LineCoding::decode(&[]), None);
        assert_eq!(LineCoding::decode(&[0; 8]), None);
    }

    #[test]
    fn close_and_reconnect_invalidate_old_receive_data() {
        let mut session = Session::NEW;
        session.set_configured(true);
        session.set_dtr(true);
        assert!(session.active());

        let epoch = session.epoch;
        session.coding.baud = 115_200;
        assert_eq!(session.epoch, epoch);

        session.set_dtr(false);
        assert!(!session.active());
        assert_ne!(session.epoch, epoch);

        session.set_dtr(true);
        let epoch = session.epoch;
        session.set_configured(false);
        session.set_configured(true);
        assert!(!session.active());
        assert_ne!(session.epoch, epoch);
    }
}
