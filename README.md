<div align="center">

# PSVR Aim Driver Emulator

**The first ever PSVR Aim Controller driver for Windows**  
Gyro · LED Sphere · Rumble · Full Input Support · Bluetooth

[![Ko-fi](https://img.shields.io/badge/Support%20on-Ko--fi-FF5E5B?style=flat-square&logo=kofi&logoColor=white)](https://ko-fi.com/tmghd272)
[![Windows](https://img.shields.io/badge/Platform-Windows-0078D6?style=flat-square&logo=windows)](https://github.com/tmghd272/ps-aim-windows/releases)
[![Downloads](https://img.shields.io/github/downloads/tmghd272/ps-aim-windows/total?style=flat-square&color=brightgreen)](https://github.com/tmghd272/ps-aim-windows/releases)
[![License](https://img.shields.io/badge/License-MIT-green?style=flat-square)](LICENSE)

</div>

---

## Overview

PSVR Aim Driver Emulator brings full PSVR Aim Controller support to PC. It emulates the controller as a **DualShock 4**, **XInput gamepad**, or **mouse + keyboard** — with working gyro, rumble, LED control, and optional PS3 Eye camera tracking for lightgun games. Supports both USB and Bluetooth.

> **Proof of concept** — this demonstrates that PSVR Aim is absolutely viable on PC. PS Eye tracking is experimental and may have limitations due to the age of the hardware.

---

## Features

- **DS4 Mode** — Full DualShock 4 emulation with gyro/accel passthrough. Best for Steam and games with native PlayStation support.
- **Lightgun XInput Mode** — Emulates an Xbox 360 controller. Gyro drives the right stick for aiming. Perfect for TeknoParrot and XInput lightgun games.
- **Lightgun RawInput Mode** — Emulates a mouse + keyboard. R2 = left click, left stick = WASD, face buttons mapped to game keys. Great for MAME, Demul, and other emulators.
- **Gyro** — The most anticipated feature. Full gyro exposure on PC.
- **LED Control** — Full RGB control of the controller's lightbar/sphere per mode.
- **Rumble / Recoil** — Simulated recoil feedback with configurable intensity, duration, and rapid fire modes.
- **Bluetooth Support** — Works over both USB and Bluetooth.
- **PS Eye Tracking** *(Experimental)* — Uses a PS3 Eye camera to track the LED sphere for absolute cursor positioning.

---

## Gameplay Preview

[![Gameplay Preview](https://img.youtube.com/vi/H5igLI-Edpk/mqdefault.jpg)](https://www.youtube.com/watch?v=H5igLI-Edpk)

---

## Prerequisites

- **Windows 10/11** (x64)
- **[Visual C++ Redistributable 2022 x64](https://aka.ms/vs/17/release/vc_redist.x64.exe)** — Required for PS Eye tracker
- **[ViGEmBus](https://github.com/nefarius/ViGEmBus/releases/latest)** — Required for DS4 and XInput emulation
- **[HidHide](https://github.com/nefarius/HidHide/releases/latest)** — Required to hide the native controller from conflicting with emulated inputs
- **PS3 Eye Camera** + **[PS3 Eye Driver via Zadig](https://github.com/psmoveservice/PSMoveService/wiki/PSEye-Software-Setup-%28Windows%29)** + **[Zadig](https://zadig.akeo.ie)** *(optional — PS Eye Tracking only)*
- PSVR Aim Controller connected via **USB** or **Bluetooth**

---

## Installation

1. Download the latest release from the [Releases](../../releases) page
2. Install **ViGEmBus** and **HidHide** from the prerequisites above
3. Extract the release folder anywhere and launch **PSVR Aim Driver Emulator.lnk**

---

## Usage

### Connecting the Controller

**USB** — Simply plug in your PSVR Aim Controller.

**Bluetooth** — Hold **PS Button + Share** simultaneously until the light sphere starts rapidly blinking, then pair from Windows Bluetooth settings.

### Getting Started

1. Launch **PSVR Aim Driver Emulator**
2. Click **Setup HidHide** — hides the native controller so it doesn't conflict with the emulated device. Only needs to be done once.
3. Select your desired mode:

| Mode | Best For | Input Tester |
|------|----------|--------------|
| DS4 | Steam, PC, PS Remote Play, emulators | [Gamepad Tester](https://hardwaretester.com/gamepad) |
| Lightgun XInput | TeknoParrot, MAME, Demul, emulators | [Gamepad Tester](https://hardwaretester.com/gamepad) |
| Lightgun RawInput | TeknoParrot, MAME, Demul, emulators | Windows cursor |

4. Click **Start** and test your inputs
5. Once satisfied, go to **Options** and enable **Auto-start** or **Start on Windows Startup**

---

## Troubleshooting

**Games not detecting input, rumble, gyro, or LED sphere:**
Add the game as a Non-Steam game via Steam and launch it through Steam. Steam's input layer improves compatibility with emulated devices.

**PSVR Aim disconnecting in Bluetooth mode:**
Forget the Bluetooth pairing on your PC and re-pair. Hold PS + Share again to put the controller back into pairing mode.

**HidHide conflicts or native inputs leaking through:**
Click **Reset / Wipe** in the HidHide section of the UI, then click **Setup HidHide** again.

**Tunings behaving unexpectedly:**
Click **Reset Defaults** (top right of the Test & Tuning tab) to restore all sliders to their original values.

---

## Controller Input Map

### DS4 Mode
| PSVR Aim | Output |
|----------|--------|
| Left Stick | Left Stick |
| Right Stick | Right Stick |
| Gyro / Accel | DS4 Gyro / Accel |
| Cross / Circle / Square / Triangle | Cross / Circle / Square / Triangle |
| L1 / R1 / L2 / R2 | L1 / R1 / L2 / R2 |
| Share / Options | Share / Options |
| PS Button | PS Button |
| Touchpad Click | Touchpad Click |
| DPad | DPad |

### Lightgun XInput Mode
| PSVR Aim | Output |
|----------|--------|
| Gyro | Right Stick (aim) |
| Left Stick | Left Stick |
| R2 | Right Trigger |
| Cross / Circle / Square / Triangle | A / B / X / Y |
| L1 / R1 | LB / RB |
| Share / Options | Back / Start |
| DPad | DPad |
| Right Stick Click | Recalibrate gyro center |
| Pad Button (tap) | Cycle recoil mode |
| Pad Button (hold 5s) | Pause all input |

### Lightgun RawInput Mode
| PSVR Aim | Output |
|----------|--------|
| Gyro / PS Eye | Mouse cursor |
| R2 | Left Click |
| Left Stick | WASD |
| Cross | Space |
| Circle | Left Ctrl |
| Square | R |
| Triangle | E |
| L1 | Left Shift |
| R1 | Q |
| Share | Backspace |
| Options | Enter |
| PS Button | Escape |
| DPad | Arrow Keys |

---

## PS Eye Tracking *(Experimental)*

The PS3 Eye camera tracks the colored LED sphere on the controller for absolute cursor positioning in lightgun modes.

> ⚠️ Requires a PS3 Eye camera with the libusb driver installed via Zadig. Due to the age of the hardware, tracking accuracy may vary.

1. Install the PS3 Eye libusb driver via Zadig
2. Enable **PS Eye Tracking** in the UI (only available in Lightgun modes)
3. To calibrate: hold **Right Stick** for ~1 second, then point at each screen corner and pull the trigger when prompted

---

## Building from Source

### Requirements
- [Rust](https://rustup.rs/) (stable)
- [Node.js](https://nodejs.org/) + npm
- Visual Studio 2022 with C++ workload *(PS Eye tracker only)*
- OpenCV, libusb, hidapi *(PS Eye tracker only)*

### Build
Open **x64 Native Tools Command Prompt for VS 2022**, navigate to the project root and run:
```bat
bundle.bat
```
Output will be in `release\`.

---

## Support

If you find this project useful, please support me via Ko-fi!:

[![Ko-fi](https://ko-fi.com/img/githubbutton_sm.svg)](https://ko-fi.com/tmghd272)

---

## License

MIT — see [LICENSE](LICENSE)

---

<div align="center">
Made with ❤️ by <a href="https://github.com/tmghd272">TMGHD272</a>
</div>
