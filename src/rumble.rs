//! Real hardware's USB/BT output report (rumble + lightbar). Byte layout
//! reverse-engineered against actual hardware on the Linux side -- same
//! physical device, so this is identical regardless of host OS or which
//! virtual target we're driving. Shared between DS4 and Lightgun modes.

pub struct RumbleLed {
    pub motor_weak: u8,
    pub motor_strong: u8,
    pub red: u8,
    pub green: u8,
    pub blue: u8,
}

const RUMBLE_GAIN: f32 = 1.8;
const RUMBLE_FLOOR: u8 = 90;

fn boost_motor(v: u8) -> u8 {
    if v == 0 {
        return 0;
    }
    let boosted = (v as f32 * RUMBLE_GAIN).round().clamp(RUMBLE_FLOOR as f32, 255.0);
    boosted as u8
}

impl RumbleLed {
    pub fn to_report(&self) -> [u8; 32] {
        let mut buf = [0u8; 32];
        buf[0] = 0x05;
        buf[1] = 0x07;
        buf[4] = boost_motor(self.motor_weak);
        buf[5] = boost_motor(self.motor_strong);
        buf[6] = self.red;
        buf[7] = self.green;
        buf[8] = self.blue;
        buf
    }

    /// Bluetooth uses a completely different output report: ID 0x11,
    /// 78 bytes, CRC32-terminated. The firmware silently ignores this
    /// report if the checksum is missing/wrong.
    pub fn to_bt_report(&self) -> [u8; 78] {
        let mut buf = [0u8; 78];
        buf[0] = 0x11;
        buf[1] = 0xC0; // hw_control: HID + CRC32 enabled
        buf[3] = 0x03; // valid_flag0: motor + LED both valid
        buf[6] = boost_motor(self.motor_weak);
        buf[7] = boost_motor(self.motor_strong);
        buf[8] = self.red;
        buf[9] = self.green;
        buf[10] = self.blue;

        let crc = crate::crc32::sony_crc32(0xA2, &buf[0..74]);
        buf[74..78].copy_from_slice(&crc.to_le_bytes());
        buf
    }
}

/// Sends a report to the real device, using the correct format for the
/// currently-detected transport. USB needs two consecutive writes due to
/// the firmware's double-buffered commit behavior; BT needs just one.
pub fn send(writer: &hidapi::HidDevice, is_bt: bool, rl: &RumbleLed) {
    if is_bt {
        let _ = writer.write(&rl.to_bt_report());
    } else {
        let buf = rl.to_report();
        let _ = writer.write(&buf);
        std::thread::sleep(std::time::Duration::from_millis(15));
        let _ = writer.write(&buf);
    }
}
