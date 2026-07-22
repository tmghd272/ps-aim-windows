@echo off
REM Builds the PS Eye tracker (headless + debug UI) from tracker\vendor\ps3eyedriver\
REM Run from the tracker\ directory using VS Developer Command Prompt.
REM Output: tracker\vendor\ps3eyedriver\ps_aim_tracker.exe (headless)
REM         tracker\vendor\ps3eyedriver\ps_aim_tracker_debug.exe (with UI)

cd /d "%~dp0"

set SRC=vendor\ps3eyedriver\ps_aim_tracker_ui.cpp
set PSEYESRC=vendor\ps3eyedriver\ps3eye.cpp
set OUT=vendor\ps3eyedriver
set INCLUDES=/I "vendor\ps3eyedriver" /I "C:\opencv\build\include" /I "C:\Users\%USERNAME%\Downloads\libusb-1.0.30\include" /I "C:\Users\%USERNAME%\Downloads\hidapi-win\include"
set LIBS=/LIBPATH:"C:\opencv\build\x64\vc16\lib" opencv_world500.lib /LIBPATH:"C:\Users\%USERNAME%\Downloads\libusb-1.0.30\VS2022\MS64\dll" libusb-1.0.lib /LIBPATH:"C:\Users\%USERNAME%\Downloads\hidapi-win\x64" hidapi.lib ws2_32.lib winmm.lib user32.lib

echo [1/2] Building headless tracker (no window, auto-launched by driver)...
cl.exe /EHsc /std:c++14 /DHEADLESS /O2 %SRC% %PSEYESRC% %INCLUDES% /link /SUBSYSTEM:WINDOWS %LIBS% /OUT:"%OUT%\ps_aim_tracker.exe"
if %ERRORLEVEL% NEQ 0 goto :fail

echo [2/2] Building debug tracker (OpenCV window + sliders, manual tuning)...
cl.exe /EHsc /std:c++14 /O2 %SRC% %PSEYESRC% %INCLUDES% /link %LIBS% /OUT:"%OUT%\ps_aim_tracker_debug.exe"
if %ERRORLEVEL% NEQ 0 goto :fail

echo.
echo Done.
echo   Headless: %OUT%\ps_aim_tracker.exe
echo   Debug UI: %OUT%\ps_aim_tracker_debug.exe
goto :end

:fail
echo Build failed. Check paths above match your system.
exit /b 1
:end
