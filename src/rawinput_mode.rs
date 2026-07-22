use crate::aim_report::AimState;
use crate::config::{Config, SavedRecoilMode};
use crate::pseye_client;
use crate::rumble;
use crate::{REAL_PID, REAL_VID};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYBD_EVENT_FLAGS,
    KEYEVENTF_KEYUP, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
    MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP,
    MOUSEINPUT,
};
use windows::Win32::Foundation::POINT;
use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};

// Genuine mouse + keyboard emulation, no virtual gamepad at all -- a
// hybrid (virtual Xbox 360 pad for buttons/sticks, mouse injection only
// for aim) was confusing device detection in TeknoParrot and doesn't
// match this mode's actual purpose (a real lightgun-style PC input
// scheme). Gyro drives the mouse, R2/L2 are mouse clicks, everything
// else is keyboard keys. Real-hardware recoil/LED/pause still work
// exactly as before -- those write directly to the physical Aim
// Controller, independent of whatever virtual device (or lack thereof)
// we present to Windows.
//
// Key mapping is a reasonable-default guess, not something tested
// against a specific game -- treat these as easy to swap if they don't
// match what a given title expects:
//   Left stick -> WASD (movement)
//   Cross -> Space (jump)
//   Circle -> Left Ctrl (crouch)
//   Square -> R (reload)
//   Triangle -> E (interact/use)
//   L1 -> Left Shift (sprint)
//   R1 -> Q (weapon switch)
//   Share -> Backspace
//   Options -> Enter
//   PS/Guide -> Escape
//   D-pad -> Arrow keys
//   R2 -> Left mouse click
//   L2 -> Right mouse click

const VK_W: u16 = 0x57;
const VK_A: u16 = 0x41;
const VK_S: u16 = 0x53;
const VK_D: u16 = 0x44;
const VK_SPACE: u16 = 0x20;
const VK_LCONTROL: u16 = 0xA2;
const VK_R: u16 = 0x52;
const VK_E: u16 = 0x45;
const VK_LSHIFT: u16 = 0xA0;
const VK_Q: u16 = 0x51;
const VK_BACK: u16 = 0x08;
const VK_ESCAPE: u16 = 0x1B;
const VK_RETURN: u16 = 0x0D;
const VK_LEFT: u16 = 0x25;
const VK_UP: u16 = 0x26;
const VK_RIGHT: u16 = 0x27;
const VK_DOWN: u16 = 0x28;

// Shared cursor position in normalized (0-65535) screen space, updated
// by both the gyro (main loop, every HID report) and PS Eye (dedicated
// thread, every camera update) -- real fusion instead of a hard
// either/or switch. Gyro applies its relative delta directly onto this
// shared position for consistent, distance-independent responsiveness;
// PS Eye blends the same position toward its absolute reading to
// correct drift, rather than overwriting it outright. Stored as
// fixed-point (x10) since there's no AtomicF32 in std.
#[derive(Clone)]
struct FusedCursor {
    x: Arc<AtomicI32>,
    y: Arc<AtomicI32>,
}

impl FusedCursor {
    fn new(start_x: f32, start_y: f32) -> Self {
        FusedCursor {
            x: Arc::new(AtomicI32::new((start_x * 10.0) as i32)),
            y: Arc::new(AtomicI32::new((start_y * 10.0) as i32)),
        }
    }
    fn get(&self) -> (f32, f32) {
        (
            self.x.load(Ordering::Relaxed) as f32 / 10.0,
            self.y.load(Ordering::Relaxed) as f32 / 10.0,
        )
    }
    fn set(&self, x: f32, y: f32) {
        self.x.store((x * 10.0) as i32, Ordering::Relaxed);
        self.y.store((y * 10.0) as i32, Ordering::Relaxed);
    }
    /// Adds a relative delta directly (gyro's contribution).
    fn add_delta(&self, dx: f32, dy: f32) {
        let (cx, cy) = self.get();
        self.set(cx + dx, cy + dy);
    }
    /// Blends toward an absolute target (PS Eye's contribution) rather
    /// than snapping to it -- avoids a visible jump each time a fresh
    /// camera position arrives while gyro has been moving things in
    /// between updates.
    fn blend_toward(&self, target_x: f32, target_y: f32, factor: f32) {
        let (cx, cy) = self.get();
        self.set(cx + (target_x - cx) * factor, cy + (target_y - cy) * factor);
    }
}

// Real, measured camera-space edges from 4-point calibration (point at
// each screen corner, pull trigger) -- replaces guessing at a
// sensitivity multiplier with directly capturing the actual usable
// range at the player's actual distance and position. Defaults to the
// full camera frame (matching the old fixed-mapping behavior) until the
// player calibrates for real. Shared across threads; stored fixed-point
// (x10) for the same reason as the other atomics here.
#[derive(Clone)]
struct CalibratedRange {
    left: Arc<AtomicI32>,
    right: Arc<AtomicI32>,
    top: Arc<AtomicI32>,
    bottom: Arc<AtomicI32>,
    // Constant correction applied after the 4-corner mapping, captured
    // by the 5th "center" validation step -- catches lens
    // distortion/misalignment that a purely linear 4-corner mapping
    // can't, which is exactly the kind of drift that caused real
    // in-game misalignment.
    correction_x: Arc<AtomicI32>,
    correction_y: Arc<AtomicI32>,
}

impl CalibratedRange {
    fn default_full_frame() -> Self {
        CalibratedRange {
            left: Arc::new(AtomicI32::new(0)),
            right: Arc::new(AtomicI32::new((PSEYE_FRAME_WIDTH * 10.0) as i32)),
            top: Arc::new(AtomicI32::new(0)),
            bottom: Arc::new(AtomicI32::new((PSEYE_FRAME_HEIGHT * 10.0) as i32)),
            correction_x: Arc::new(AtomicI32::new(0)),
            correction_y: Arc::new(AtomicI32::new(0)),
        }
    }
    fn snapshot(&self) -> CalRangeValues {
        CalRangeValues {
            left: self.left.load(Ordering::Relaxed) as f32 / 10.0,
            right: self.right.load(Ordering::Relaxed) as f32 / 10.0,
            top: self.top.load(Ordering::Relaxed) as f32 / 10.0,
            bottom: self.bottom.load(Ordering::Relaxed) as f32 / 10.0,
            correction_x: self.correction_x.load(Ordering::Relaxed) as f32 / 10.0,
            correction_y: self.correction_y.load(Ordering::Relaxed) as f32 / 10.0,
        }
    }
    fn set(&self, left: f32, right: f32, top: f32, bottom: f32) {
        self.left.store((left * 10.0) as i32, Ordering::Relaxed);
        self.right.store((right * 10.0) as i32, Ordering::Relaxed);
        self.top.store((top * 10.0) as i32, Ordering::Relaxed);
        self.bottom.store((bottom * 10.0) as i32, Ordering::Relaxed);
        // Reset correction whenever the base range is recalibrated --
        // a stale correction from a previous calibration wouldn't be
        // valid against a freshly-captured range.
        self.correction_x.store(0, Ordering::Relaxed);
        self.correction_y.store(0, Ordering::Relaxed);
    }
    fn set_correction(&self, dx: f32, dy: f32) {
        self.correction_x.store((dx * 10.0) as i32, Ordering::Relaxed);
        self.correction_y.store((dy * 10.0) as i32, Ordering::Relaxed);
    }
}

/// Plain snapshot used by pseye_to_normalized, so it doesn't need
/// several separate atomic loads inline at every call site.
struct CalRangeValues {
    left: f32,
    right: f32,
    top: f32,
    bottom: f32,
    correction_x: f32,
    correction_y: f32,
}

/// The 4-point calibration sequence -- point at each screen corner in
/// turn and pull the trigger to confirm. Tracked only in the main loop;
/// the PS Eye thread just reads whatever CalibratedRange currently
/// holds, calibrating or not.
#[derive(Clone, Copy, PartialEq)]
enum CalibStep {
    Idle,
    WaitingTopLeft,
    WaitingTopRight,
    WaitingBottomLeft,
    WaitingBottomRight,
    WaitingCenter,
}

impl CalibStep {
    fn to_code(self) -> i32 {
        match self {
            CalibStep::Idle => 0,
            CalibStep::WaitingTopLeft => 1,
            CalibStep::WaitingTopRight => 2,
            CalibStep::WaitingBottomLeft => 3,
            CalibStep::WaitingBottomRight => 4,
            CalibStep::WaitingCenter => 5,
        }
    }
}

fn gyro_accel(v: f32, accel_threshold: f32, accel_gain: f32) -> f32 {
    let extra = if v.abs() > accel_threshold {
        (1.0 + (v.abs() - accel_threshold) * accel_gain).min(6.0)
    } else {
        1.0
    };
    v * extra
}

/// Sends a corrective burst of relative movement to cancel out
/// everything accumulated since the last reset -- games reading RawInput
/// deltas (like TeknoParrot) accumulate their own internal "virtual
/// position" purely from relative movement, with no connection to the
/// OS cursor's actual position at all, so SetCursorPos has zero effect
/// on them. Sent as several rapid small steps rather than one giant
/// delta, since some input pipelines clamp or drop an excessively large
/// single relative move.
fn recenter_via_relative_burst(total_dx: i32, total_dy: i32) {
    if total_dx == 0 && total_dy == 0 {
        return;
    }
    // More, smaller steps at a slower interval than the first attempt --
    // very rapid SendInput calls may have been dropped or coalesced by
    // the input pipeline, causing inconsistent results each time this
    // fired.
    const STEPS: i32 = 40;
    let step_dx = -total_dx / STEPS;
    let step_dy = -total_dy / STEPS;
    let mut remaining_dx = -total_dx;
    let mut remaining_dy = -total_dy;
    for i in 0..STEPS {
        let dx = if i == STEPS - 1 { remaining_dx } else { step_dx };
        let dy = if i == STEPS - 1 { remaining_dy } else { step_dy };
        send_relative_mouse_move(dx, dy);
        remaining_dx -= dx;
        remaining_dy -= dy;
        thread::sleep(Duration::from_millis(4));
    }
}

fn send_relative_mouse_move(dx: i32, dy: i32) {
    if dx == 0 && dy == 0 {
        return;
    }
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: 0,
                dwFlags: MOUSEEVENTF_MOVE,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    unsafe {
        SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
    }
}

// Camera-tracked absolute cursor positioning, for genuine point-and-aim
// input (real lightgun style) instead of gyro's relative-turn style.
// Windows' absolute mouse API uses a normalized 0-65535 coordinate
// space covering the full virtual desktop, regardless of actual pixel
// resolution -- fractions in, not raw pixels, hence norm_x/norm_y here
// rather than dx/dy.
fn send_absolute_mouse_move(norm_x: i32, norm_y: i32) {
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: norm_x,
                dy: norm_y,
                mouseData: 0,
                dwFlags: MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    unsafe {
        SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
    }
}

// Camera frame resolution from the C++ tracker (fixed at 320x240 for
// now) -- used to map its pixel coordinates onto the normalized
// absolute-cursor coordinate space. First-pass linear mapping (camera's
// full frame -> full screen); genuinely likely to need real calibration
// once tested, same story as every other first-attempt axis mapping in
// this project.
const PSEYE_FRAME_WIDTH: f32 = 320.0;
const PSEYE_FRAME_HEIGHT: f32 = 240.0;

/// Maps a raw camera position to normalized (0-65535) screen space
/// using an actually-measured usable range (from 4-point calibration)
/// rather than a guessed center + sensitivity multiplier. left/top/
/// right/bottom are the calibrated camera-space edges corresponding to
/// the screen's edges -- defaults to the full camera frame before
/// calibration, matching the old fixed-mapping behavior until the user
/// calibrates for real.
fn pseye_to_normalized(cam_x: f32, cam_y: f32, cal: &CalRangeValues) -> (i32, i32) {
    let raw_frac_x = (cam_x - cal.left) / (cal.right - cal.left);
    let raw_frac_y = (cam_y - cal.top) / (cal.bottom - cal.top);
    // Overscan: expand the range beyond what was captured so aiming
    // near the edge still reaches the screen edge. Clamp happens AFTER
    // scaling to normalized coords -- clamping the fraction first (as
    // before) was preventing the overscan from actually reaching 0/65535.
    const OVERSCAN: f32 = 1.08;
    let frac_x = (raw_frac_x - 0.5) * OVERSCAN + 0.5;
    let frac_y = (raw_frac_y - 0.5) * OVERSCAN + 0.5;
    let norm_x = ((1.0 - frac_x) * 65535.0 + cal.correction_x).clamp(0.0, 65535.0) as i32;
    let norm_y = (frac_y * 65535.0 + cal.correction_y).clamp(0.0, 65535.0) as i32;
    (norm_x, norm_y)
}

fn send_mouse_button(left: bool, down: bool) {
    let flag = match (left, down) {
        (true, true) => MOUSEEVENTF_LEFTDOWN,
        (true, false) => MOUSEEVENTF_LEFTUP,
        (false, true) => MOUSEEVENTF_RIGHTDOWN,
        (false, false) => MOUSEEVENTF_RIGHTUP,
    };
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT { dx: 0, dy: 0, mouseData: 0, dwFlags: flag, time: 0, dwExtraInfo: 0 },
        },
    };
    unsafe {
        SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
    }
}

fn send_key(vk: u16, down: bool) {
    let flags = if down { KEYBD_EVENT_FLAGS(0) } else { KEYEVENTF_KEYUP };
    let input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY(vk),
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    unsafe {
        SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
    }
}

/// Sends a key down/up transition only when the state actually changed,
/// so we're not spamming SendInput every single frame for a held key.
fn update_key(vk: u16, now_held: bool, was_held: &mut bool) {
    if now_held != *was_held {
        send_key(vk, now_held);
        *was_held = now_held;
    }
}

fn update_mouse_button(left: bool, now_held: bool, was_held: &mut bool) {
    if now_held != *was_held {
        send_mouse_button(left, now_held);
        *was_held = now_held;
    }
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

/// Releases every key/mouse button we might currently be holding down --
/// used when pausing, so nothing gets stuck held while input is
/// disabled.
struct HeldState {
    w: bool, a: bool, s: bool, d: bool,
    space: bool, lctrl: bool, r: bool, e: bool, lshift: bool, q: bool,
    escape: bool, enter: bool, ps_esc: bool,
    left: bool, up: bool, right: bool, down: bool,
    lmb: bool, rmb: bool,
}

impl HeldState {
    fn new() -> Self {
        HeldState {
            w: false, a: false, s: false, d: false,
            space: false, lctrl: false, r: false, e: false, lshift: false, q: false,
            escape: false, enter: false, ps_esc: false,
            left: false, up: false, right: false, down: false,
            lmb: false, rmb: false,
        }
    }

    fn release_all(&mut self) {
        update_key(VK_W, false, &mut self.w);
        update_key(VK_A, false, &mut self.a);
        update_key(VK_S, false, &mut self.s);
        update_key(VK_D, false, &mut self.d);
        update_key(VK_SPACE, false, &mut self.space);
        update_key(VK_LCONTROL, false, &mut self.lctrl);
        update_key(VK_R, false, &mut self.r);
        update_key(VK_E, false, &mut self.e);
        update_key(VK_LSHIFT, false, &mut self.lshift);
        update_key(VK_Q, false, &mut self.q);
        // NOTE: self.escape now tracks the Share button's Backspace-key
        // state (was Escape, then Return, before this remapping),
        // self.ps_esc tracks the PS/Guide button's Escape-key state.
        update_key(VK_BACK, false, &mut self.escape);
        update_key(VK_RETURN, false, &mut self.enter);
        update_key(VK_ESCAPE, false, &mut self.ps_esc);
        update_key(VK_LEFT, false, &mut self.left);
        update_key(VK_UP, false, &mut self.up);
        update_key(VK_RIGHT, false, &mut self.right);
        update_key(VK_DOWN, false, &mut self.down);
        update_mouse_button(true, false, &mut self.lmb);
        update_mouse_button(false, false, &mut self.rmb);
    }
}

pub fn run(pseye_enabled: bool) -> Result<(), Box<dyn std::error::Error>> {
    println!("Connecting to real Aim Controller ({REAL_VID:04x}:{REAL_PID:04x})...");
    let hid = hidapi::HidApi::new()?;
    let reader = hid.open(REAL_VID, REAL_PID)?;
    let writer = hid.open(REAL_VID, REAL_PID)?;
    println!("Connected to real device.");

    // PS Eye camera-tracked absolute aiming, on top of gyro -- gyro
    // keeps running unconditionally below regardless of this setting,
    // so its bias-correction state never goes stale and it can take
    // over instantly and smoothly the moment the sphere isn't visible
    // (or if --pseye wasn't passed at all).
    let pseye = if pseye_enabled {
        println!("PS Eye tracking enabled -- connecting to camera tracker on 127.0.0.1:9876...");
        Some(pseye_client::start(9876))
    } else {
        None
    };

    // Real screen resolution -- needed to convert gyro's pixel-space
    // delta into the same normalized (0-65535) space PS Eye already
    // works in, so both sources can update one shared position
    // consistently.
    let screen_w = unsafe { GetSystemMetrics(SM_CXSCREEN) }.max(1) as f32;
    let screen_h = unsafe { GetSystemMetrics(SM_CYSCREEN) }.max(1) as f32;

    // Starts centered; PS Eye's first real reading (or a calibration
    // press) will correct it from there.
    let fused_cursor = FusedCursor::new(32768.0, 32768.0);

    // Real, measured calibration -- point at each screen corner and
    // pull the trigger. Replaces the old guessed sensitivity multiplier
    // entirely: this directly captures the actual usable camera range
    // at the player's real distance and position, which is the actual
    // fix for both "far distance doesn't work well" and "doesn't use
    // the whole screen" rather than approximating either with a single
    // number.
    let calib_range = CalibratedRange::default_full_frame();
    let mut calib_step = CalibStep::Idle;
    // Mirrored to the PS Eye thread below (as a plain i32 code) so it
    // can pin the cursor to the actual target corner during each step
    // instead of continuing to drive it off the old, not-yet-calibrated
    // mapping -- gives a clear visual anchor of exactly where to point.
    let calib_step_shared = Arc::new(AtomicI32::new(0));
    let mut calib_top_left: Option<(f32, f32)> = None;
    let mut calib_top_right: Option<(f32, f32)> = None;
    let mut calib_bottom_left: Option<(f32, f32)> = None;
    let mut calib_bottom_right: Option<(f32, f32)> = None;
    let mut last_calib_r2 = false;
    // Sampling state for calibration point capture -- a single
    // instantaneous camera reading at the exact moment of trigger pull
    // was vulnerable to the tracker's normal frame-to-frame jitter,
    // producing a randomly-off capture each time (matching exactly what
    // was observed: the unreachable border varying in size each
    // calibration run, rather than a consistent offset). Averaging
    // several samples over a brief hold cancels that noise out.
    let mut calib_sample_since: Option<Instant> = None;
    let mut calib_sample_sum: (f32, f32) = (0.0, 0.0);
    let mut calib_sample_count: u32 = 0;
    const CALIB_SAMPLE_MS: u64 = 200;
    // Prevents an immediate phantom click right as calibration finishes
    // -- the same physical trigger pull that confirms the final point
    // is still held down the instant calib_step flips back to Idle, so
    // normal R2 firing needs to wait for an actual release first.
    let mut suppress_r2_until_release = false;
    let mut r3_held_since: Option<Instant> = None;
    const CALIB_HOLD_MS: u64 = 800;

    // Shared with the dedicated PS Eye thread below -- that thread runs
    // fully independently of the main loop and had no way to know input
    // was paused, so it kept moving the cursor regardless. Real bug,
    // now fixed by sharing this flag instead of keeping it as a local
    // only the main loop could see.
    let input_disabled_shared = Arc::new(AtomicBool::new(false));
    // Signals the PS Eye thread that gyro has moved -- cancels the
    // real-mouse grace period immediately instead of waiting out the
    // fixed 1500ms timer.
    let gyro_moved = Arc::new(AtomicBool::new(false));

    // PS Eye cursor updates run on their own dedicated, fixed-rate
    // thread -- completely independent of the main loop below, which
    // only wakes up when the controller sends a new HID report. Over
    // Bluetooth that report rate can be well under 60Hz, which was
    // capping cursor smoothness at the controller's report rate rather
    // than the camera's actual frame rate.
    //
    // This thread is the SOLE sender of absolute cursor moves -- gyro
    // (in the main loop below) only ever updates the shared fused
    // position, it never calls SendInput itself. Having both threads
    // independently call SendInput on their own schedules was a real
    // race condition (two threads setting cursor position concurrently
    // from a non-atomic read-modify-write), which was the actual cause
    // of the erratic "goes all over the place" behavior -- not the
    // calibration math, which was already correct. This thread picks up
    // gyro's contribution on its next ~8ms tick regardless of whether a
    // fresh PS Eye reading arrived that cycle, so responsiveness is
    // preserved with only imperceptible added delay.
    if let Some(ref pseye_handle) = pseye {
        let pseye_handle = pseye_handle.clone();
        let fused_cursor = fused_cursor.clone();
        let calib_range = calib_range.clone();
        let calib_step_shared = calib_step_shared.clone();
        let input_disabled_shared = input_disabled_shared.clone();
        let gyro_moved = gyro_moved.clone();
        thread::spawn(move || {
            // Detects when something else (a real physical mouse) has
            // moved the cursor since our own last send, and backs off
            // for a grace period instead of fighting it -- this was
            // the actual cause of not being able to reach the taskbar
            // at all with PS Eye active, since this thread kept pinning
            // the cursor back to its own position every ~8ms regardless
            // of what a real mouse was doing.
            let mut last_sent_px: Option<(i32, i32)> = None;
            let mut real_mouse_until: Option<Instant> = None;
            const REAL_MOUSE_GRACE_MS: u64 = 1500;
            const REAL_MOUSE_TOLERANCE_PX: i32 = 4;

            loop {
                if !input_disabled_shared.load(Ordering::Relaxed) {
                    let cur_pos = unsafe {
                        let mut pt = POINT::default();
                        if GetCursorPos(&mut pt).is_ok() { Some((pt.x, pt.y)) } else { None }
                    };

                    let real_mouse_active = if let (Some((cx, cy)), Some((sx, sy))) = (cur_pos, last_sent_px) {
                        (cx - sx).abs() > REAL_MOUSE_TOLERANCE_PX || (cy - sy).abs() > REAL_MOUSE_TOLERANCE_PX
                    } else {
                        false
                    };

                    if real_mouse_active {
                        real_mouse_until = Some(Instant::now() + Duration::from_millis(REAL_MOUSE_GRACE_MS));
                    }

                    // Gyro movement cancels the grace period immediately
                    if gyro_moved.swap(false, Ordering::Relaxed) {
                        real_mouse_until = None;
                    }

                    let deferring = real_mouse_until.map(|t| Instant::now() < t).unwrap_or(false);

                    if deferring {
                        // Real mouse has control -- track where cursor
                        // actually is so fused_cursor stays synced and
                        // so last_sent_px resets to current position,
                        // preventing the diff from re-triggering active
                        // on the very next frame after we resume.
                        if let Some((cx, cy)) = cur_pos {
                            let norm_x = cx as f32 / screen_w as f32 * 65535.0;
                            let norm_y = cy as f32 / screen_h as f32 * 65535.0;
                            fused_cursor.set(norm_x.clamp(0.0, 65535.0), norm_y.clamp(0.0, 65535.0));
                            last_sent_px = Some((cx, cy));
                        }
                    } else {
                        let calib_code = calib_step_shared.load(Ordering::Relaxed);
                        let in_corner_calib = matches!(calib_code, 1 | 2 | 3 | 4 | 5);

                        if !in_corner_calib {
                            // Normal operation, live-camera-driven.
                            // During corner calibration, this is
                            // deliberately skipped entirely -- the
                            // corner marker is a purely static visual
                            // reference, unaffected by gyro or camera,
                            // until the trigger confirms it.
                            if let Some((cam_x, cam_y)) = pseye_handle.get() {
                                let cal = calib_range.snapshot();
                                let (norm_x, norm_y) = pseye_to_normalized(cam_x, cam_y, &cal);
                                fused_cursor.blend_toward(norm_x as f32, norm_y as f32, 0.8);
                            }
                        }

                        let (fx, fy) = fused_cursor.get();
                        send_absolute_mouse_move(fx as i32, fy as i32);
                        last_sent_px = cur_pos.map(|_| {
                            // Store in the same pixel space GetCursorPos
                            // reports, so next tick's comparison is
                            // apples-to-apples -- SendInput's normalized
                            // coordinates round to real pixels via the
                            // OS, so read back the actual result rather
                            // than assuming an exact conversion.
                            let mut pt = POINT::default();
                            unsafe { let _ = GetCursorPos(&mut pt); }
                            (pt.x, pt.y)
                        });
                    }
                }
                thread::sleep(Duration::from_millis(8)); // ~120Hz
            }
        });
    }

    for (report_id, size) in [(0x02usize, 37usize), (0xa3, 49), (0x12, 16)] {
        let mut buf = vec![0u8; size];
        buf[0] = report_id as u8;
        let _ = reader.get_feature_report(&mut buf);
        thread::sleep(Duration::from_millis(50));
    }

    println!("Emulating mouse + keyboard (no virtual gamepad). Relaying input -- Ctrl+C to stop.");

    let cfg = Config::load();
    let sensitivity = cfg.lightgun_sensitivity;
    let accel_threshold = cfg.lightgun_accel_threshold;
    let accel_gain = cfg.lightgun_accel_gain;
    let recoil_intensity = cfg.lightgun_recoil_intensity;
    let recoil_duration_ms = cfg.lightgun_recoil_duration_ms;
    let rapidfire_interval_ms = cfg.lightgun_rapidfire_interval_ms;
    // When PS Eye is active, match the color the camera tracker
    // expects (its default target) rather than the usual orange --
    // both programs write to the LED independently with no
    // coordination between them, so agreeing on the color avoids the
    // visible blink/conflict rather than actually solving the
    // underlying "two owners" issue.
    let led: (u8, u8, u8) = if pseye_enabled { (80, 0, 150) } else { cfg.lightgun_raw_led };

    let mut recoil_mode = match cfg.recoil_mode {
        SavedRecoilMode::SingleKick => RecoilMode::SingleKick,
        SavedRecoilMode::RapidFire => RecoilMode::RapidFire,
        SavedRecoilMode::Off => RecoilMode::Off,
    };
    println!("Recoil mode: {} (Pad button cycles modes, hold 5s to pause all input, Right Stick click recalibrates gyro)", recoil_mode.label());

    let is_bt = Arc::new(AtomicBool::new(false));
    let mut last_sensor_time = Instant::now();
    let mut last_ps_guide = false;
    let mut last_r3 = false;
    let mut recoil_until: Option<Instant> = None;
    let mut next_rapid_fire = Instant::now();
    #[allow(unused_assignments)]
    let mut r2_held_for_recoil = false;
    let mut pad_button_since: Option<Instant> = None;
    let mut pad_button_actioned = false;
    let mut last_mode_switch = Instant::now() - Duration::from_secs(10);
    let mut last_led_resend = Instant::now() - Duration::from_secs(10);
    let mut input_disabled = false;
    const DISABLE_HOLD_MS: u64 = 5000;

    let mut roll_bias: f32 = 0.0;
    let mut pitch_bias: f32 = 0.0;
    let mut yaw_bias: f32 = 0.0;
    const STILLNESS_THRESHOLD: f32 = 30.0;
    const BIAS_ADAPT_RATE: f32 = 0.02;
    const GYRO_DEADZONE: f32 = 15.0;
    // Temporarily boosted after a manual recalibration request, so it
    // converges quickly but smoothly rather than snapping straight to a
    // single (possibly button-press-jolted) sample -- an instant snap
    // was baking the press's own physical jolt into the new baseline,
    // causing a fresh drift immediately after recalibrating.
    let mut fast_recalibrate_until: Option<Instant> = None;

    let mut pixel_remainder_x: f32 = 0.0;
    let mut pixel_remainder_y: f32 = 0.0;
    // Running total of everything sent since the last reset -- lets R3
    // send a corrective burst that cancels it back out.
    let mut accumulated_dx: i32 = 0;
    let mut accumulated_dy: i32 = 0;

    let mut held = HeldState::new();
    // Requires a few consecutive "not found" readings before actually

    let mut raw_buf = [0u8; 78];
    let mut consecutive_errors: u32 = 0;
    const DISCONNECT_THRESHOLD: u32 = 10;
    loop {
        let n = match reader.read_timeout(&mut raw_buf, 100) {
            Ok(n) => { consecutive_errors = 0; n }
            Err(e) => {
                consecutive_errors += 1;
                if consecutive_errors == 1 { eprintln!("rawinput read error: {e}"); }
                if consecutive_errors >= DISCONNECT_THRESHOLD {
                    held.release_all();
                    return Err(format!("device disconnected: {e}").into());
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
                continue;
            }
        };
        if n == 0 {
            continue;
        }
        let Some(state) = AimState::parse(&raw_buf[..n]) else {
            continue;
        };
        is_bt.store(raw_buf[0] == 0x11 || n < 24, Ordering::Relaxed);

        if state.ps_guide && !last_ps_guide {
            pixel_remainder_x = 0.0;
            pixel_remainder_y = 0.0;
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
                    calib_step_shared.store(calib_step.to_code(), Ordering::Relaxed);
                    fused_cursor.set(0.0, 0.0);
                    println!("PS Eye calibration: point at TOP-LEFT corner and pull trigger");
                    r3_held_since = None; // consumed -- don't also fire the short-tap gyro recal on release
                }
            }
        }
        if !state.r3 && last_r3 {
            if let Some(_since) = r3_held_since {
                // Released before the hold threshold -- short tap,
                // gyro recalibration only (unrelated to PS Eye). Real
                // bug fixed here: this used to fire even while
                // calibration was already active, since r3_held_since
                // only got consumed on the *hold-to-enter* path, which
                // never runs once calib_step is no longer Idle -- so
                // any R3 press during an active calibration sequence
                // fell through here on release, sending a gyro recal
                // burst that visibly yanked the cursor away from
                // wherever it had just been snapped.
                if calib_step == CalibStep::Idle {
                    fast_recalibrate_until = Some(Instant::now() + Duration::from_millis(600));
                    recenter_via_relative_burst(accumulated_dx, accumulated_dy);
                    accumulated_dx = 0;
                    accumulated_dy = 0;
                    println!("Recalibrating gyro...");
                }
            }
            r3_held_since = None;
        }
        last_r3 = state.r3;

        // 4-point calibration sequence: pull the trigger to confirm
        // each corner in turn. Genuine measured range instead of a
        // guessed multiplier -- directly captures the actual usable
        // camera area at the player's real distance and position.
        if calib_step != CalibStep::Idle {
            if let Some(ref pseye_handle) = pseye {
                if state.r2_click {
                    if !last_calib_r2 {
                        // Fresh press -- start a new sampling window
                        // rather than capturing immediately.
                        calib_sample_since = Some(Instant::now());
                        calib_sample_sum = (0.0, 0.0);
                        calib_sample_count = 0;
                    }
                    if let Some((cam_x, cam_y)) = pseye_handle.get() {
                        calib_sample_sum.0 += cam_x;
                        calib_sample_sum.1 += cam_y;
                        calib_sample_count += 1;
                    }
                    let ready = calib_sample_since
                        .map(|since| since.elapsed() >= Duration::from_millis(CALIB_SAMPLE_MS))
                        .unwrap_or(false);
                    if ready {
                        if calib_sample_count == 0 {
                            println!("Sphere not visible -- point at the sphere in camera view before pulling trigger");
                            calib_sample_since = None;
                        } else {
                            let cam_x = calib_sample_sum.0 / calib_sample_count as f32;
                            let cam_y = calib_sample_sum.1 / calib_sample_count as f32;
                            calib_sample_since = None;
                            match calib_step {
                                CalibStep::WaitingTopLeft => {
                                    calib_top_left = Some((cam_x, cam_y));
                                    calib_step = CalibStep::WaitingTopRight;
                                    calib_step_shared.store(calib_step.to_code(), Ordering::Relaxed);
                                    fused_cursor.set(65535.0, 0.0);
                                    println!("Point at TOP-RIGHT corner and pull trigger");
                                }
                                CalibStep::WaitingTopRight => {
                                    calib_top_right = Some((cam_x, cam_y));
                                    calib_step = CalibStep::WaitingBottomLeft;
                                    calib_step_shared.store(calib_step.to_code(), Ordering::Relaxed);
                                    fused_cursor.set(0.0, 65535.0);
                                    println!("Point at BOTTOM-LEFT corner and pull trigger");
                                }
                                CalibStep::WaitingBottomLeft => {
                                    calib_bottom_left = Some((cam_x, cam_y));
                                    calib_step = CalibStep::WaitingBottomRight;
                                    calib_step_shared.store(calib_step.to_code(), Ordering::Relaxed);
                                    fused_cursor.set(65535.0, 65535.0);
                                    println!("Point at BOTTOM-RIGHT corner and pull trigger");
                                }
                                CalibStep::WaitingBottomRight => {
                                    calib_bottom_right = Some((cam_x, cam_y));
                                    // All 4 corners captured -- average
                                    // the left/right and top/bottom
                                    // edges from both corners on each
                                    // side for a steadier result than
                                    // trusting a single point per edge.
                                    if let (Some(tl), Some(tr), Some(bl), Some(br)) =
                                        (calib_top_left, calib_top_right, calib_bottom_left, calib_bottom_right)
                                    {
                                        let left = (tl.0 + bl.0) / 2.0;
                                        let right = (tr.0 + br.0) / 2.0;
                                        let top = (tl.1 + tr.1) / 2.0;
                                        let bottom = (bl.1 + br.1) / 2.0;
                                        calib_range.set(left.min(right), left.max(right), top.min(bottom), top.max(bottom));
                                    }
                                    calib_step = CalibStep::WaitingCenter;
                                    calib_step_shared.store(calib_step.to_code(), Ordering::Relaxed);
                                    fused_cursor.set(32768.0, 32768.0);
                                    println!("Now point at the CENTER of the screen and pull trigger");
                                }
                                CalibStep::WaitingCenter => {
                                    // Same static engineering as the 4
                                    // corners -- the marker held still
                                    // at screen center the whole time,
                                    // so wherever the player is
                                    // physically pointing when the
                                    // sampling window elapses is a
                                    // clean, averaged reading. Compares
                                    // what the just-captured 4-corner
                                    // mapping would have predicted for
                                    // this camera reading against true
                                    // center -- the gap becomes a
                                    // constant correction applied to
                                    // every future reading, catching
                                    // lens distortion/misalignment a
                                    // purely linear 4-corner mapping
                                    // can miss.
                                    let cal = calib_range.snapshot();
                                    let (predicted_x, predicted_y) = pseye_to_normalized(cam_x, cam_y, &cal);
                                    let dx = 32768.0 - predicted_x as f32;
                                    let dy = 32768.0 - predicted_y as f32;
                                    calib_range.set_correction(dx, dy);
                                    println!("PS Eye calibration complete!");
                                    calib_step = CalibStep::Idle;
                                    calib_step_shared.store(calib_step.to_code(), Ordering::Relaxed);
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
        }
        last_calib_r2 = state.r2_click;
        if suppress_r2_until_release && !state.r2_click {
            suppress_r2_until_release = false;
        }

        let dt = last_sensor_time.elapsed().as_secs_f32().min(0.1);
        last_sensor_time = Instant::now();

        let adapt_rate = if fast_recalibrate_until.map(|t| Instant::now() < t).unwrap_or(false) {
            0.4 // fast convergence window after a manual recalibration request
        } else {
            BIAS_ADAPT_RATE
        };
        if fast_recalibrate_until.map(|t| Instant::now() >= t).unwrap_or(false) {
            fast_recalibrate_until = None;
        }

        if (state.gyro_roll as f32).abs() < STILLNESS_THRESHOLD || adapt_rate > BIAS_ADAPT_RATE {
            roll_bias += (state.gyro_roll as f32 - roll_bias) * (adapt_rate * dt).clamp(0.0, 1.0);
        }
        if (state.gyro_pitch as f32).abs() < STILLNESS_THRESHOLD || adapt_rate > BIAS_ADAPT_RATE {
            pitch_bias += (state.gyro_pitch as f32 - pitch_bias) * (adapt_rate * dt).clamp(0.0, 1.0);
        }
        if (state.gyro_yaw as f32).abs() < STILLNESS_THRESHOLD || adapt_rate > BIAS_ADAPT_RATE {
            yaw_bias += (state.gyro_yaw as f32 - yaw_bias) * (adapt_rate * dt).clamp(0.0, 1.0);
        }

        let roll_corrected = state.gyro_roll as f32 - roll_bias;
        let pitch_corrected = state.gyro_pitch as f32 - pitch_bias;
        let yaw_corrected = state.gyro_yaw as f32 - yaw_bias;

        let roll_corrected = if roll_corrected.abs() < GYRO_DEADZONE { 0.0 } else { roll_corrected };
        let pitch_corrected = if pitch_corrected.abs() < GYRO_DEADZONE { 0.0 } else { pitch_corrected };
        let yaw_corrected = if yaw_corrected.abs() < GYRO_DEADZONE { 0.0 } else { yaw_corrected };

        if !input_disabled {
            let gyro_dx = (gyro_accel(roll_corrected, accel_threshold, accel_gain)
                + gyro_accel(pitch_corrected, accel_threshold, accel_gain))
                * -1.0
                * sensitivity
                * dt
                * 0.05;
            let gyro_dy = gyro_accel(yaw_corrected, accel_threshold, accel_gain) * -1.0 * sensitivity * dt * 0.05;

            let total_dx = gyro_dx + pixel_remainder_x;
            let total_dy = gyro_dy + pixel_remainder_y;
            let move_dx = total_dx.trunc() as i32;
            let move_dy = total_dy.trunc() as i32;
            pixel_remainder_x = total_dx.fract();
            pixel_remainder_y = total_dy.fract();

            if let Some(ref pseye_handle) = pseye {
                // Gyro only contributes when PS Eye genuinely doesn't
                // have a current position -- letting gyro add its delta
                // continuously even while PS Eye was actively tracking
                // caused a real, visible drag/drift over time (gyro
                // integrates tiny bias/noise every single HID report,
                // and the blend-correction wasn't strong enough to keep
                // up with that against genuine, sustained hand
                // movement). PS Eye alone is the primary driver whenever
                // it's actually tracking; gyro is purely a fallback for
                // when the sphere briefly isn't visible.
                //
                // During the 4 corner calibration steps specifically,
                // gyro contributes nothing at all -- the corner marker
                // is a purely static visual reference for where to aim,
                // not something meant to move. Letting anything nudge
                // it (gyro drift, camera noise) just reintroduced
                // uncertainty about where the marker actually was.
                let in_corner_calib = matches!(
                    calib_step,
                    CalibStep::WaitingTopLeft
                        | CalibStep::WaitingTopRight
                        | CalibStep::WaitingBottomLeft
                        | CalibStep::WaitingBottomRight
                        | CalibStep::WaitingCenter
                );
                let pseye_has_position = pseye_handle.get().is_some();
                if !pseye_has_position && !in_corner_calib {
                    let norm_dx = move_dx as f32 / screen_w * 65535.0;
                    let norm_dy = move_dy as f32 / screen_h * 65535.0;
                    fused_cursor.add_delta(norm_dx, norm_dy);
                    if move_dx != 0 || move_dy != 0 {
                        gyro_moved.store(true, Ordering::Relaxed);
                    }
                }

                pixel_remainder_x = 0.0;
                pixel_remainder_y = 0.0;
            } else {
                send_relative_mouse_move(move_dx, move_dy);
                accumulated_dx += move_dx;
                accumulated_dy += move_dy;
            }

            // Left stick -> WASD, threshold-based digitization.
            const STICK_THRESHOLD: i32 = 40;
            let lx = state.lstick_x as i32 - 128;
            let ly = state.lstick_y as i32 - 128;
            update_key(VK_W, ly < -STICK_THRESHOLD, &mut held.w);
            update_key(VK_S, ly > STICK_THRESHOLD, &mut held.s);
            update_key(VK_A, lx < -STICK_THRESHOLD, &mut held.a);
            update_key(VK_D, lx > STICK_THRESHOLD, &mut held.d);

            update_key(VK_SPACE, state.cross, &mut held.space);
            update_key(VK_LCONTROL, state.circle, &mut held.lctrl);
            update_key(VK_R, state.square, &mut held.r);
            update_key(VK_E, state.triangle, &mut held.e);
            update_key(VK_LSHIFT, state.l1, &mut held.lshift);
            update_key(VK_Q, state.r1, &mut held.q);
            update_key(VK_BACK, state.share, &mut held.escape);
            update_key(VK_RETURN, state.options, &mut held.enter);
            update_key(VK_ESCAPE, state.ps_guide, &mut held.ps_esc);

            update_key(VK_UP, state.dpad == 0, &mut held.up);
            update_key(VK_DOWN, state.dpad == 4, &mut held.down);
            update_key(VK_LEFT, state.dpad == 6, &mut held.left);
            update_key(VK_RIGHT, state.dpad == 2, &mut held.right);

            update_mouse_button(true, state.r2_click && calib_step == CalibStep::Idle && !suppress_r2_until_release, &mut held.lmb);
            update_mouse_button(false, state.l2_click, &mut held.rmb);
        } else {
            held.release_all();
        }

        let mut cfg_dirty = false;

        if state.pad_button {
            let is_fresh_press = pad_button_since.is_none();
            if is_fresh_press {
                pad_button_since = Some(Instant::now());
                pad_button_actioned = false;
            }

            if input_disabled {
                if is_fresh_press {
                    input_disabled = false;
                    input_disabled_shared.store(false, Ordering::Relaxed);
                    pad_button_actioned = true;
                    println!("Input resumed");
                    apply_led(&writer, is_bt.load(Ordering::Relaxed), led);
                    last_led_resend = Instant::now();
                }
            } else {
                let held_5s = pad_button_since
                    .map(|t| t.elapsed() > Duration::from_millis(DISABLE_HOLD_MS))
                    .unwrap_or(false);
                if held_5s && !pad_button_actioned {
                    pad_button_actioned = true;
                    input_disabled = true;
                    input_disabled_shared.store(true, Ordering::Relaxed);
                    println!("Input paused (tap Pad button to resume)");
                    held.release_all();
                    let dimmed = dim_led(led);
                    apply_led(&writer, is_bt.load(Ordering::Relaxed), dimmed);
                    last_led_resend = Instant::now();
                }
            }
        } else {
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

                    // Open a second HID handle for the preview so
                    // the preview thread can sleep without blocking the
                    // main loop. The main writer keeps running normally.
                    let is_bt_now = is_bt.load(Ordering::Relaxed);
                    let preview_hid = hidapi::HidApi::new()
                        .and_then(|h| h.open(REAL_VID, REAL_PID));
                    match recoil_mode {
                        RecoilMode::SingleKick => {
                            if let Ok(pw) = preview_hid {
                                thread::spawn(move || {
                                    fire_pulse(&pw, is_bt_now, recoil_intensity, led);
                                    thread::sleep(Duration::from_millis(recoil_duration_ms));
                                    stop_pulse(&pw, is_bt_now, led);
                                });
                            }
                        }
                        RecoilMode::RapidFire => {
                            if let Ok(pw) = preview_hid {
                                thread::spawn(move || {
                                    for _ in 0..2 {
                                        fire_pulse(&pw, is_bt_now, recoil_intensity, led);
                                        thread::sleep(Duration::from_millis(recoil_duration_ms));
                                        stop_pulse(&pw, is_bt_now, led);
                                        thread::sleep(Duration::from_millis(
                                            rapidfire_interval_ms.saturating_sub(recoil_duration_ms),
                                        ));
                                    }
                                });
                            }
                        }
                        RecoilMode::Off => {
                            stop_pulse(&writer, is_bt_now, led);
                        }
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

        let current_led = if input_disabled { dim_led(led) } else { led };

        if !input_disabled && calib_step == CalibStep::Idle && !suppress_r2_until_release && recoil_mode == RecoilMode::SingleKick && state.r2_click && !r2_held_for_recoil {
            recoil_until = Some(Instant::now() + Duration::from_millis(recoil_duration_ms));
            fire_pulse(&writer, is_bt.load(Ordering::Relaxed), recoil_intensity, current_led);
        }
        r2_held_for_recoil = state.r2_click;

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
            && state.r2_click
            && !pulse_active
            && Instant::now() >= next_rapid_fire
        {
            recoil_until = Some(Instant::now() + Duration::from_millis(recoil_duration_ms));
            next_rapid_fire = Instant::now() + Duration::from_millis(rapidfire_interval_ms);
            fire_pulse(&writer, is_bt.load(Ordering::Relaxed), recoil_intensity, current_led);
        }
        if !state.r2_click {
            next_rapid_fire = Instant::now();
        }

        if !pulse_active && last_led_resend.elapsed() > Duration::from_millis(500) {
            stop_pulse(&writer, is_bt.load(Ordering::Relaxed), current_led);
            last_led_resend = Instant::now();
        }
    }
    // Release all held keys/buttons on exit so nothing gets stuck
    held.release_all();
    update_mouse_button(true, false, &mut held.lmb);
    update_mouse_button(false, false, &mut held.rmb);
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

fn apply_led(writer: &hidapi::HidDevice, is_bt: bool, led: (u8, u8, u8)) {
    stop_pulse(writer, is_bt, led);
}

fn dim_led(led: (u8, u8, u8)) -> (u8, u8, u8) {
    (
        (led.0 as f32 * 0.05) as u8,
        (led.1 as f32 * 0.05) as u8,
        (led.2 as f32 * 0.05) as u8,
    )
}
