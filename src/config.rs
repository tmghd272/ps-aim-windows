//! Persistent settings for all modes.
//! Stored as `key=value` under `%APPDATA%\ps-aim-windows\config.txt`.

use std::collections::HashMap;
use std::io::Write;

#[derive(Clone, Copy, PartialEq)]
pub enum SavedRecoilMode {
    SingleKick,
    RapidFire,
    Off,
}

pub struct Config {
    pub recoil_mode: SavedRecoilMode,
    pub lightgun_sensitivity: f32,
    pub lightgun_accel_threshold: f32,
    pub lightgun_accel_gain: f32,
    pub lightgun_recoil_intensity: u8,
    pub lightgun_recoil_duration_ms: u64,
    pub lightgun_rapidfire_interval_ms: u64,
    /// LED color used in --lightgun (XInput) mode.
    pub lightgun_led: (u8, u8, u8),
    /// LED color used in --lightgun-raw (RawInput) mode.
    /// Separate from lightgun_led so each mode can be tuned independently.
    pub lightgun_raw_led: (u8, u8, u8),
    pub lightgun_translation_gain: f32,
    pub lightgun_translation_decay: f32,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            recoil_mode: SavedRecoilMode::SingleKick,
            lightgun_sensitivity: 75.0,
            lightgun_accel_threshold: 2500.0,
            lightgun_accel_gain: 0.0006,
            lightgun_recoil_intensity: 255,
            lightgun_recoil_duration_ms: 140,
            lightgun_rapidfire_interval_ms: 200,
            lightgun_led:     (0, 128, 0),
            lightgun_raw_led: (255, 100, 0),
            lightgun_translation_gain: 12.0,
            lightgun_translation_decay: 0.85,
        }
    }
}

fn config_path() -> std::path::PathBuf {
    let appdata = std::env::var("APPDATA").unwrap_or_else(|_| ".".to_string());
    std::path::PathBuf::from(appdata).join("ps-aim-windows").join("config.txt")
}

impl Config {
    pub fn load() -> Self {
        let path = config_path();
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => return Config::default(),
        };
        let mut map: HashMap<String, String> = HashMap::new();
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') { continue; }
            if let Some((k, v)) = line.split_once('=') {
                map.insert(k.trim().to_string(), v.trim().to_string());
            }
        }
        let mut cfg = Config::default();
        if let Some(v) = map.get("recoil_mode") {
            cfg.recoil_mode = match v.as_str() {
                "rapid_fire" => SavedRecoilMode::RapidFire,
                "off"        => SavedRecoilMode::Off,
                _            => SavedRecoilMode::SingleKick,
            };
        }
        macro_rules! load_f32 { ($k:expr, $f:expr) => {
            if let Some(v) = map.get($k).and_then(|s| s.parse().ok()) { $f = v; }
        }}
        macro_rules! load_u8 { ($k:expr, $f:expr) => {
            if let Some(v) = map.get($k).and_then(|s| s.parse().ok()) { $f = v; }
        }}
        macro_rules! load_u64 { ($k:expr, $f:expr) => {
            if let Some(v) = map.get($k).and_then(|s| s.parse().ok()) { $f = v; }
        }}
        load_f32!("lightgun_sensitivity",          cfg.lightgun_sensitivity);
        load_f32!("lightgun_accel_threshold",      cfg.lightgun_accel_threshold);
        load_f32!("lightgun_accel_gain",           cfg.lightgun_accel_gain);
        load_u8! ("lightgun_recoil_intensity",     cfg.lightgun_recoil_intensity);
        load_u64!("lightgun_recoil_duration_ms",   cfg.lightgun_recoil_duration_ms);
        load_u64!("lightgun_rapidfire_interval_ms",cfg.lightgun_rapidfire_interval_ms);
        load_f32!("lightgun_translation_gain",     cfg.lightgun_translation_gain);
        load_f32!("lightgun_translation_decay",    cfg.lightgun_translation_decay);
        // XInput LED
        let (r, g, b) = (
            map.get("lightgun_led_r").and_then(|s| s.parse().ok()),
            map.get("lightgun_led_g").and_then(|s| s.parse().ok()),
            map.get("lightgun_led_b").and_then(|s| s.parse().ok()),
        );
        if let (Some(r), Some(g), Some(b)) = (r, g, b) { cfg.lightgun_led = (r, g, b); }
        // RawInput LED (separate key so the UI can tune them independently)
        let (rr, rg, rb) = (
            map.get("lightgun_raw_led_r").and_then(|s| s.parse().ok()),
            map.get("lightgun_raw_led_g").and_then(|s| s.parse().ok()),
            map.get("lightgun_raw_led_b").and_then(|s| s.parse().ok()),
        );
        if let (Some(r), Some(g), Some(b)) = (rr, rg, rb) { cfg.lightgun_raw_led = (r, g, b); }
        cfg
    }

    pub fn save(&self) {
        let path = config_path();
        if let Some(parent) = path.parent() { let _ = std::fs::create_dir_all(parent); }
        let recoil_str = match self.recoil_mode {
            SavedRecoilMode::SingleKick => "single_kick",
            SavedRecoilMode::RapidFire  => "rapid_fire",
            SavedRecoilMode::Off        => "off",
        };
        let contents = format!(
            "# ps-aim-windows config -- edited by UI or driver\n\
             recoil_mode={recoil_str}\n\
             lightgun_sensitivity={}\n\
             lightgun_accel_threshold={}\n\
             lightgun_accel_gain={}\n\
             lightgun_recoil_intensity={}\n\
             lightgun_recoil_duration_ms={}\n\
             lightgun_rapidfire_interval_ms={}\n\
             lightgun_led_r={}\n\
             lightgun_led_g={}\n\
             lightgun_led_b={}\n\
             lightgun_raw_led_r={}\n\
             lightgun_raw_led_g={}\n\
             lightgun_raw_led_b={}\n\
             lightgun_translation_gain={}\n\
             lightgun_translation_decay={}\n",
            self.lightgun_sensitivity,
            self.lightgun_accel_threshold,
            self.lightgun_accel_gain,
            self.lightgun_recoil_intensity,
            self.lightgun_recoil_duration_ms,
            self.lightgun_rapidfire_interval_ms,
            self.lightgun_led.0, self.lightgun_led.1, self.lightgun_led.2,
            self.lightgun_raw_led.0, self.lightgun_raw_led.1, self.lightgun_raw_led.2,
            self.lightgun_translation_gain,
            self.lightgun_translation_decay,
        );
        if let Ok(mut f) = std::fs::File::create(&path) { let _ = f.write_all(contents.as_bytes()); }
    }
}
