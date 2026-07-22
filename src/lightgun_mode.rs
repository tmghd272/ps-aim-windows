use crate::aim_report::AimState;
use crate::config::{Config, SavedRecoilMode};
use crate::pseye_client;
use crate::rumble;
use crate::{REAL_PID, REAL_VID};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use vigem_rust::{Client, X360Button, X360Report};

fn gyro_accel(v: f32, accel_threshold: f32, accel_gain: f32) -> f32 {
    let extra = if v.abs() > accel_threshold {
        (1.0 + (v.abs() - accel_threshold) * accel_gain).min(3.0)
    } else {
        1.0
    };
    v * extra
}

// NOTE: XInput's Y-axis convention is up=positive, opposite of the
// raster-style down=positive convention the real hardware's raw stick
// values (and DS4's own protocol) use -- confirmed via testing, both
// left stick and gyro-driven right stick needed this flip specifically
// for Windows/XInput (Linux's uinput ABS_Y didn't have this mismatch).
fn stick_u8_to_i16_x(raw: u8) -> i16 {
    (((raw as i32) - 128) * 256).clamp(-32768, 32767) as i16
}
fn stick_u8_to_i16_y(raw: u8) -> i16 {
    (((128 - raw as i32)) * 256).clamp(-32768, 32767) as i16
}

#[derive(Clone, Copy, PartialEq)]
enum RecoilMode {
    SingleKick,
    RapidFire,
    Off,
}

impl RecoilMode {
    fn next(self) -> Self {
        match self {
            RecoilMode::SingleKick => RecoilMode::RapidFire,
            RecoilMode::RapidFire => RecoilMode::Off,
            RecoilMode::Off => RecoilMode::SingleKick,
        }
    }
    fn label(self) -> &'static str {
        match self {
            RecoilMode::SingleKick => "Single Kick",
            RecoilMode::RapidFire => "Rapid Fire",
            RecoilMode::Off => "Vibration Off",
        }
    }
    fn to_saved(self) -> SavedRecoilMode {
        match self {
            RecoilMode::SingleKick => SavedRecoilMode::SingleKick,
            RecoilMode::RapidFire => SavedRecoilMode::RapidFire,
            RecoilMode::Off => SavedRecoilMode::Off,
        }
    }
}

// Camera frame resolution from the C++ tracker (fixed at 320x240).
const PSEYE_FRAME_WIDTH: f32 = 320.0;
const PSEYE_FRAME_HEIGHT: f32 = 240.0;

// Real, measured camera-space edges from 4-point calibration (point at
// each screen corner, pull trigger) plus a center validation point --
// same underlying idea as the RawInput mode's PS Eye calibration, but
// there's no cursor to snap to a visual anchor here: a virtual stick
// has no concept of absolute position, only deflection. Calibration
// instead just needs to know the real camera-space range corresponding
// to "as far as you'd ever plausibly aim" -- the player looks at the
// actual physical screen corners themselves (there's nothing to
// display) and points the lightgun there when prompted.
struct CalibratedRange {
    left: f32,
    right: f32,
    top: f32,
    bottom: f32,
    correction_x: f32,
    correction_y: f32,
}

impl CalibratedRange {
    fn default_full_frame() -> Self {
        CalibratedRange { left: 0.0, right: PSEYE_FRAME_WIDTH, top: 0.0, bottom: PSEYE_FRAME_HEIGHT, correction_x: 0.0, correction_y: 0.0 }
    }
}

/// Maps a raw camera position to stick deflection (-32768..32767) using
/// the calibrated range -- offset from calibrated center, not absolute
/// position (a stick has no such concept). Includes the same overscan
/// margin as RawInput mode, so full deflection doesn't require
/// pixel-perfect aim at the exact calibrated extreme.
fn pseye_to_deflection(cam_x: f32, cam_y: f32, cal: &CalibratedRange) -> (f32, f32) {
    // Guards against a near-zero-width captured range (possible if the
    // calibration corners ended up close together) producing a
    // division that yields NaN -- Rust casts NaN to i16 as exactly 0,
    // which would look identical to a completely dead, unresponsive
    // stick regardless of real movement, rather than an out-of-range
    // clamp like a merely-narrow-but-valid range would.
    let x_span = (cal.right - cal.left).abs().max(1.0);
    let y_span = (cal.bottom - cal.top).abs().max(1.0);
    let raw_frac_x = (cam_x - cal.left) / x_span; // 0..1
    let raw_frac_y = (cam_y - cal.top) / y_span;
    const OVERSCAN: f32 = 1.08;
    let frac_x = ((raw_frac_x - 0.5) * OVERSCAN).clamp(-0.5, 0.5); // -0.5..0.5, offset from center
    let frac_y = ((raw_frac_y - 0.5) * OVERSCAN).clamp(-0.5, 0.5);
    // Camera faces the player, so its view is mirrored left/right
    // relative to their own perspective (like a mirror). Y is also
    // flipped here: camera Y increases downward (standard image
    // coordinates), but XInput's convention is up=positive (same
    // mismatch this file's gyro path already accounts for) -- without
    // this flip, aiming up produced negative deflection instead of
    // positive, inverting up/down.
    let defl_x = (-frac_x * 2.0 * 32767.0 + cal.correction_x).clamp(-32768.0, 32767.0);
    let defl_y = (-frac_y * 2.0 * 32767.0 + cal.correction_y).clamp(-32768.0, 32767.0);
    (defl_x, defl_y)
}

#[derive(Clone, Copy, PartialEq)]
enum CalibStep {
    Idle,
    WaitingTopLeft,
    WaitingTopRight,
    WaitingBottomLeft,
    WaitingBottomRight,
    WaitingCenter,
}

pub fn run(pseye_enabled: bool) -> Result<(), Box<dyn std::error::Error>> {
    println!("Connecting to real Aim Controller ({REAL_VID:04x}:{REAL_PID:04x})...");
    let hid = hidapi::HidApi::new()?;
    let reader = hid.open(REAL_VID, REAL_PID)?;
    let writer = hid.open(REAL_VID, REAL_PID)?;
    println!("Connected to real device.");

    for (report_id, size) in [(0x02usize, 37usize), (0xa3, 49), (0x12, 16)] {
        let mut buf = vec![0u8; size];
        buf[0] = report_id as u8;
        let _ = reader.get_feature_report(&mut buf);
        thread::sleep(Duration::from_millis(50));
    }

    println!("Connecting to ViGEmBus...");
    let client = Client::connect()?;
    let x360 = client.new_x360_target().plugin()?;
    x360.wait_for_ready()?;
    println!("Virtual Xbox 360 pad ready. Relaying input -- Ctrl+C to stop.");

    // NOTE: deliberately no rumble-notification relay here, unlike DS4
    // mode. Matches the Linux Lightgun mode exactly -- games' own
    // gamepad vibration is ignored entirely; only the custom R2-recoil
    // pulses (below) drive the real motor. An Xbox 360 pad has no
    // lightbar either way, so the LED stays fixed/driver-controlled
    // regardless of what any host software sends.

    let cfg = Config::load();
    let sensitivity = cfg.lightgun_sensitivity;
    let accel_threshold = cfg.lightgun_accel_threshold;
    let accel_gain = cfg.lightgun_accel_gain;
    let recoil_intensity = cfg.lightgun_recoil_intensity;
    let recoil_duration_ms = cfg.lightgun_recoil_duration_ms;
    let rapidfire_interval_ms = cfg.lightgun_rapidfire_interval_ms;
    // When PS Eye is active, match the color the camera tracker
    // expects (its default target) rather than the usual configured
    // color -- both programs write to the LED independently with no
    // coordination between them, so agreeing on the color avoids a
    // visible fight/flicker between the two.
    let led = if pseye_enabled { (80, 0, 150) } else { cfg.lightgun_led };
    let translation_gain = cfg.lightgun_translation_gain;
    let translation_decay = cfg.lightgun_translation_decay;

    let mut recoil_mode = match cfg.recoil_mode {
        SavedRecoilMode::SingleKick => RecoilMode::SingleKick,
        SavedRecoilMode::RapidFire => RecoilMode::RapidFire,
        SavedRecoilMode::Off => RecoilMode::Off,
    };
    println!("Recoil mode: {} (Pad button cycles modes, Right Stick click or PS button resets aim)", recoil_mode.label());

    let is_bt = Arc::new(AtomicBool::new(false));
    let mut aim_x: f32 = 0.0;
    let mut aim_y: f32 = 0.0;
    // Damped "nudge" from physically moving the controller (not just
    // tilting it) -- spring-back toward zero each frame rather than
    // true position integration, which would drift badly.
    let mut translate_x: f32 = 0.0;
    let mut translate_y: f32 = 0.0;
    // Slowly-adapting baseline per axis -- auto-calibrates for gravity's
    // constant pull (bigger on whichever axis is more vertically
    // aligned when holding the controller normally, which is why the
    // up/down axis jittered more than left/right with a flat deadzone
    // alone). React to deviation from this baseline, not the raw value.
    let mut accel_x_baseline: f32 = 0.0;
    let mut accel_y_baseline: f32 = 0.0;
    let mut baseline_initialized = false;
    // Corrects small constant gyro bias that otherwise integrates into
    // steadily-growing drift -- but ONLY adapts while the controller is
    // genuinely near-motionless (stillness-gated), same proven pattern
    // already used for the VR gyro bias correction in this project's
    // soft_knuckles code. An earlier attempt that adapted toward
    // whatever the current reading was at all times ended up cancelling
    // out genuine sustained rotation too (slow panning is common and
    // legitimate for gyro aiming), which felt like the aim being pulled
    // back toward center -- gating on stillness avoids that entirely,
    // since sustained aiming motion never looks like "the new still
    // baseline" to it.
    let mut gyro_pitch_baseline: f32 = 0.0;
    let mut gyro_yaw_baseline: f32 = 0.0;
    let mut gyro_roll_baseline: f32 = 0.0;
    let mut gyro_baseline_initialized = false;
    const GYRO_STILLNESS_THRESHOLD: f32 = 30.0;
    const GYRO_BASELINE_ADAPT_RATE: f32 = 0.02;
    let mut last_sensor_time = Instant::now();
    let mut last_r3 = false;
    let mut last_ps_guide = false;
    let mut last_r2_click = false;
    #[allow(unused_assignments)]
    let mut r2_held = false;
    let mut recoil_until: Option<Instant> = None;
    let mut next_rapid_fire = Instant::now();
    let mut pad_button_since: Option<Instant> = None;
    let mut pad_button_actioned = false;
    let mut last_mode_switch = Instant::now() - Duration::from_secs(10);
    let mut last_led_resend = Instant::now() - Duration::from_secs(10);
    // Hold Pad button 5s to pause all input/gyro output entirely --
    // lets you set the controller down without it interfering with
    // whatever's currently using the virtual pad. LED dims to signal
    // the paused state and save battery.
    let mut input_disabled = false;
    const DISABLE_HOLD_MS: u64 = 5000;

    // PS Eye camera-tracked aiming, on top of gyro -- gyro keeps
    // running unconditionally below regardless of this setting, so it
    // can take over instantly and smoothly the moment the sphere isn't
    // visible (or if --pseye wasn't passed at all).
    let pseye = if pseye_enabled {
        println!("PS Eye tracking enabled -- connecting to camera tracker on 127.0.0.1:9876...");
        Some(pseye_client::start(9876))
    } else {
        None
    };
    let mut calib_range = CalibratedRange::default_full_frame();
    let mut calib_step = CalibStep::Idle;
    let mut calib_top_left: Option<(f32, f32)> = None;
    let mut calib_top_right: Option<(f32, f32)> = None;
    let mut calib_bottom_left: Option<(f32, f32)> = None;
    let mut calib_bottom_right: Option<(f32, f32)> = None;
    let mut last_calib_r2 = false;
    // Prevents an immediate phantom shot right as calibration finishes
    // -- the same physical trigger pull that confirms the final point
    // is still held down the instant calib_step flips back to Idle, so
    // normal R2 firing needs to wait for an actual release first.
    let mut suppress_r2_until_release = false;
    let mut r3_held_since: Option<Instant> = None;
    const CALIB_HOLD_MS: u64 = 800;
    // Accumulates camera readings for however long the trigger is held
    // during calibration -- captured on release with whatever's been
    // collected (however brief), rather than requiring a fixed hold
    // duration. Averaging cancels out normal frame-to-frame jitter when
    // held a bit longer, but a quick tap still works fine too.
    let mut calib_sample_sum: (f32, f32) = (0.0, 0.0);
    let mut calib_sample_count: u32 = 0;

    let mut raw_buf = [0u8; 78];
    loop {
        let n = reader.read_timeout(&mut raw_buf, 100)?;
        if n == 0 {
            continue;
        }
        let Some(state) = AimState::parse(&raw_buf[..n]) else {
            continue;
        };
        is_bt.store(raw_buf[0] == 0x11 || n < 24, Ordering::Relaxed);

        if state.ps_guide && !last_ps_guide {
            aim_x = 0.0;
            aim_y = 0.0;
            translate_x = 0.0;
            translate_y = 0.0;
        }
        last_ps_guide = state.ps_guide;

        if state.r3 && !last_r3 {
            r3_held_since = Some(Instant::now());
        }
        if state.r3 {
            if let Some(since) = r3_held_since {
                if calib_step == CalibStep::Idle
                    && pseye.is_some()
                    && since.elapsed() >= Duration::from_millis(CALIB_HOLD_MS)
                {
                    calib_step = CalibStep::WaitingTopLeft;
                    println!("PS Eye calibration: point at TOP-LEFT corner and pull trigger");
                    r3_held_since = None; // consumed -- don't also fire the short-tap reset on release
                }
            }
        }
        if !state.r3 && last_r3 {
            if r3_held_since.is_some() {
                // Released before the hold threshold, and not consumed
                // by entering calibration -- short tap, existing
                // aim-reset behavior, unrelated to PS Eye.
                aim_x = 0.0;
                aim_y = 0.0;
                translate_x = 0.0;
                translate_y = 0.0;
            }
            r3_held_since = None;
        }
        last_r3 = state.r3;

        // 5-point PS Eye calibration sequence: hold the trigger briefly
        // to confirm each point in turn (averaging several camera
        // readings over the hold rather than trusting one possibly
        // noisy instant). No cursor to snap to a visual anchor here --
        // the player looks at the actual physical screen corners
        // themselves and points the lightgun there when prompted.
        if calib_step != CalibStep::Idle {
            // Analog threshold instead of the digital r2_click -- more
            // forgiving of a real trigger pull that isn't fully/
            // consistently depressed.
            let calib_r2_pressed = state.r2_analog > 60;
            if let Some(ref pseye_handle) = pseye {
                if calib_r2_pressed {
                    if !last_calib_r2 {
                        calib_sample_sum = (0.0, 0.0);
                        calib_sample_count = 0;
                    }
                    if let Some((cam_x, cam_y)) = pseye_handle.get() {
                        calib_sample_sum.0 += cam_x;
                        calib_sample_sum.1 += cam_y;
                        calib_sample_count += 1;
                    }
                } else if last_calib_r2 {
                    // Just released -- capture immediately with
                    // whatever was collected during the press, however
                    // long or short it was, rather than requiring a
                    // fixed hold duration. A longer hold naturally
                    // averages more samples (steadier against noise),
                    // but a quick tap still works fine too -- no
                    // attempt is ever rejected.
                    if calib_sample_count == 0 {
                        println!("Sphere not visible -- point at the sphere in camera view before pulling trigger");
                    } else {
                        let cam_x = calib_sample_sum.0 / calib_sample_count as f32;
                        let cam_y = calib_sample_sum.1 / calib_sample_count as f32;
                        match calib_step {
                            CalibStep::WaitingTopLeft => {
                                calib_top_left = Some((cam_x, cam_y));
                                calib_step = CalibStep::WaitingTopRight;
                                println!("Point at TOP-RIGHT corner and pull trigger");
                            }
                            CalibStep::WaitingTopRight => {
                                calib_top_right = Some((cam_x, cam_y));
                                calib_step = CalibStep::WaitingBottomLeft;
                                println!("Point at BOTTOM-LEFT corner and pull trigger");
                            }
                            CalibStep::WaitingBottomLeft => {
                                calib_bottom_left = Some((cam_x, cam_y));
                                calib_step = CalibStep::WaitingBottomRight;
                                println!("Point at BOTTOM-RIGHT corner and pull trigger");
                            }
                            CalibStep::WaitingBottomRight => {
                                calib_bottom_right = Some((cam_x, cam_y));
                                if let (Some(tl), Some(tr), Some(bl), Some(br)) =
                                    (calib_top_left, calib_top_right, calib_bottom_left, calib_bottom_right)
                                {
                                    let mut left = (tl.0 + bl.0) / 2.0;
                                    let mut right = (tr.0 + br.0) / 2.0;
                                    let mut top = (tl.1 + tr.1) / 2.0;
                                    let mut bottom = (bl.1 + br.1) / 2.0;
                                    if right < left { std::mem::swap(&mut left, &mut right); }
                                    if bottom < top { std::mem::swap(&mut top, &mut bottom); }
                                    // Enforce a sane minimum span --
                                    // without a required hold duration,
                                    // a quick tap might only capture a
                                    // couple of samples, and a noisy
                                    // capture could otherwise produce a
                                    // pathologically narrow range (we've
                                    // seen spans under 15px in testing),
                                    // which saturates output at the
                                    // extremes for most real aiming
                                    // regardless of actual movement --
                                    // exactly the "stuck at the edge"
                                    // feeling. Expands symmetrically
                                    // around whatever center was
                                    // actually captured rather than
                                    // discarding it.
                                    const MIN_SPAN: f32 = 180.0;
                                    if right - left < MIN_SPAN {
                                        let center = (left + right) / 2.0;
                                        left = center - MIN_SPAN / 2.0;
                                        right = center + MIN_SPAN / 2.0;
                                    }
                                    if bottom - top < MIN_SPAN {
                                        let center = (top + bottom) / 2.0;
                                        top = center - MIN_SPAN / 2.0;
                                        bottom = center + MIN_SPAN / 2.0;
                                    }
                                    calib_range.left = left;
                                    calib_range.right = right;
                                    calib_range.top = top;
                                    calib_range.bottom = bottom;
                                }
                                calib_step = CalibStep::WaitingCenter;
                                println!("Now point at the CENTER of the screen and pull trigger");
                            }
                            CalibStep::WaitingCenter => {
                                // Compares what the just-captured
                                // 4-corner mapping would have predicted
                                // for this camera reading (zero
                                // deflection, i.e. dead center) against
                                // what it actually computes -- the gap
                                // becomes a constant correction applied
                                // to every future reading.
                                let (predicted_x, predicted_y) = pseye_to_deflection(cam_x, cam_y, &calib_range);
                                // Clamped -- this is meant to be a small
                                // fix-up for lens distortion/minor
                                // misalignment, not something that can
                                // override the whole signal.
                                const MAX_CORRECTION: f32 = 8000.0;
                                calib_range.correction_x = (-predicted_x).clamp(-MAX_CORRECTION, MAX_CORRECTION);
                                calib_range.correction_y = (-predicted_y).clamp(-MAX_CORRECTION, MAX_CORRECTION);
                                println!("PS Eye calibration complete!");
                                println!("[CALIB] left={:.1} right={:.1} top={:.1} bottom={:.1} correction=({:.0},{:.0})",
                                    calib_range.left, calib_range.right, calib_range.top, calib_range.bottom,
                                    calib_range.correction_x, calib_range.correction_y);
                                calib_step = CalibStep::Idle;
                                calib_top_left = None;
                                calib_top_right = None;
                                calib_bottom_left = None;
                                calib_bottom_right = None;
                                suppress_r2_until_release = true;
                            }
                            CalibStep::Idle => {}
                        }
                    }
                }
            }
        }
        last_calib_r2 = state.r2_analog > 60;
        if suppress_r2_until_release && !state.r2_click {
            suppress_r2_until_release = false;
        }

        let dt = last_sensor_time.elapsed().as_secs_f32().min(0.1);
        last_sensor_time = Instant::now();
        let pseye_has_position = pseye.as_ref().map(|p| p.get().is_some()).unwrap_or(false);
        let in_calib = calib_step != CalibStep::Idle;
        if !input_disabled && !pseye_has_position && !in_calib {
            if !gyro_baseline_initialized {
                gyro_pitch_baseline = state.gyro_pitch as f32;
                gyro_yaw_baseline = state.gyro_yaw as f32;
                gyro_roll_baseline = state.gyro_roll as f32;
                gyro_baseline_initialized = true;
            }
            // Only adapt while genuinely still on BOTH axes -- if
            // either axis shows real rotation, freeze the baseline
            // entirely rather than letting it partially chase whatever
            // is happening, so sustained aiming motion is never
            // mistaken for bias.
            if state.gyro_pitch.abs() as f32 <= GYRO_STILLNESS_THRESHOLD
                && state.gyro_yaw.abs() as f32 <= GYRO_STILLNESS_THRESHOLD
            {
                gyro_pitch_baseline += (state.gyro_pitch as f32 - gyro_pitch_baseline) * GYRO_BASELINE_ADAPT_RATE;
                gyro_yaw_baseline += (state.gyro_yaw as f32 - gyro_yaw_baseline) * GYRO_BASELINE_ADAPT_RATE;
                gyro_roll_baseline += (state.gyro_roll as f32 - gyro_roll_baseline) * GYRO_BASELINE_ADAPT_RATE;
            }
            let gyro_pitch_dev = state.gyro_pitch as f32 - gyro_pitch_baseline;
            let gyro_yaw_dev = state.gyro_yaw as f32 - gyro_yaw_baseline;

            // X_SIGN kept at -1.0 (unaffected by the Y-axis convention bug);
            // Y accumulation direction fixed to match XInput's up=positive.
            // Roll combined with pitch for horizontal aim (same as rawinput).
            aim_x += gyro_accel(gyro_pitch_dev, accel_threshold, accel_gain) * -1.0 * sensitivity * dt;
            aim_y += gyro_accel(gyro_yaw_dev, accel_threshold, accel_gain) * 1.0 * sensitivity * dt;
            aim_x = aim_x.clamp(-32768.0, 32767.0);
            aim_y = aim_y.clamp(-32768.0, 32767.0);
        }

        // Translation nudge: decay toward zero each frame (spring-back),
        // then add a contribution from the current accelerometer reading.
        // This gives a responsive "shove" feel from physically moving the
        // controller without drifting like true position integration
        // would. Axis mapping (accel_x -> horizontal, accel_y ->
        // vertical) is a starting guess, same as every other new axis
        // mapping in this project -- may need a sign flip or swap once
        // tested against the real hardware.
        // Deadzone filters out any remaining noise once gravity's
        // baseline is already subtracted out below.
        const ACCEL_DEADZONE: f32 = 300.0;

        if !baseline_initialized {
            accel_x_baseline = state.accel_x as f32;
            accel_y_baseline = state.accel_y as f32;
            baseline_initialized = true;
        }
        // Slow low-pass filter (~a few seconds to fully adapt) so it
        // tracks gravity/orientation drift without chasing genuine
        // quick movements as if they were the new "still" baseline.
        const BASELINE_RATE: f32 = 0.005;
        accel_x_baseline += (state.accel_x as f32 - accel_x_baseline) * BASELINE_RATE;
        accel_y_baseline += (state.accel_y as f32 - accel_y_baseline) * BASELINE_RATE;

        let accel_x_dev = state.accel_x as f32 - accel_x_baseline;
        let accel_y_dev = state.accel_y as f32 - accel_y_baseline;
        let accel_x_filtered = if accel_x_dev.abs() > ACCEL_DEADZONE { accel_x_dev } else { 0.0 };
        let accel_y_filtered = if accel_y_dev.abs() > ACCEL_DEADZONE { accel_y_dev } else { 0.0 };
        if !input_disabled {
            translate_x = translate_x * translation_decay + accel_x_filtered * translation_gain * dt;
            translate_y = translate_y * translation_decay + accel_y_filtered * translation_gain * dt;
        }

        let (final_x, final_y) = if in_calib {
            // Neutral during calibration -- nothing to display for
            // deflection to represent yet, and the player's aim is
            // being sampled directly from the camera reading above,
            // not fed through the stick at all.
            (0.0, 0.0)
        } else if let Some((cam_x, cam_y)) = pseye.as_ref().and_then(|p| p.get()) {
            pseye_to_deflection(cam_x, cam_y, &calib_range)
        } else {
            ((aim_x + translate_x).clamp(-32768.0, 32767.0), (aim_y + translate_y).clamp(-32768.0, 32767.0))
        };

        let mut buttons = X360Button::empty();
        if state.cross { buttons |= X360Button::A; }
        if state.circle { buttons |= X360Button::B; }
        if state.square { buttons |= X360Button::X; }
        if state.triangle { buttons |= X360Button::Y; }
        if state.l1 { buttons |= X360Button::LEFT_SHOULDER; }
        if state.r1 { buttons |= X360Button::RIGHT_SHOULDER; }
        if state.share { buttons |= X360Button::BACK; }
        if state.options { buttons |= X360Button::START; }
        if state.ps_guide { buttons |= X360Button::GUIDE; }
        if state.l3 { buttons |= X360Button::LEFT_THUMB; }
        // L2 mapped to a digital button (RIGHT_THUMB, otherwise unused
        // here) instead of sending it as an analog trigger -- removes it
        // from DirectInput's combined-trigger-Z-axis entirely, so R2's
        // analog value no longer gets cancelled out when both are
        // pressed together. Right Stick click itself is still not
        // forwarded (repurposed for aim-reset), so this doesn't
        // conflict with anything.
        if state.l2_click { buttons |= X360Button::RIGHT_THUMB; }
        match state.dpad {
            0 => buttons |= X360Button::DPAD_UP,
            1 => buttons |= X360Button::DPAD_UP | X360Button::DPAD_RIGHT,
            2 => buttons |= X360Button::DPAD_RIGHT,
            3 => buttons |= X360Button::DPAD_DOWN | X360Button::DPAD_RIGHT,
            4 => buttons |= X360Button::DPAD_DOWN,
            5 => buttons |= X360Button::DPAD_DOWN | X360Button::DPAD_LEFT,
            6 => buttons |= X360Button::DPAD_LEFT,
            7 => buttons |= X360Button::DPAD_UP | X360Button::DPAD_LEFT,
            _ => {}
        }

        let report = if input_disabled || in_calib {
            X360Report::default()
        } else {
            X360Report {
                buttons,
                left_trigger: 0, // L2 is now RIGHT_THUMB (see above), not an analog axis
                right_trigger: state.r2_analog,
                thumb_lx: stick_u8_to_i16_x(state.lstick_x),
                thumb_ly: stick_u8_to_i16_y(state.lstick_y),
                thumb_rx: final_x.round() as i16,
                thumb_ry: final_y.round() as i16,
            }
        };
        x360.update(&report)?;


        let mut cfg_dirty = false;

        if state.pad_button {
            let is_fresh_press = pad_button_since.is_none();
            if is_fresh_press {
                pad_button_since = Some(Instant::now());
                pad_button_actioned = false;
            }

            if input_disabled {
                // Resuming fires immediately on press, not release, and
                // not after another 5s hold -- being stuck paused with
                // no easy way back would be a bad experience.
                if is_fresh_press {
                    input_disabled = false;
                    pad_button_actioned = true; // this hold is "used up"
                    println!("Input resumed");
                    apply_led(&writer, is_bt.load(Ordering::Relaxed), led);
                    last_led_resend = Instant::now();
                }
            } else {
                let held_5s = pad_button_since
                    .map(|t| t.elapsed() > Duration::from_millis(DISABLE_HOLD_MS))
                    .unwrap_or(false);
                if held_5s && !pad_button_actioned {
                    // Long-press: pause everything. Marking
                    // pad_button_actioned here (not a separate flag)
                    // means the release-time short-press check below
                    // correctly sees this hold as "already handled" and
                    // won't also cycle the recoil mode.
                    pad_button_actioned = true;
                    input_disabled = true;
                    println!("Input paused (tap Pad button to resume)");
                    let dimmed = dim_led(led);
                    apply_led(&writer, is_bt.load(Ordering::Relaxed), dimmed);
                    last_led_resend = Instant::now();
                }
            }
        } else {
            // Release: a short hold (never reached 5s, and wasn't a
            // resume-tap) cycles the recoil mode. Deciding this on
            // release instead of at a fixed 80ms means it can never
            // race with the 5s disable check on the same hold.
            if let Some(since) = pad_button_since {
                let was_short_press = !pad_button_actioned
                    && since.elapsed() < Duration::from_millis(DISABLE_HOLD_MS)
                    && !input_disabled
                    && last_mode_switch.elapsed() > Duration::from_millis(350);
                if was_short_press {
                    last_mode_switch = Instant::now();
                    recoil_mode = recoil_mode.next();
                    println!("Recoil mode: {}", recoil_mode.label());
                    cfg_dirty = true;

                    let is_bt_now = is_bt.load(Ordering::Relaxed);
                    match recoil_mode {
                        RecoilMode::SingleKick => {
                            fire_pulse(&writer, is_bt_now, recoil_intensity, led);
                            thread::sleep(Duration::from_millis(recoil_duration_ms));
                            stop_pulse(&writer, is_bt_now, led);
                        }
                        RecoilMode::RapidFire => {
                            for _ in 0..2 {
                                fire_pulse(&writer, is_bt_now, recoil_intensity, led);
                                thread::sleep(Duration::from_millis(recoil_duration_ms));
                                stop_pulse(&writer, is_bt_now, led);
                                thread::sleep(Duration::from_millis(
                                    rapidfire_interval_ms - recoil_duration_ms,
                                ));
                            }
                        }
                        RecoilMode::Off => {}
                    }
                    last_led_resend = Instant::now();
                }
            }
            pad_button_since = None;
            pad_button_actioned = false;
        }

        if cfg_dirty {
            let mut cfg_to_save = Config::load();
            cfg_to_save.recoil_mode = recoil_mode.to_saved();
            cfg_to_save.save();
        }

        // Dim the LED while paused -- signals the state visually and
        // saves a little battery since recoil pulses are also
        // suppressed below. Also applied immediately at the moment
        // input_disabled changes (see the pad-button handling above),
        // this just keeps it consistent for the rest of this frame.
        let current_led = if input_disabled { dim_led(led) } else { led };

        if !input_disabled && calib_step == CalibStep::Idle && !suppress_r2_until_release && recoil_mode == RecoilMode::SingleKick && state.r2_click && !last_r2_click {
            recoil_until = Some(Instant::now() + Duration::from_millis(recoil_duration_ms));
            fire_pulse(&writer, is_bt.load(Ordering::Relaxed), recoil_intensity, current_led);
        }
        last_r2_click = state.r2_click;
        r2_held = state.r2_click;

        let pulse_active = recoil_until.map(|t| Instant::now() < t).unwrap_or(false);
        if !pulse_active && recoil_until.is_some() {
            recoil_until = None;
            stop_pulse(&writer, is_bt.load(Ordering::Relaxed), current_led);
            last_led_resend = Instant::now();
        }
        if !input_disabled
            && calib_step == CalibStep::Idle
            && !suppress_r2_until_release
            && recoil_mode == RecoilMode::RapidFire
            && r2_held
            && !pulse_active
            && Instant::now() >= next_rapid_fire
        {
            recoil_until = Some(Instant::now() + Duration::from_millis(recoil_duration_ms));
            next_rapid_fire = Instant::now() + Duration::from_millis(rapidfire_interval_ms);
            fire_pulse(&writer, is_bt.load(Ordering::Relaxed), recoil_intensity, current_led);
        }
        if !r2_held {
            next_rapid_fire = Instant::now();
        }

        // Keep the LED asserted periodically -- firmware has an
        // autonomous idle indicator that overrides an unheld color.
        if !pulse_active && last_led_resend.elapsed() > Duration::from_millis(500) {
            stop_pulse(&writer, is_bt.load(Ordering::Relaxed), current_led);
            last_led_resend = Instant::now();
        }
    }
}

fn fire_pulse(writer: &hidapi::HidDevice, is_bt: bool, intensity: u8, led: (u8, u8, u8)) {
    rumble::send(
        writer,
        is_bt,
        &rumble::RumbleLed { motor_weak: 0, motor_strong: intensity, red: led.0, green: led.1, blue: led.2 },
    );
}

fn stop_pulse(writer: &hidapi::HidDevice, is_bt: bool, led: (u8, u8, u8)) {
    rumble::send(
        writer,
        is_bt,
        &rumble::RumbleLed { motor_weak: 0, motor_strong: 0, red: led.0, green: led.1, blue: led.2 },
    );
}

/// Immediately asserts the given LED color with no rumble -- same as
/// stop_pulse, just named for clarity when the intent is "just set the
/// LED right now" rather than "a recoil pulse just ended."
fn apply_led(writer: &hidapi::HidDevice, is_bt: bool, led: (u8, u8, u8)) {
    stop_pulse(writer, is_bt, led);
}

/// Dimmed to 5% of current brightness -- very faint, near-off, to
/// clearly signal the paused state and minimize battery draw.
fn dim_led(led: (u8, u8, u8)) -> (u8, u8, u8) {
    (
        (led.0 as f32 * 0.05) as u8,
        (led.1 as f32 * 0.05) as u8,
        (led.2 as f32 * 0.05) as u8,
    )
}
