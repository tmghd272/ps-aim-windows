//! Parses the PS VR Aim Controller's raw HID input report (Report ID 1).
//!
//! Field map reverse-engineered by hand against a real unit (VID 054c,
//! PID 0bb2). Two transports are supported:
//!   - USB: 64 bytes, full IMU data (gyro/accel) in a vendor block
//!     starting at byte 10.
//!   - Bluetooth: 10 bytes. Confirmed via hid-decode against the BT
//!     hidraw node that bytes 1-9 use the *exact same* layout as USB
//!     (sticks, hat, buttons, analog triggers) -- the report simply ends
//!     there with no IMU block at all. Real hardware limitation, not a
//!     parsing gap: gyro/accel genuinely aren't transmitted over BT.

#[derive(Debug, Clone, Copy, Default)]
pub struct AimState {
    // Sticks, 0-255, center ~126-134 depending on axis.
    pub lstick_x: u8,
    pub lstick_y: u8,
    pub rstick_x: u8,
    pub rstick_y: u8,

    // D-pad hat: 0=Up 2=Right 4=Down 6=Left 8=Neutral (even values only observed).
    pub dpad: u8,

    // Face buttons
    pub square: bool,
    pub cross: bool,
    pub circle: bool,
    pub triangle: bool,

    // Shoulders / sticks-click / menu
    pub l1: bool,
    pub r1: bool,
    pub l2_click: bool, // digital click, distinct from analog pull
    pub r2_click: bool, // digital click / trigger detent
    pub share: bool,
    pub options: bool,
    pub l3: bool, // left stick click
    pub r3: bool, // right stick click

    pub ps_guide: bool,
    pub pad_button: bool,

    // Analog triggers, 0-255
    pub l2_analog: u8,
    pub r2_analog: u8, // main gun trigger

    // Gyro, raw firmware units (not yet scaled to deg/s). Zeroed when
    // connected over Bluetooth -- not transmitted on that transport.
    pub gyro_yaw: i16,
    pub gyro_pitch: i16,
    pub gyro_roll: i16,

    // Accelerometer, raw firmware units. Zeroed over Bluetooth, same as gyro.
    pub accel_x: i16,
    pub accel_y: i16,
    pub accel_z: i16,

    /// True if this report came with real IMU data (USB). False for BT
    /// reports, where gyro/accel fields above are just zeroed placeholders.
    #[allow(dead_code)]
    pub has_motion: bool,
}

impl AimState {
    /// Parse a raw HID input report (report ID byte included at [0]).
    /// Handles all three known report shapes:
    ///   - USB: report ID 0x01, 64 bytes, full IMU in a vendor block.
    ///   - BT "simple": report ID 0x01, 10 bytes, no IMU at all.
    ///   - BT "full": report ID 0x11, 78 bytes, real gyro/accel -- but
    ///     only streamed after querying certain feature reports first
    ///     (see main.rs's BT wake-up sequence). Confirmed against a live
    ///     capture: matches the real DS4's known BT report struct exactly.
    /// Returns None if the report is too short to even contain
    /// buttons/sticks, or isn't a report ID we recognize.
    pub fn parse(report: &[u8]) -> Option<AimState> {
        if report.len() >= 78 && report[0] == 0x11 {
            return Self::parse_bt_full(report);
        }
        if report.len() >= 10 && report[0] == 0x01 {
            return Self::parse_simple(report);
        }
        None
    }

    fn parse_simple(report: &[u8]) -> Option<AimState> {
        let btn5 = report[5];
        let btn6 = report[6];
        let btn7 = report[7] & 0x03; // low 2 bits only; high bits are a free-running counter (USB) / padding (BT)

        let has_motion = report.len() >= 24;
        let (gyro_yaw, gyro_pitch, gyro_roll, accel_x, accel_y, accel_z) = if has_motion {
            // IMU block starts at byte 10, 7x little-endian i16: [ts, gx, gy, gz, ax, ay, az]
            let imu = |idx: usize| -> i16 {
                let base = 10 + 2 + idx * 2; // +2 skips the per-sample timestamp field
                i16::from_le_bytes([report[base], report[base + 1]])
            };
            (imu(0), imu(1), imu(2), imu(3), imu(4), imu(5))
        } else {
            (0, 0, 0, 0, 0, 0)
        };

        Some(AimState {
            lstick_x: report[1],
            lstick_y: report[2],
            rstick_x: report[3],
            rstick_y: report[4],
            dpad: btn5 & 0x0F,
            square: btn5 & 0x10 != 0,
            cross: btn5 & 0x20 != 0,
            circle: btn5 & 0x40 != 0,
            triangle: btn5 & 0x80 != 0,
            l1: btn6 & 0x01 != 0,
            r1: btn6 & 0x02 != 0,
            l2_click: btn6 & 0x04 != 0,
            r2_click: btn6 & 0x08 != 0,
            share: btn6 & 0x10 != 0,
            options: btn6 & 0x20 != 0,
            l3: btn6 & 0x40 != 0,
            r3: btn6 & 0x80 != 0,
            ps_guide: btn7 & 0x01 != 0,
            pad_button: btn7 & 0x02 != 0,
            l2_analog: report[8],
            r2_analog: report[9],
            gyro_yaw,
            gyro_pitch,
            gyro_roll,
            accel_x,
            accel_y,
            accel_z,
            has_motion,
        })
    }

    /// BT full-mode report (ID 0x11, 78 bytes). Layout confirmed against
    /// live capture during sustained rotation: byte0=report_id,
    /// bytes1-2=header, then byte3..=x,y,rx,ry,buttons[3],z,rz -- same as
    /// USB. The IMU block contains two oversampled 14-byte sub-samples
    /// back-to-back (timestamp + 6x i16 gyro/accel each), matching the
    /// USB report's IMU structure. The *second* sub-sample (starting at
    /// absolute byte 26) is populated far more consistently than the
    /// first across captured packets, and its values form a smooth,
    /// continuous ramp during sustained rotation -- confirmed the
    /// reliable data source, unlike the first sub-sample which is often
    /// all-zero even during active motion.
    fn parse_bt_full(report: &[u8]) -> Option<AimState> {
        const BASE: usize = 3;
        let btn0 = report[BASE + 4];
        let btn1 = report[BASE + 5];
        let btn2 = report[BASE + 6] & 0x03;

        let le16 = |off: usize| -> i16 {
            i16::from_le_bytes([report[off], report[off + 1]])
        };

        // Second IMU sub-sample: timestamp at 26-27, then 6x i16 starting at 28.
        const SAMPLE2_SENSORS: usize = 28;
        let gyro_yaw = le16(SAMPLE2_SENSORS);
        let gyro_pitch = le16(SAMPLE2_SENSORS + 2);
        let gyro_roll = le16(SAMPLE2_SENSORS + 4);
        let accel_x = le16(SAMPLE2_SENSORS + 6);
        let accel_y = le16(SAMPLE2_SENSORS + 8);
        let accel_z = le16(SAMPLE2_SENSORS + 10);

        Some(AimState {
            lstick_x: report[BASE],
            lstick_y: report[BASE + 1],
            rstick_x: report[BASE + 2],
            rstick_y: report[BASE + 3],
            dpad: btn0 & 0x0F,
            square: btn0 & 0x10 != 0,
            cross: btn0 & 0x20 != 0,
            circle: btn0 & 0x40 != 0,
            triangle: btn0 & 0x80 != 0,
            l1: btn1 & 0x01 != 0,
            r1: btn1 & 0x02 != 0,
            l2_click: btn1 & 0x04 != 0,
            r2_click: btn1 & 0x08 != 0,
            share: btn1 & 0x10 != 0,
            options: btn1 & 0x20 != 0,
            l3: btn1 & 0x40 != 0,
            r3: btn1 & 0x80 != 0,
            ps_guide: btn2 & 0x01 != 0,
            pad_button: btn2 & 0x02 != 0,
            l2_analog: report[BASE + 7],
            r2_analog: report[BASE + 8],
            gyro_yaw,
            gyro_pitch,
            gyro_roll,
            accel_x,
            accel_y,
            accel_z,
            has_motion: true,
        })
    }
}
