//! Client-to-host 0x5502, SS_FRAME_FEC_STATUS in moonlight-common-c's
//! pinned 2600beaf13f18bfa43453609cf5e3b84a4227760 src/Video.h.
use anyhow::{Result, ensure};

#[derive(Debug, PartialEq, Eq)]
pub struct Status {
    pub frame: u32,
    pub highest_received: u16,
    pub next_contiguous: u16,
    pub missing_before_highest: u16,
    pub data: u16,
    pub parity: u16,
    pub received_data: u16,
    pub received_parity: u16,
    pub percentage: u8,
    pub block: u8,
    pub blocks: u8,
}
impl Status {
    pub fn parse(payload: &[u8]) -> Result<Self> {
        ensure!(payload.len() == 21, "invalid FEC status length");
        // Packed u32, seven u16s and three bytes; unlike the control header,
        // every multibyte field in this payload is big-endian.
        let word = |offset| u16::from_be_bytes([payload[offset], payload[offset + 1]]);
        let status = Self {
            frame: u32::from_be_bytes(payload[..4].try_into().unwrap()),
            highest_received: word(4),
            next_contiguous: word(6),
            missing_before_highest: word(8),
            data: word(10),
            parity: word(12),
            received_data: word(14),
            received_parity: word(16),
            percentage: payload[18],
            block: payload[19],
            blocks: payload[20],
        };
        ensure!(
            (1..=4).contains(&status.blocks) && status.block < status.blocks,
            "invalid FEC block index/count"
        );
        ensure!((1..=1023).contains(&status.data), "invalid FEC data count");
        let total = u32::from(status.data) + u32::from(status.parity);
        ensure!(
            u32::from(status.parity)
                == (u32::from(status.data) * u32::from(status.percentage)).div_ceil(100)
                && (status.parity == 0 || total <= 255),
            "invalid FEC parity count"
        );
        ensure!(
            status.received_data <= status.data && status.received_parity <= status.parity,
            "invalid FEC received counts"
        );
        let received = u32::from(status.received_data) + u32::from(status.received_parity);
        ensure!(
            u32::from(status.missing_before_highest) <= total - received,
            "invalid FEC missing count"
        );
        ensure!(
            u32::from(
                status
                    .highest_received
                    .wrapping_add(1)
                    .wrapping_sub(status.next_contiguous)
            ) <= total,
            "invalid FEC sequence range"
        );
        Ok(status)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload() -> [u8; 21] {
        // Sequence numbers cross 65535; one of ten data packets was recovered.
        [
            0x12, 0x34, 0x56, 0x78, 0, 4, 0xff, 0xfe, 0, 1, 0, 10, 0, 2, 0, 9, 0, 1, 20, 1, 3,
        ]
    }
    #[test]
    fn parses_packed_big_endian_fields_and_wrapped_sequences() {
        assert_eq!(
            Status::parse(&payload()).unwrap(),
            Status {
                frame: 0x12345678,
                highest_received: 4,
                next_contiguous: 65534,
                missing_before_highest: 1,
                data: 10,
                parity: 2,
                received_data: 9,
                received_parity: 1,
                percentage: 20,
                block: 1,
                blocks: 3,
            }
        );
    }
    #[test]
    fn rejects_bad_lengths_counts_blocks_and_sequence_ranges() {
        for length in 0..=64 {
            if length != 21 {
                assert!(Status::parse(&vec![0; length]).is_err());
            }
        }
        for (offset, value) in [
            (10, 0),
            (10, 1024),
            (12, 3),
            (14, 11),
            (16, 3),
            (8, 3),
            (6, 100),
        ] {
            let mut bytes = payload();
            bytes[offset..offset + 2].copy_from_slice(&u16::to_be_bytes(value));
            assert!(
                Status::parse(&bytes).is_err(),
                "offset={offset} value={value}"
            );
        }
        for (offset, value) in [(18, 0), (19, 3), (20, 0), (20, 5)] {
            let mut bytes = payload();
            bytes[offset] = value;
            assert!(Status::parse(&bytes).is_err());
        }
    }
    #[test]
    fn permits_no_fec_and_pyrowave_percentages_above_one_hundred() {
        let mut bytes = payload();
        bytes[12..14].copy_from_slice(&26u16.to_be_bytes());
        bytes[18] = 255;
        assert!(Status::parse(&bytes).is_ok());
        bytes[12..14].fill(0);
        bytes[16..19].fill(0);
        assert!(Status::parse(&bytes).is_ok());
    }
}
