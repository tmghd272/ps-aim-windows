#pragma once

#include <cstdint>
#include <thread>
#include <chrono>
#include "hidapi.h"

struct AimState {
    // 0-255, center ~128, same convention as every other mode.
    uint8_t lstick_x = 128, lstick_y = 128, rstick_x = 128, rstick_y = 128;

    bool square = false, cross = false, circle = false, triangle = false;
    bool l1 = false, r1 = false, l2_click = false, r2_click = false;
    bool share = false, options = false, l3 = false, r3 = false;
    bool ps_guide = false, pad_button = false;
    uint8_t l2_analog = 0, r2_analog = 0;

    // Raw firmware units, zeroed over Bluetooth in simple mode (same
    // hardware limitation documented in every other mode's parser).
    int16_t gyro_yaw = 0, gyro_pitch = 0, gyro_roll = 0;
    int16_t accel_x = 0, accel_y = 0, accel_z = 0;
};

// Opens the real Aim Controller and parses its raw HID reports. Handles
// both USB (report ID 0x01) and Bluetooth full-mode (report ID 0x11)
// formats -- same byte offsets already verified extensively on both
// Linux and the Windows DS4/Lightgun modes, just reimplemented here in
// C++ since this driver can't use the Rust parser directly.
class AimReader {
public:
    AimReader() {
        device_ = hid_open(0x054C, 0x0BB2, nullptr);
        if (device_) {
            hid_set_nonblocking(device_, 1);
        }
        if (device_) {
            for (int i = 0; i < 3; i++) {
                int report_id = (i == 0) ? 0x02 : (i == 1) ? 0xa3 : 0x12;
                int size = (i == 0) ? 37 : (i == 1) ? 49 : 16;
                unsigned char buf[64] = { 0 };
                buf[0] = (unsigned char)report_id;
                hid_get_feature_report(device_, buf, size);
            }
            
        }
    }

    ~AimReader() {
        if (device_) {
            hid_close(device_);
        }
    }

    bool IsConnected() const { return device_ != nullptr; }

    // Sets the sphere/lightbar RGB color. First-pass port of the USB
    // output report format proven across every other mode in this
    // project (rumble.rs) -- report ID 0x05, double-write required
    // (real hardware firmware quirk, ignores a single write). This is
    // the USB path only; Bluetooth needs a CRC32 appended to the report
    // and isn't handled here yet -- fine for now since this is being
    // tested over USB, worth adding if/when this needs to work over BT.
    void SetColor(uint8_t r, uint8_t g, uint8_t b) {
        if (!device_) return;

        unsigned char buf[32] = { 0 };
        buf[0] = 0x05;
        buf[1] = 0x07;
        buf[6] = r;
        buf[7] = g;
        buf[8] = b;
        hid_write(device_, buf, sizeof(buf));
        std::this_thread::sleep_for(std::chrono::milliseconds(15));
        hid_write(device_, buf, sizeof(buf)); // USB firmware needs the double-write
    }

    // Reads and parses the most recent available report, if any. Returns
    // false if nothing new was available (caller should keep using the
    // last-known state) or if the device isn't connected.
    bool ReadLatest(AimState& out) {
        if (!device_) return false;

        unsigned char buf[78];
        int n = -1;
        int latest_n = 0;
        unsigned char latest_buf[78];
        // Drain the buffer completely -- if reports arrive faster than we
        // poll, hid_read() only returns the oldest queued report per call,
        // meaning we'd perpetually lag behind stale data and never catch up
        // to the actual current stick position.
        while ((n = hid_read(device_, buf, sizeof(buf))) > 0) {
            latest_n = n;
            memcpy(latest_buf, buf, n);
        }
        if (latest_n <= 0) return false;
        n = latest_n;
        memcpy(buf, latest_buf, n);

        if (buf[0] == 0x01 && n >= 10) {
            // USB simple report.
            out.lstick_x = buf[1];
            out.lstick_y = buf[2];
            out.rstick_x = buf[3];
            out.rstick_y = buf[4];
            uint8_t btn5 = buf[5];
            uint8_t btn6 = buf[6];
            uint8_t btn7 = buf[7];
            out.square = btn5 & 0x10;
            out.cross = btn5 & 0x20;
            out.circle = btn5 & 0x40;
            out.triangle = btn5 & 0x80;
            out.l1 = btn6 & 0x01;
            out.r1 = btn6 & 0x02;
            out.l2_click = btn6 & 0x04;
            out.r2_click = btn6 & 0x08;
            out.share = btn6 & 0x10;
            out.options = btn6 & 0x20;
            out.l3 = btn6 & 0x40;
            out.r3 = btn6 & 0x80;
            out.ps_guide = btn7 & 0x01;
            out.pad_button = btn7 & 0x02;
            out.l2_analog = buf[8];
            out.r2_analog = buf[9];
            if (n >= 24) {
                // Full USB report includes IMU data starting at byte 10.
                out.gyro_yaw = int16_t(buf[12] | (buf[13] << 8));
                out.gyro_pitch = int16_t(buf[14] | (buf[15] << 8));
                out.gyro_roll = int16_t(buf[16] | (buf[17] << 8));
                out.accel_x = int16_t(buf[20] | (buf[21] << 8));
                out.accel_y = int16_t(buf[22] | (buf[23] << 8));
            }
            return true;
        } else if (buf[0] == 0x11 && n >= 40) {
            // Bluetooth full-mode report, BASE = 3.
            out.lstick_x = buf[3];
            out.lstick_y = buf[4];
            out.rstick_x = buf[5];
            out.rstick_y = buf[6];
            uint8_t btn0 = buf[7];
            uint8_t btn1 = buf[8];
            uint8_t btn2 = buf[9] & 0x03;
            out.square = btn0 & 0x10;
            out.cross = btn0 & 0x20;
            out.circle = btn0 & 0x40;
            out.triangle = btn0 & 0x80;
            out.l1 = btn1 & 0x01;
            out.r1 = btn1 & 0x02;
            out.l2_click = btn1 & 0x04;
            out.r2_click = btn1 & 0x08;
            out.share = btn1 & 0x10;
            out.options = btn1 & 0x20;
            out.l3 = btn1 & 0x40;
            out.r3 = btn1 & 0x80;
            out.ps_guide = btn2 & 0x01;
            out.pad_button = btn2 & 0x02;
            out.l2_analog = buf[10];
            out.r2_analog = buf[11];
            // Second IMU sub-sample, more reliably populated (per the
            // earlier Linux reverse-engineering work).
            out.gyro_yaw = int16_t(buf[28] | (buf[29] << 8));
            out.gyro_pitch = int16_t(buf[30] | (buf[31] << 8));
            out.gyro_roll = int16_t(buf[32] | (buf[33] << 8));
            out.accel_x = int16_t(buf[34] | (buf[35] << 8));
            out.accel_y = int16_t(buf[36] | (buf[37] << 8));
            out.accel_z = int16_t(buf[38] | (buf[39] << 8));
            return true;
        }
        return false;
    }

private:
    hid_device* device_ = nullptr;
};

// Converts a raw 0-255 stick axis (center ~128) to OpenVR's normalized
// -1.0..1.0 scalar range. Axis direction is a starting guess -- may need
// a sign flip once actually tested, same story as every other mode's
// first attempt at a new axis mapping.
inline float StickToNormalized(uint8_t raw) {
    float v = (float(raw) - 128.0f) / 128.0f;
    if (v < -1.0f) v = -1.0f;
    if (v > 1.0f) v = 1.0f;
    return v;
}
