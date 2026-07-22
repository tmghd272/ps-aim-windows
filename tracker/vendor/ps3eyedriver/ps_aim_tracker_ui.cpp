// PS Eye tracker with an interactive calibration UI -- sliders for live
// threshold tuning, click-on-sphere to sample its actual color, and
// keyboard shortcuts to change the physical sphere's color. Uses
// OpenCV's own GUI (imshow/createTrackbar/setMouseCallback) instead of
// raw Win32, since OpenCV's already a dependency and this is much
// simpler than building custom Win32 controls for the same thing.
#define NOMINMAX
#include <cstdio>
#include <chrono>
#include <vector>
#include <winsock2.h>
#include <ws2tcpip.h>
#include <timeapi.h>
#pragma comment(lib, "ws2_32.lib")
#pragma comment(lib, "winmm.lib")
#include <opencv2/opencv.hpp>
#include <opencv2/imgproc.hpp>
#include <opencv2/geometry.hpp>
#include "aim_reader.h"
#include "ps3eye.h"

#ifdef HEADLESS
#pragma comment(linker, "/SUBSYSTEM:WINDOWS")
#pragma comment(lib, "user32.lib")
#include <windows.h>
#endif

using namespace ps3eye;

static const uint32_t WIDTH = 320;
static const uint32_t HEIGHT = 240;

// Live-adjustable via sliders -- no recompile needed to tune.
struct CalibrationParams {
    int hue_range = 12;
    int min_saturation = 70;
    int min_value = 40;
    int roi_radius = 100;
} g_params;

int g_target_hue = 136; // default: violet/magenta, matches our usual sphere color
int g_strictness = 50;  // 0-100 -- scales the adaptive threshold; 0 = always permissive (old build's feel), 100 = full daytime protection
#ifndef HEADLESS
int g_camera_gain = 20; // live-adjustable to balance noise vs low-light sensitivity
int g_camera_exposure = 60; // live-adjustable -- lower suppresses ambient light more, higher gives better base image quality
bool g_calibrate_requested = false;
cv::Point g_calibrate_point;

void on_mouse(int event, int x, int y, int, void*)
{
    if (event == cv::EVENT_LBUTTONDOWN)
    {
        g_calibrate_point = cv::Point(x, y);
        g_calibrate_requested = true;
    }
}
#else
static const int g_camera_gain = 20;
static const int g_camera_exposure = 60;
#endif

bool matches_target_hue_strict(uint8_t h, uint8_t s, uint8_t v, int target_hue)
{
    int hue_range = g_params.hue_range;
    if (s < g_params.min_saturation || v < g_params.min_value) return false;
    int d = abs((int)h - target_hue);
    if (d > 90) d = 180 - d; // OpenCV hue wraps at 180, not 360
    return d < hue_range;
}

bool matches_target_hue_relaxed(uint8_t h, uint8_t s, uint8_t v, int target_hue)
{
    if (v > 240) return true;
    return matches_target_hue_strict(h, s, v, target_hue);
}

struct BlobResult {
    bool found = false;
    float center_x = 0, center_y = 0;
};

// Broadcasts the current tracked position over a local TCP socket as
// plain text lines ("found,x,y\n") -- deliberately simple format so
// the Rust Lightgun process can parse it without needing a JSON
// library. Non-blocking throughout so a missing/slow client never
// stalls the main capture/display loop.
class PositionBroadcaster {
public:
    bool Start(unsigned short port)
    {
        WSADATA wsa;
        if (WSAStartup(MAKEWORD(2, 2), &wsa) != 0) return false;

        listen_socket_ = socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
        if (listen_socket_ == INVALID_SOCKET) return false;

        sockaddr_in addr = { 0 };
        addr.sin_family = AF_INET;
        addr.sin_addr.s_addr = inet_addr("127.0.0.1");
        addr.sin_port = htons(port);

        if (bind(listen_socket_, (sockaddr*)&addr, sizeof(addr)) == SOCKET_ERROR) return false;
        if (listen(listen_socket_, 1) == SOCKET_ERROR) return false;

        u_long mode = 1; // non-blocking
        ioctlsocket(listen_socket_, FIONBIO, &mode);

        printf("Position broadcast listening on 127.0.0.1:%d\n", port);
        return true;
    }

    void SendPosition(bool found, float x, float y)
    {
        // Accept a new client if one's waiting and we don't already have one.
        if (client_socket_ == INVALID_SOCKET)
        {
            SOCKET s = accept(listen_socket_, nullptr, nullptr);
            if (s != INVALID_SOCKET)
            {
                u_long mode = 1;
                ioctlsocket(s, FIONBIO, &mode);
                client_socket_ = s;
                printf("Lightgun client connected.\n");
            }
        }

        if (client_socket_ == INVALID_SOCKET) return;

        char buf[64];
        int len = snprintf(buf, sizeof(buf), "%d,%.1f,%.1f\n", found ? 1 : 0, x, y);
        int sent = send(client_socket_, buf, len, 0);
        if (sent == SOCKET_ERROR)
        {
            int err = WSAGetLastError();
            if (err != WSAEWOULDBLOCK)
            {
                // Client disconnected or errored -- drop it, ready to
                // accept a new connection next time one shows up.
                closesocket(client_socket_);
                client_socket_ = INVALID_SOCKET;
                printf("Lightgun client disconnected.\n");
            }
        }
    }

    ~PositionBroadcaster()
    {
        if (client_socket_ != INVALID_SOCKET) closesocket(client_socket_);
        if (listen_socket_ != INVALID_SOCKET) closesocket(listen_socket_);
        WSACleanup();
    }

private:
    SOCKET listen_socket_ = INVALID_SOCKET;
    SOCKET client_socket_ = INVALID_SOCKET;
};

#ifdef HEADLESS
int WINAPI WinMain(HINSTANCE, HINSTANCE, LPSTR, int)
#else
int main()
#endif
{
    // Windows' default timer resolution is ~15.6ms, which was silently
    // making cv::waitKey(1) actually take 15-17ms instead of the
    // requested 1ms -- this was the entire framerate bottleneck,
    // confirmed via direct measurement, not a guess.
    timeBeginPeriod(1);

    PositionBroadcaster broadcaster;
    broadcaster.Start(9876);

    AimReader reader;
    if (!reader.IsConnected())
    {
        printf("Could not open Aim Controller -- continuing without LED color control.\n");
    }

    const std::vector<PS3EYECam::PS3EYERef>& devices = PS3EYECam::getDevices();
    if (devices.empty())
    {
        printf("No PS3 Eye camera found.\n");
        return 1;
    }
    PS3EYECam::PS3EYERef cam = devices[0];
    if (!cam->init(WIDTH, HEIGHT, 60, PS3EYECam::EOutputFormat::RGB))
    {
        printf("Camera init() failed\n");
        return 1;
    }
    printf("Camera confirmed frame rate: %d fps\n", cam->getFrameRate());
    cam->setAutogain(false);
    cam->setGain(20);
    cam->setExposure(60);
    cam->setAutoWhiteBalance(true);
    cam->start();

    // Set the sphere to a default color to start.
    const uint8_t sphere_r = 80, sphere_g = 0, sphere_b = 150;
    if (reader.IsConnected())
        reader.SetColor(sphere_r, sphere_g, sphere_b);

#ifndef HEADLESS
    cv::namedWindow("PS Aim Tracker", cv::WINDOW_AUTOSIZE);
    cv::createTrackbar("Hue Range", "PS Aim Tracker", &g_params.hue_range, 60);
    cv::createTrackbar("Min Saturation", "PS Aim Tracker", &g_params.min_saturation, 255);
    cv::createTrackbar("Min Value", "PS Aim Tracker", &g_params.min_value, 255);
    cv::createTrackbar("Camera Gain", "PS Aim Tracker", &g_camera_gain, 63);
    cv::createTrackbar("Camera Exposure", "PS Aim Tracker", &g_camera_exposure, 255);
    cv::setMouseCallback("PS Aim Tracker", on_mouse);
    printf("Click on the sphere in the video to calibrate its color.\n");
    printf("Keys: ESC=quit\n");
#endif

    std::vector<uint8_t> frame(WIDTH * HEIGHT * 3);
    float smoothed_x = 0, smoothed_y = 0;
    bool have_smoothed = false;
    int miss_count = 0;
    const int MAX_MISSES_BEFORE_LOST = 45;
    const float SMOOTHING = 0.12f; // retuned after the PSMoveServiceEx-based detection rework -- more stable underlying detection means less smoothing is needed to stay steady

    auto fps_last_report = std::chrono::steady_clock::now();
    int fps_frame_count = 0;

    int last_sent_gain = -1;
    int last_sent_exposure = -1;
    while (true)
    {
      try
      {
        auto t_start = std::chrono::steady_clock::now();
        // Same fix as color, applied here too -- setGain/setExposure
        // also do real USB writes to the camera's sensor registers.
        // Calling them unconditionally every frame was paying that
        // cost 60 times a second for zero benefit on the vast majority
        // of frames where the slider hadn't actually moved.
#ifndef HEADLESS
        if (g_camera_gain != last_sent_gain)
        {
            cam->setGain((uint8_t)g_camera_gain);
            last_sent_gain = g_camera_gain;
        }
        if (g_camera_exposure != last_sent_exposure)
        {
            cam->setExposure((uint8_t)g_camera_exposure);
            last_sent_exposure = g_camera_exposure;
        }
#endif
        auto t0 = std::chrono::steady_clock::now();
        cam->getFrame(frame.data());
        auto t1 = std::chrono::steady_clock::now();

        fps_frame_count++;
        auto now_time = std::chrono::steady_clock::now();
        auto elapsed = std::chrono::duration<double>(now_time - fps_last_report).count();
        if (elapsed >= 1.0)
        {
            printf("[FPS] %.1f frames/sec\n", fps_frame_count / elapsed);
            fps_frame_count = 0;
            fps_last_report = now_time;
        }

        cv::Mat rgb_mat(HEIGHT, WIDTH, CV_8UC3, frame.data());
        cv::Mat bgr_mat;
        cv::cvtColor(rgb_mat, bgr_mat, cv::COLOR_RGB2BGR);
        cv::Mat hsv;
        cv::cvtColor(bgr_mat, hsv, cv::COLOR_BGR2HSV);

        // Handle a pending click-to-calibrate request: sample the
        // average HSV of a small region around the click and use it as
        // the new target color -- lets you point at whatever the
        // sphere actually looks like right now instead of guessing at
        // RGB numbers.
#ifndef HEADLESS
        if (g_calibrate_requested)
        {
            int click_x = g_calibrate_point.x / 2; // display is shown at 2x scale
            int click_y = g_calibrate_point.y / 2;
            // Clamp the click itself first -- a click near the edge
            // (or on/near the trackbar area, which is technically part
            // of the same window) could otherwise produce an
            // out-of-bounds rectangle and crash OpenCV.
            click_x = std::max(0, std::min((int)WIDTH - 1, click_x));
            click_y = std::max(0, std::min((int)HEIGHT - 1, click_y));
            int x0 = std::max(0, click_x - 5);
            int y0 = std::max(0, click_y - 5);
            int x1 = std::min((int)WIDTH, click_x + 5);
            int y1 = std::min((int)HEIGHT, click_y + 5);
            if (x1 > x0 && y1 > y0)
            {
                cv::Rect sample_rect(x0, y0, x1 - x0, y1 - y0);
                cv::Scalar avg = cv::mean(hsv(sample_rect));
                g_target_hue = (int)avg[0];
                printf("Calibrated target hue to %d (from click at %d,%d)\n", g_target_hue, click_x, click_y);
            }
            g_calibrate_requested = false;
        }
#endif

        cv::Mat mask;
        cv::inRange(hsv,
            cv::Scalar(std::max(0, g_target_hue - g_params.hue_range), g_params.min_saturation, g_params.min_value),
            cv::Scalar(std::min(179, g_target_hue + g_params.hue_range), 255, 255),
            mask);

        cv::Mat bright_mask;
        cv::inRange(hsv, cv::Scalar(0, 0, 235), cv::Scalar(179, 70, 255), bright_mask);
        cv::Mat mask_dilated;
        cv::dilate(mask, mask_dilated, cv::getStructuringElement(cv::MORPH_ELLIPSE, cv::Size(17, 17)));
        cv::Mat bright_near_match = bright_mask & mask_dilated;

        cv::Mat kernel = cv::getStructuringElement(cv::MORPH_ELLIPSE, cv::Size(3, 3));
        cv::Mat kernel5 = cv::getStructuringElement(cv::MORPH_ELLIPSE, cv::Size(5, 5));
        cv::morphologyEx(mask, mask, cv::MORPH_OPEN, kernel);

        cv::Mat combined = mask | bright_near_match;
        cv::morphologyEx(combined, combined, cv::MORPH_OPEN, kernel5);
        cv::dilate(combined, combined, kernel, cv::Point(-1, -1), 1);
        cv::erode(combined, combined, kernel, cv::Point(-1, -1), 1);

        // Real, proven defaults from PSMoveServiceEx's TrackerManagerConfig
        // (a mature, shipped project), scaled by 0.5 for our 320x240
        // capture vs their 640x480 default. Their area thresholds in
        // particular are dramatically lower than what we'd been using
        // (6 vs our 15-40+), and they don't use a color-purity ratio at
        // all -- just a basic contour-point-count check plus tight HSV
        // thresholds up front and a hard position-deviation cutoff.
        const float MIN_VALID_AREA = 40.0f;              // raised from their 6 -- their number assumed tighter upstream HSV filtering than ours, letting tiny noise specks qualify
        const float OCCLUDED_ON_LOSS_AREA = 25.0f;        // raised from their 4 -- same reasoning, still lower than the other two so a brief miss survives
        const float OCCLUDED_REGAIN_AREA = 60.0f;         // raised from their 32 -- higher bar to fully reacquire after fully lost
        const int MIN_POINTS_IN_CONTOUR = 4;              // their min_points_in_contour
        const float MAX_POSITION_DEVIATION = 90.0f; // raised from 45 -- was rejecting the real sphere during genuinely fast movement, causing a freeze-then-catch-up lag pattern
        const int ROI_SIZE = 70;        // tight ROI while confidently tracking -- their scaled 16px was far too small for real movement speed and let the tracker get stuck on static edge artifacts
        const int ROI_SEARCH_SIZE = 140; // looser ROI while re-acquiring

        double ambient_v = cv::mean(hsv, cv::noArray())[2];

        cv::Rect roi;
        float min_area_thresh;
        bool have_prior_pos = have_smoothed && miss_count < MAX_MISSES_BEFORE_LOST;
        if (have_prior_pos)
        {
            // Tight ROI while confidently tracking, relaxed area floor
            // since we already know roughly where the sphere is.
            int radius = ROI_SIZE;
            int x0 = std::max(0, (int)smoothed_x - radius);
            int y0 = std::max(0, (int)smoothed_y - radius);
            int x1 = std::min((int)WIDTH, (int)smoothed_x + radius);
            int y1 = std::min((int)HEIGHT, (int)smoothed_y + radius);
            roi = cv::Rect(x0, y0, x1 - x0, y1 - y0);
            min_area_thresh = OCCLUDED_ON_LOSS_AREA;
        }
        else if (have_smoothed)
        {
            // Recently lost but not fully given up yet -- wider search
            // area, and a higher area bar to actually confirm we've
            // regained the real sphere rather than snapping onto noise.
            int radius = ROI_SEARCH_SIZE;
            int x0 = std::max(0, (int)smoothed_x - radius);
            int y0 = std::max(0, (int)smoothed_y - radius);
            int x1 = std::min((int)WIDTH, (int)smoothed_x + radius);
            int y1 = std::min((int)HEIGHT, (int)smoothed_y + radius);
            roi = cv::Rect(x0, y0, x1 - x0, y1 - y0);
            min_area_thresh = OCCLUDED_REGAIN_AREA;
        }
        else
        {
            // Fully cold -- full frame, standard validity bar.
            roi = cv::Rect(0, 0, WIDTH, HEIGHT);
            min_area_thresh = MIN_VALID_AREA;
        }

        // Final safety net regardless of which branch ran -- clamps to
        // guaranteed-valid bounds so a malformed rectangle can never
        // reach cv::Mat's ROI constructor and crash, even if some other
        // edge case slips past the smoothed-position clamp above.
        roi.x = std::max(0, std::min((int)WIDTH - 1, roi.x));
        roi.y = std::max(0, std::min((int)HEIGHT - 1, roi.y));
        roi.width = std::max(1, std::min((int)WIDTH - roi.x, roi.width));
        roi.height = std::max(1, std::min((int)HEIGHT - roi.y, roi.height));

        cv::Mat combined_roi = combined(roi);
        cv::Mat mask_roi = mask(roi);

        std::vector<std::vector<cv::Point>> contours;
        cv::findContours(combined_roi, contours, cv::RETR_EXTERNAL, cv::CHAIN_APPROX_SIMPLE);

        bool found = false;
        float center_x = 0, center_y = 0;
        double best_score = 0;
        double best_area = 0;
        int best_idx = -1;
        float prior_x = smoothed_x - roi.x;
        float prior_y = smoothed_y - roi.y;
        for (size_t i = 0; i < contours.size(); i++)
        {
            if ((int)contours[i].size() < MIN_POINTS_IN_CONTOUR) continue;

            double area = cv::contourArea(contours[i]);
            if (area < min_area_thresh) continue;

            double score = area;

            // Roundness check: the real sphere is circular, while a
            // reflection glowing on a nearby flat surface tends to be
            // a smeared, elongated patch rather than a clean circle.
            if (contours[i].size() >= 5)
            {
                cv::RotatedRect ellipse = cv::fitEllipse(contours[i]);
                float w = ellipse.size.width;
                float h = ellipse.size.height;
                if (w > 0 && h > 0)
                {
                    float aspect = std::max(w, h) / std::min(w, h);
                    score /= (1.0 + (aspect - 1.0) * 3.0);
                }
            }

            if (have_prior_pos)
            {
                cv::Moments m = cv::moments(contours[i]);
                if (m.m00 > 0)
                {
                    float cx = (float)(m.m10 / m.m00);
                    float cy = (float)(m.m01 / m.m00);
                    float dx = cx - prior_x;
                    float dy = cy - prior_y;
                    float dist = sqrtf(dx * dx + dy * dy);
                    // Hard cutoff, matching their real approach, rather
                    // than our previous soft distance penalty -- a
                    // candidate that jumped further than a real sphere
                    // plausibly could between frames just isn't a
                    // candidate at all.
                    if (dist > MAX_POSITION_DEVIATION) continue;
                }
            }

            if (score <= best_score) continue;
            best_score = score;
            best_area = area;
            best_idx = (int)i;
        }

        if (best_idx >= 0 && best_area >= min_area_thresh)
        {
            if (contours[best_idx].size() >= 5)
            {
                cv::RotatedRect ellipse = cv::fitEllipse(contours[best_idx]);
                center_x = ellipse.center.x + roi.x;
                center_y = ellipse.center.y + roi.y;
                found = true;
            }
            else
            {
                cv::Moments m = cv::moments(contours[best_idx]);
                if (m.m00 > 0)
                {
                    center_x = (float)(m.m10 / m.m00) + roi.x;
                    center_y = (float)(m.m01 / m.m00) + roi.y;
                    found = true;
                }
            }
        }

        cv::Mat display;
        bgr_mat.copyTo(display);
        display.setTo(cv::Scalar(0, 255, 0), combined);
        cv::rectangle(display, roi, cv::Scalar(0, 0, 255), 1);

        if (found)
        {
            miss_count = 0;

            if (!have_smoothed) { smoothed_x = center_x; smoothed_y = center_y; have_smoothed = true; }
            else
            {
                float dx = center_x - smoothed_x;
                float dy = center_y - smoothed_y;
                float dist = sqrtf(dx * dx + dy * dy);
                float dynamic_smoothing = SMOOTHING;
                if (dist > 40.0f) dynamic_smoothing = 0.9f;
                else if (dist > 15.0f) dynamic_smoothing = 0.5f;
                smoothed_x += dx * dynamic_smoothing;
                smoothed_y += dy * dynamic_smoothing;
            }
            // Clamp immediately after every update -- an ellipse fit
            // near the frame edge can extrapolate its center slightly
            // outside actual pixel bounds, and letting that propagate
            // into next frame's ROI computation was the root cause of
            // a real crash (negative-width cv::Rect).
            smoothed_x = std::max(0.0f, std::min((float)WIDTH - 1, smoothed_x));
            smoothed_y = std::max(0.0f, std::min((float)HEIGHT - 1, smoothed_y));

            static int diag_counter = 0;
            if (++diag_counter % 30 == 0)
            {
                printf("mode=%s contours=%zu best_area=%.1f min_area_needed=%.1f miss_count=%d ambient_v=%.0f -> found=%d pos=(%.0f,%.0f)\n",
                    have_prior_pos ? "ROI" : (have_smoothed ? "REGAIN" : "FULL"),
                    contours.size(), best_area, min_area_thresh, miss_count, ambient_v,
                    found, smoothed_x, smoothed_y);
            }

            if (miss_count <= MAX_MISSES_BEFORE_LOST)
            {
                cv::drawMarker(display, cv::Point((int)smoothed_x, (int)smoothed_y),
                    cv::Scalar(0, 255, 255), cv::MARKER_CROSS, 20, 2);
            }
            else
            {
                have_smoothed = false;
            }
        }
        else
        {
            miss_count++;

            static int diag_counter2 = 0;
            if (++diag_counter2 % 30 == 0)
            {
                printf("mode=%s contours=%zu best_area=%.1f miss_count=%d ambient_v=%.0f -> found=0 (not found this frame)\n",
                    (have_smoothed && miss_count < MAX_MISSES_BEFORE_LOST) ? "ROI" : "FULL",
                    contours.size(), best_area, miss_count, ambient_v);
            }

            if (miss_count <= MAX_MISSES_BEFORE_LOST && have_smoothed)
            {
                cv::drawMarker(display, cv::Point((int)smoothed_x, (int)smoothed_y),
                    cv::Scalar(0, 255, 255), cv::MARKER_CROSS, 20, 2);
            }
            else
            {
                have_smoothed = false;
            }
        }

        // Broadcast whatever's actually being shown as the crosshair
        // (the smoothed position while within the hysteresis window),
        // not the raw noisy per-frame detection.
        broadcaster.SendPosition(have_smoothed, smoothed_x, smoothed_y);

        // Show the current calibration target as a small swatch, so
        // it's obvious what color the tracker thinks it's looking for.
        cv::Mat swatch_hsv(1, 1, CV_8UC3, cv::Scalar(g_target_hue, 255, 255));
        cv::Mat swatch_bgr;
        cv::cvtColor(swatch_hsv, swatch_bgr, cv::COLOR_HSV2BGR);
        cv::rectangle(display, cv::Rect(5, 5, 20, 20), swatch_bgr.at<cv::Vec3b>(0, 0), cv::FILLED);
        cv::rectangle(display, cv::Rect(5, 5, 20, 20), cv::Scalar(255, 255, 255), 1);

        char info_text[128];
        snprintf(info_text, sizeof(info_text), "ambient_v=%.0f min_area=%.1f", ambient_v, min_area_thresh);
        cv::putText(display, info_text, cv::Point(30, 15), cv::FONT_HERSHEY_SIMPLEX, 0.4, cv::Scalar(255, 255, 255), 1);

#ifndef HEADLESS
        cv::Mat display_2x;
        cv::resize(display, display_2x, cv::Size(WIDTH * 2, HEIGHT * 2), 0, 0, cv::INTER_NEAREST);
        auto t2 = std::chrono::steady_clock::now();
        cv::imshow("PS Aim Tracker", display_2x);
        auto t3 = std::chrono::steady_clock::now();

        int key = cv::waitKey(1);
        auto t4 = std::chrono::steady_clock::now();

        static int timing_counter = 0;
        if (++timing_counter % 30 == 0)
        {
            double setcolor_ms = std::chrono::duration<double, std::milli>(t0 - t_start).count();
            double capture_ms = std::chrono::duration<double, std::milli>(t1 - t0).count();
            double process_ms = std::chrono::duration<double, std::milli>(t2 - t1).count();
            double imshow_ms = std::chrono::duration<double, std::milli>(t3 - t2).count();
            double waitkey_ms = std::chrono::duration<double, std::milli>(t4 - t3).count();
            printf("[TIMING] setcolor=%.1fms capture=%.1fms process=%.1fms imshow=%.1fms waitkey=%.1fms\n",
                setcolor_ms, capture_ms, process_ms, imshow_ms, waitkey_ms);
        }
        if (key == 27) break; // ESC
        // Also exit if the user closed the window via the X button --
        // waitKey only catches keyboard, not window close events.
        if (cv::getWindowProperty("PS Aim Tracker", cv::WND_PROP_VISIBLE) < 1) break;
#else
        Sleep(1);
#endif
      }
      catch (const cv::Exception& e)
      {
          printf("[CAUGHT OpenCV EXCEPTION] %s\n", e.what());
      }
      catch (const std::exception& e)
      {
          printf("[CAUGHT EXCEPTION] %s\n", e.what());
      }
    }

    cam->stop();
    return 0;
}
