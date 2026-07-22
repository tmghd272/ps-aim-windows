//! CRC32 (IEEE 802.3 / zlib polynomial) matching Sony's convention for DS4
//! Bluetooth reports: a seed byte is hashed in first, then the report
//! data, with a final bitwise invert. BT output reports (rumble/LED) are
//! silently ignored by the firmware if this checksum is missing or wrong.

const POLY: u32 = 0xEDB88320;

fn update(crc: &mut u32, byte: u8) {
    *crc ^= byte as u32;
    for _ in 0..8 {
        if *crc & 1 != 0 {
            *crc = (*crc >> 1) ^ POLY;
        } else {
            *crc >>= 1;
        }
    }
}

/// Compute Sony's DS4-style CRC32: seed byte followed by `data`, standard
/// IEEE crc32 with init 0xFFFFFFFF, final result bitwise-inverted.
pub fn sony_crc32(seed: u8, data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFFFFFF;
    update(&mut crc, seed);
    for &b in data {
        update(&mut crc, b);
    }
    !crc
}
