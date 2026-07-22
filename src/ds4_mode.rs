use crate::aim_report::AimState;
use crate::rumble::{self, RumbleLed};
use crate::{REAL_PID, REAL_VID};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use vigem_rust::controller::ds4::{Ds4ReportEx, Ds4SpecialButton};
use vigem_rust::{Client, Ds4Button, Ds4Dpad};

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    println!("Connecting to real Aim Controller ({REAL_VID:04x}:{REAL_PID:04x})...");
    let hid = hidapi::HidApi::new()?;
    let reader = hid.open(REAL_VID, REAL_PID)?;
    let writer = hid.open(REAL_VID, REAL_PID)?;
    println!("Connected to real device.");

    // Wake the controller into "full" report mode (report ID 0x11,
    // includes real gyro/accel) if connected over Bluetooth -- BT
    // defaults to a limited 10-byte report with no motion data at all
    // until these feature reports are queried. Harmless no-op over USB,
    // which already streams full data by default on report ID 0x01.
    for (report_id, size) in [(0x02usize, 37usize), (0xa3, 49), (0x12, 16)] {
        let mut buf = vec![0u8; size];
        buf[0] = report_id as u8;
        let _ = reader.get_feature_report(&mut buf);
        thread::sleep(std::time::Duration::from_millis(50));
    }

    println!("Connecting to ViGEmBus...");
    let client = Client::connect()?;
    let ds4 = client.new_ds4_target().plugin()?;
    ds4.wait_for_ready()?;
    println!("Virtual DS4 ready. Relaying input -- Ctrl+C to stop.");

    let notifications = ds4.register_notification()?;
    let is_bt = Arc::new(AtomicBool::new(false));
    let is_bt_for_thread = is_bt.clone();
    thread::spawn(move || {
        // The real hardware's firmware has an autonomous idle indicator
        // that overrides a custom LED color if it isn't periodically
        // re-asserted -- same root cause Lightgun mode already had to
        // handle. Switched from blocking recv() to a timeout-based loop
        // so we can re-send the last-known state even when no fresh
        // notification has arrived recently.
        let mut last_rl = RumbleLed { motor_weak: 0, motor_strong: 0, red: 0, green: 0, blue: 0 };
        loop {
            match notifications.recv_timeout(std::time::Duration::from_millis(500)) {
                Ok(Ok(notification)) => {
                    last_rl = RumbleLed {
                        motor_weak: notification.small_motor,
                        motor_strong: notification.large_motor,
                        red: notification.lightbar.red,
                        green: notification.lightbar.green,
                        blue: notification.lightbar.blue,
                    };
                    rumble::send(&writer, is_bt_for_thread.load(Ordering::Relaxed), &last_rl);
                }
                Ok(Err(_)) => break,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    rumble::send(&writer, is_bt_for_thread.load(Ordering::Relaxed), &last_rl);
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
    });

    // Gyro/accel via Ds4ReportEx::update_ex() -- works via a local patch
    // to the vigem-rust crate (see Cargo.toml [patch.crates-io]) plus
    // avoiding its buggy set_dpad() helper (see the comment below).
    //
    // DS4's gyro/accel data is only meaningful to games if the report's
    // timestamp field increments at roughly the real rate DS4 hardware
    // updates at (documented elsewhere as ~188 units per 1.25ms of real
    // time). We're not on a precisely-timed loop, so this is an
    // approximation -- confirmed working, but the exact feel could
    // potentially be refined further.
    let mut timestamp: u16 = 0;

    let mut raw_buf = [0u8; 78];
    let mut last_state: Option<AimState> = None;
    loop {
        let n = reader.read_timeout(&mut raw_buf, 15)?;
        if n > 0 {
            if let Some(state) = AimState::parse(&raw_buf[..n]) {
                is_bt.store(raw_buf[0] == 0x11 || n < 24, Ordering::Relaxed);
                last_state = Some(state);
            }
        }
        // Always send an update every loop iteration (short timeout
        // above, so this runs frequently) using the last-known state --
        // previously, if the hardware went quiet, we'd skip calling
        // update_ex() entirely until new data arrived, leaving gaps
        // that may explain the virtual device looking intermittently
        // "stale" while idle.
        let Some(state) = &last_state else {
            continue;
        };

        let mut report = Ds4ReportEx::default();

        report.thumb_lx = state.lstick_x;
        report.thumb_ly = state.lstick_y;
        report.thumb_rx = state.rstick_x;
        report.thumb_ry = state.rstick_y;

        report.trigger_l = state.l2_analog;
        report.trigger_r = state.r2_analog;

        let mut buttons = Ds4Button::empty();
        if state.square { buttons |= Ds4Button::SQUARE; }
        if state.cross { buttons |= Ds4Button::CROSS; }
        if state.circle { buttons |= Ds4Button::CIRCLE; }
        if state.triangle { buttons |= Ds4Button::TRIANGLE; }
        if state.l1 { buttons |= Ds4Button::SHOULDER_LEFT; }
        if state.r1 { buttons |= Ds4Button::SHOULDER_RIGHT; }
        if state.l2_click { buttons |= Ds4Button::TRIGGER_LEFT; }
        if state.r2_click { buttons |= Ds4Button::TRIGGER_RIGHT; }
        if state.share { buttons |= Ds4Button::SHARE; }
        if state.options { buttons |= Ds4Button::OPTIONS; }
        if state.l3 { buttons |= Ds4Button::THUMB_LEFT; }
        if state.r3 { buttons |= Ds4Button::THUMB_RIGHT; }
        report.buttons = buttons.bits();

        let mut special = Ds4SpecialButton::empty();
        if state.ps_guide { special |= Ds4SpecialButton::PS; }
        if state.pad_button { special |= Ds4SpecialButton::TOUCHPAD; }
        report.special = special.bits();

        let dpad = match state.dpad {
            0 => Ds4Dpad::North,
            1 => Ds4Dpad::NorthEast,
            2 => Ds4Dpad::East,
            3 => Ds4Dpad::SouthEast,
            4 => Ds4Dpad::South,
            5 => Ds4Dpad::SouthWest,
            6 => Ds4Dpad::West,
            7 => Ds4Dpad::NorthWest,
            _ => Ds4Dpad::Neutral,
        };
        // NOTE: deliberately not calling report.set_dpad() here -- that
        // helper internally casts &mut Ds4ReportExData (packed) to
        // &mut Ds4Report (NOT packed, assumes normal 2-byte alignment
        // for its u16 buttons field), which is unsound when the object
        // actually lives inside a packed container and triggers a
        // misaligned-pointer panic. Direct field writes on the correct
        // (packed) type are fine, so just replicate the same bit-masking
        // the helper does, inline, on the field we're already safely
        // writing to elsewhere.
        const DPAD_MASK: u16 = 0x000F;
        report.buttons = (report.buttons & !DPAD_MASK) | (dpad as u16);

        // Axis mapping is a starting guess -- our parser's yaw/pitch/roll
        // labels were reverse-engineered against the real hardware's own
        // convention, not against what a real DS4 expects on the same
        // axes. Confirmed working (gyro visibly responds to motion), but
        // the exact axis-to-axis correspondence hasn't been precisely
        // validated the way Lightgun mode's gyro was on Linux.
        report.gyro_x = state.gyro_yaw;
        report.gyro_y = state.gyro_pitch;
        report.gyro_z = state.gyro_roll;
        report.accel_x = state.accel_x;
        report.accel_y = state.accel_y;
        report.accel_z = state.accel_z;

        report.timestamp = timestamp;
        timestamp = timestamp.wrapping_add(188);

        ds4.update_ex(&report)?;
    }
}
