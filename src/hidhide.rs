//! Automates HidHide setup (whitelisting our exe, hiding the real Aim
//! Controller) instead of requiring manual configuration through
//! HidHideClient.exe every time.
//!
//! Doesn't need Administrator privileges. What *does* cause an
//! "Access is denied" error from every HidHideCLI command: having
//! HidHideClient.exe (the GUI) open at the same time -- it holds a
//! handle that conflicts with the CLI. Close it first. This is meant to
//! be run once, as a separate setup step, not as part of the normal
//! relay program's every-launch path -- HidHide's whitelist/blacklist
//! configuration is driver-level and persists, so it doesn't need to be
//! redone on every run, only when this exe's path changes or a device
//! needs (re-)hiding.

use std::process::Command;

const REAL_VID: &str = "054c";
const REAL_PID: &str = "0bb2";

// Standard install location. HidHide doesn't offer much choice in
// install path in practice, so this is a reasonable default -- if it's
// ever installed somewhere else, this would need adjusting.
const HIDHIDE_CLI: &str =
    r"C:\Program Files\Nefarius Software Solutions\HidHide\x64\HidHideCLI.exe";

pub fn run_setup() -> Result<(), Box<dyn std::error::Error>> {
    check_cli_exists()?;

    println!("Whitelisting this program with HidHide...");
    let exe_path = std::env::current_exe()?;
    let exe_path_str = exe_path.to_string_lossy();
    run_cli(&["--app-reg", &exe_path_str])?;
    println!("  Whitelisted: {exe_path_str}");

    println!("Finding the real Aim Controller's device instance(s)...");
    let listing = run_cli(&["--dev-gaming"])?;
    let paths = find_device_instance_paths(&listing)?;
    if paths.is_empty() {
        return Err(
            "No Aim Controller found in HidHide's device listing. Make sure it's \
             connected (either USB or Bluetooth) before running setup."
                .into(),
        );
    }

    for path in &paths {
        println!("Hiding device: {path}");
        run_cli(&["--dev-hide", path])?;
    }

    println!("Enabling HidHide cloaking...");
    run_cli(&["--cloak-on"])?;

    println!(
        "\nSetup complete. {} device instance(s) hidden, this program whitelisted.\n\
         If you use the controller over both USB and Bluetooth, re-run this setup \
         with whichever transport wasn't connected this time, since only \
         currently-present devices show up in the listing.",
        paths.len()
    );
    Ok(())
}

/// Undoes everything run_setup() did: un-hides any currently-listed Aim
/// Controller device instances, un-whitelists this exe, and turns
/// cloaking off entirely. Doesn't touch anything else HidHide might be
/// managing for other devices/apps -- cloak-off is global (simplest,
/// safest full reset if something's gone wrong), but the un-hide/
/// un-whitelist calls are scoped to just this device/exe.
pub fn run_wipe() -> Result<(), Box<dyn std::error::Error>> {
    check_cli_exists()?;

    println!("Disabling HidHide cloaking...");
    run_cli(&["--cloak-off"])?;

    println!("Un-hiding any currently-listed Aim Controller device instance(s)...");
    let listing = run_cli(&["--dev-gaming"])?;
    let paths = find_device_instance_paths(&listing)?;
    for path in &paths {
        println!("Un-hiding device: {path}");
        run_cli(&["--dev-unhide", path])?;
    }
    if paths.is_empty() {
        println!("  (none currently listed -- nothing to un-hide right now, but the \
                   blacklist entry may still exist if the device isn't connected. If \
                   you reconnect it later and it's still hidden, run this again.)");
    }

    println!("Un-whitelisting this program...");
    let exe_path = std::env::current_exe()?;
    let exe_path_str = exe_path.to_string_lossy();
    // --app-unreg is the reasonable inverse of --app-reg; if HidHideCLI's
    // actual flag name differs, this specific call may fail even though
    // everything else in this function still worked -- not fatal to the
    // overall wipe.
    if let Err(e) = run_cli(&["--app-unreg", &exe_path_str]) {
        eprintln!("  warning: couldn't un-whitelist automatically ({e}). \
                    You can remove it manually via HidHideClient.exe's Applications tab.");
    }

    println!("\nWipe complete. HidHide cloaking is now off entirely.");
    Ok(())
}

fn check_cli_exists() -> Result<(), Box<dyn std::error::Error>> {
    if !std::path::Path::new(HIDHIDE_CLI).exists() {
        return Err(format!(
            "HidHideCLI.exe not found at the expected path:\n  {HIDHIDE_CLI}\n\
             Install HidHide first: https://github.com/nefarius/HidHide/releases"
        )
        .into());
    }
    Ok(())
}

fn run_cli(args: &[&str]) -> Result<String, Box<dyn std::error::Error>> {
    let output = Command::new(HIDHIDE_CLI).args(args).output()?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if !output.status.success() || stderr.contains("Error code") || stdout.contains("Error code")
    {
        return Err(format!(
            "HidHideCLI {args:?} failed:\n{stdout}{stderr}\n\
             (Make sure HidHideClient.exe isn't open at the same time -- \
             it holds a conflicting handle that blocks the CLI.)"
        )
        .into());
    }
    Ok(stdout)
}

/// Parses HidHideCLI's `--dev-gaming` JSON output, looking for any device
/// entries matching the real Aim Controller's VID/PID (case-insensitive,
/// since HidHide's own output has been observed using inconsistent case
/// for the same value across fields). Returns every matching
/// deviceInstancePath found -- there can be more than one if both USB
/// and Bluetooth connections are currently present.
fn find_device_instance_paths(json_text: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let parsed: serde_json::Value = serde_json::from_str(json_text)?;
    let mut paths = Vec::new();

    let Some(entries) = parsed.as_array() else {
        return Ok(paths);
    };
    for entry in entries {
        let Some(devices) = entry.get("devices").and_then(|d| d.as_array()) else {
            continue;
        };
        for device in devices {
            let Some(instance_path) = device.get("deviceInstancePath").and_then(|p| p.as_str())
            else {
                continue;
            };
            let lower = instance_path.to_lowercase();
            // Format-agnostic on purpose: USB instance paths look like
            // "USB\VID_054C&PID_0BB2\..." while Bluetooth ones look like
            // "HID\{...}_VID&0002054c_PID&0bb2\..." -- rather than match
            // one exact prefix style and risk silently missing the
            // other, just check both VID and PID substrings appear
            // anywhere in the path (both need to co-occur, which is
            // specific enough to avoid false positives).
            let matches = lower.contains(REAL_VID) && lower.contains(REAL_PID);
            if matches {
                paths.push(instance_path.to_string());
            }
        }
    }
    Ok(paths)
}
