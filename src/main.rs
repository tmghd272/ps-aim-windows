mod aim_report;
mod config;
mod crc32;
mod ds4_mode;
mod hidhide;
mod lightgun_mode;
mod pseye_client;
mod rawinput_mode;
mod rumble;

use std::os::windows::io::AsRawHandle;

pub const REAL_VID: u16 = 0x054C;
pub const REAL_PID: u16 = 0x0BB2;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let has_flag = |f: &str| args.iter().any(|a| a == f);

    if has_flag("--setup-hidhide") {
        return hidhide::run_setup();
    }
    if has_flag("--wipe-hidhide") {
        return hidhide::run_wipe();
    }

    // ── Single-instance guard ────────────────────────────────────────────────
    // Prevents duplicate driver processes when the UI restarts the driver on
    // mode switch. A named Win32 mutex is the standard Windows approach --
    // the second instance detects it, kills the existing one, and takes over.
    // This means mode switches always cleanly replace the running instance
    // rather than leaving orphaned processes.
    let mutex_name = "Global\\PsAimWindowsDriver\0";
    let mutex_handle = unsafe {
        use windows::Win32::System::Threading::CreateMutexW;
        use windows::core::PCWSTR;
        let wide: Vec<u16> = mutex_name.encode_utf16().collect();
        CreateMutexW(None, true, PCWSTR(wide.as_ptr())).unwrap_or_default()
    };
    let already_running = unsafe {
        windows::Win32::Foundation::GetLastError()
    } == windows::Win32::Foundation::ERROR_ALREADY_EXISTS;

    if already_running {
        // Kill the existing instance so our new mode/flags take over cleanly
        eprintln!("Another ps-aim-windows instance detected -- replacing it.");
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/IM", "ps-aim-windows.exe"])
            .output();
        std::thread::sleep(std::time::Duration::from_millis(400));
    }

    let pseye_enabled = has_flag("--pseye");

    // ── PS Eye tracker auto-launch ───────────────────────────────────────────
    // Spawns the headless tracker as a hidden child process when --pseye is
    // passed. The child is tracked so it can be killed on exit -- previously
    // it outlived the driver, accumulated as duplicates on mode switch, and
    // kept running after the UI quit.
    let mut tracker_child: Option<std::process::Child> = None;

    if pseye_enabled {
        // First, kill any existing tracker to prevent duplicates on restart
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/IM", "ps_aim_tracker.exe"])
            .output();
        std::thread::sleep(std::time::Duration::from_millis(100));

        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;

        // Create a Job Object with KILL_ON_JOB_CLOSE so the tracker is
        // automatically killed when this process dies for any reason,
        // including end-task. The job handle stays open for the lifetime
        // of this process; when it closes (even on crash/kill), Windows
        // terminates all processes assigned to the job.
        let job_handle = unsafe {
            use windows::Win32::System::JobObjects::{
                CreateJobObjectW, SetInformationJobObject,
                JobObjectExtendedLimitInformation,
                JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
                JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            };
            let job = CreateJobObjectW(None, windows::core::PCWSTR::null()).unwrap_or_default();
            if !job.is_invalid() {
                let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let _ = SetInformationJobObject(
                    job,
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as *const _,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                );
            }
            job
        };

        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
            .unwrap_or_else(|| std::path::PathBuf::from("."));

        let tracker_name = "ps_aim_tracker.exe";
        let tracker_path = [
            exe_dir.join(tracker_name),
            exe_dir.join("..").join("..").join("tracker").join("vendor").join("ps3eyedriver").join(tracker_name),
            exe_dir.join("..").join("..").join("..").join("tracker").join("vendor").join("ps3eyedriver").join(tracker_name),
        ]
        .into_iter()
        .find(|p| p.exists());

        match tracker_path {
            Some(path) => {
                println!("PS Eye tracker: launching {:?}", path);
                match std::process::Command::new(&path)
                    .creation_flags(CREATE_NO_WINDOW)
                    .spawn()
                {
                    Ok(child) => {
                        // Assign tracker to the job so it dies with us
                        unsafe {
                            use windows::Win32::System::JobObjects::AssignProcessToJobObject;
                            use windows::Win32::Foundation::HANDLE;
                            let raw = child.as_raw_handle();
                            if !job_handle.is_invalid() {
                                let _ = AssignProcessToJobObject(job_handle, HANDLE(raw as _));
                            }
                        }
                        tracker_child = Some(child);
                        std::thread::sleep(std::time::Duration::from_millis(500));
                    }
                    Err(e) => eprintln!("warning: failed to launch tracker: {e}"),
                }
            }
            None => {
                eprintln!("warning: --pseye passed but ps_aim_tracker.exe not found.");
                eprintln!("  Release: place it next to ps-aim-windows.exe");
                eprintln!("  Dev: build via tracker\\pseye_psaimtrackerui.bat first");
            }
        }
    }

    // ── Run selected mode (with reconnect on disconnect) ─────────────────────
    let result = loop {
        let r = if has_flag("--lightgun-raw") {
            rawinput_mode::run(pseye_enabled)
        } else if has_flag("--lightgun") {
            lightgun_mode::run(pseye_enabled)
        } else {
            ds4_mode::run()
        };
        match r {
            Ok(()) => break Ok(()),
            Err(e) => {
                let msg = e.to_string();
                eprintln!("Session ended: {msg}");
                // Reconnect on any HID/device error; bail on logic errors
                if msg.contains("disconnected") || msg.contains("HID") 
                    || msg.contains("open") || msg.contains("device") 
                    || msg.contains("hid") || msg.contains("read") {
                    eprintln!("Controller disconnected -- waiting for reconnect...");
                    loop {
                        std::thread::sleep(std::time::Duration::from_millis(1000));
                        if hidapi::HidApi::new()
                            .and_then(|h| h.open(REAL_VID, REAL_PID))
                            .is_ok()
                        {
                            eprintln!("Controller reconnected -- restarting session.");
                            break;
                        }
                    }
                } else {
                    break Err(e);
                }
            }
        }
    };

    // ── Cleanup ──────────────────────────────────────────────────────────────
    // Kill the tracker child we spawned -- prevents it from lingering after
    // the driver exits or the UI stops it.
    if let Some(mut child) = tracker_child {
        let _ = child.kill();
    }

    // Release the mutex
    unsafe { let _ = windows::Win32::System::Threading::ReleaseMutex(mutex_handle); }

    result
}
