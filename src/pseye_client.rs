//! Client for the PS Eye camera tracker's position broadcast (see the
//! C++ `ps_aim_tracker_ui` companion tool, which listens on this same
//! port and sends plain-text "found,x,y\n" lines). Runs entirely on its
//! own thread so a slow/missing camera-tracker connection can never
//! block the main HID-polling loop that everything else depends on.
//! If --pseye isn't passed, this module is never touched at all --
//! existing gyro-only behavior is completely unaffected.

use std::io::{BufRead, BufReader};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

#[derive(Clone)]
pub struct PsEyePosition {
    found: Arc<AtomicBool>,
    // Stored as fixed-point (x10) in an atomic int, since there's no
    // AtomicF32 in std -- avoids needing a Mutex for something read
    // every frame on a hot path.
    x_fixed: Arc<AtomicI32>,
    y_fixed: Arc<AtomicI32>,
}

impl PsEyePosition {
    pub fn get(&self) -> Option<(f32, f32)> {
        if self.found.load(Ordering::Relaxed) {
            Some((
                self.x_fixed.load(Ordering::Relaxed) as f32 / 10.0,
                self.y_fixed.load(Ordering::Relaxed) as f32 / 10.0,
            ))
        } else {
            None
        }
    }
}

/// Spawns the background connection thread and returns a handle for
/// reading the latest position. Camera frame resolution (320x240) is
/// fixed on the C++ tracker side for now -- returned here so callers
/// can map camera pixel coordinates to screen coordinates.
pub fn start(port: u16) -> PsEyePosition {
    let handle = PsEyePosition {
        found: Arc::new(AtomicBool::new(false)),
        x_fixed: Arc::new(AtomicI32::new(0)),
        y_fixed: Arc::new(AtomicI32::new(0)),
    };

    let thread_handle = handle.clone();
    thread::spawn(move || {
        loop {
            match TcpStream::connect(("127.0.0.1", port)) {
                Ok(stream) => {
                    println!("PS Eye tracker connected (127.0.0.1:{port}).");
                    let mut reader = BufReader::new(stream);
                    let mut line = String::new();
                    loop {
                        line.clear();
                        match reader.read_line(&mut line) {
                            Ok(0) => break, // connection closed
                            Ok(_) => {
                                if let Some((found, x, y)) = parse_line(&line) {
                                    thread_handle.found.store(found, Ordering::Relaxed);
                                    thread_handle.x_fixed.store((x * 10.0) as i32, Ordering::Relaxed);
                                    thread_handle.y_fixed.store((y * 10.0) as i32, Ordering::Relaxed);
                                }
                            }
                            Err(_) => break,
                        }
                    }
                    println!("PS Eye tracker disconnected, will retry...");
                    thread_handle.found.store(false, Ordering::Relaxed);
                }
                Err(_) => {
                    // Tracker not running yet / not reachable -- keep
                    // retrying quietly rather than spamming the console.
                }
            }
            thread::sleep(Duration::from_millis(1000));
        }
    });

    handle
}

/// Parses a "found,x,y" line, e.g. "1,182.3,70.1". Returns None for
/// anything malformed rather than panicking -- a corrupted or partial
/// line should just be skipped, not crash the reader thread.
fn parse_line(line: &str) -> Option<(bool, f32, f32)> {
    let line = line.trim();
    let mut parts = line.split(',');
    let found = parts.next()?.parse::<i32>().ok()? != 0;
    let x = parts.next()?.parse::<f32>().ok()?;
    let y = parts.next()?.parse::<f32>().ok()?;
    Some((found, x, y))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_line() {
        assert_eq!(parse_line("1,182.3,70.1"), Some((true, 182.3, 70.1)));
        assert_eq!(parse_line("0,0.0,0.0\n"), Some((false, 0.0, 0.0)));
    }

    #[test]
    fn rejects_malformed_line() {
        assert_eq!(parse_line(""), None);
        assert_eq!(parse_line("garbage"), None);
        assert_eq!(parse_line("1,182.3"), None);
    }
}
