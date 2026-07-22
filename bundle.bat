@echo off
cd /d "%~dp0"

REM ── Auto-detect VS x64 tools ─────────────────────────────────────────────
where cl.exe >nul 2>&1
if %ERRORLEVEL% EQU 0 goto :vs_ready

echo cl.exe not in PATH -- searching for Visual Studio...

REM Use vswhere if available (most reliable)
set "VSWHERE=%ProgramFiles(x86)%\Microsoft Visual Studio\Installer\vswhere.exe"
if not exist "%VSWHERE%" set "VSWHERE=%ProgramFiles%\Microsoft Visual Studio\Installer\vswhere.exe"

if exist "%VSWHERE%" (
    for /f "usebackq delims=" %%i in (`"%VSWHERE%" -latest -property installationPath`) do set "VSINSTALL=%%i"
    if defined VSINSTALL (
        set "VCVARS=%VSINSTALL%\VC\Auxiliary\Build\vcvars64.bat"
        goto :call_vcvars
    )
)

REM Fallback: hardcoded common paths
for %%v in (
    "%ProgramFiles%\Microsoft Visual Studio\2022\Community\VC\Auxiliary\Build\vcvars64.bat"
    "%ProgramFiles%\Microsoft Visual Studio\2022\Professional\VC\Auxiliary\Build\vcvars64.bat"
    "%ProgramFiles%\Microsoft Visual Studio\2022\Enterprise\VC\Auxiliary\Build\vcvars64.bat"
    "%ProgramFiles%\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat"
    "%ProgramFiles(x86)%\Microsoft Visual Studio\2019\Community\VC\Auxiliary\Build\vcvars64.bat"
    "%ProgramFiles(x86)%\Microsoft Visual Studio\2019\BuildTools\VC\Auxiliary\Build\vcvars64.bat"
) do if exist %%v ( set "VCVARS=%%~v" & goto :call_vcvars )

echo ERROR: Visual Studio not found. Install VS 2022 with C++ workload.
exit /b 1

:call_vcvars
echo Found: %VCVARS%
call "%VCVARS%" >nul 2>&1
echo VS x64 tools loaded.

:vs_ready
echo.
echo ============================================================
echo  PS Aim Windows -- Release Bundler
echo ============================================================
echo.

if not exist release mkdir release

REM ── 1. Rust driver ──────────────────────────────────────────────────────────
echo [1/4] Building ps-aim-windows (Rust)...
cargo build --release
if %ERRORLEVEL% NEQ 0 ( echo ERROR: Rust build failed. & exit /b 1 )
copy /Y "target\release\ps-aim-windows.exe" "release\ps-aim-windows.exe" >nul
echo       OK: release\ps-aim-windows.exe
echo.

REM ── 2. PS Eye tracker (C++) ─────────────────────────────────────────────────
echo [2/4] Building PS Eye tracker (C++)...
set "TSRC=tracker\vendor\ps3eyedriver"
if not exist "%TSRC%\obj" mkdir "%TSRC%\obj"
set "TINC=/I "%TSRC%" /I "C:\opencv\build\include" /I "%USERPROFILE%\Downloads\libusb-1.0.30\include" /I "%USERPROFILE%\Downloads\hidapi-win\include""
set "TLIBS=/LIBPATH:"C:\opencv\build\x64\vc16\lib" opencv_world500.lib /LIBPATH:"%USERPROFILE%\Downloads\libusb-1.0.30\VS2022\MS64\dll" libusb-1.0.lib /LIBPATH:"%USERPROFILE%\Downloads\hidapi-win\x64" hidapi.lib ws2_32.lib winmm.lib user32.lib"

cl.exe /EHsc /std:c++14 /DHEADLESS /O2 /Fo"%TSRC%\obj\\" ^
  "%TSRC%\ps_aim_tracker_ui.cpp" "%TSRC%\ps3eye.cpp" ^
  /I "%TSRC%" /I "C:\opencv\build\include" ^
  /I "%USERPROFILE%\Downloads\libusb-1.0.30\include" ^
  /I "%USERPROFILE%\Downloads\hidapi-win\include" ^
  /link /SUBSYSTEM:WINDOWS ^
  /LIBPATH:"C:\opencv\build\x64\vc16\lib" opencv_world500.lib ^
  /LIBPATH:"%USERPROFILE%\Downloads\libusb-1.0.30\VS2022\MS64\dll" libusb-1.0.lib ^
  /LIBPATH:"%USERPROFILE%\Downloads\hidapi-win\x64" hidapi.lib ^
  ws2_32.lib winmm.lib user32.lib ^
  /OUT:"%TSRC%\ps_aim_tracker.exe"
if %ERRORLEVEL% NEQ 0 ( echo ERROR: Headless tracker build failed. & exit /b 1 )

cl.exe /EHsc /std:c++14 /O2 /Fo"%TSRC%\obj\\" ^
  "%TSRC%\ps_aim_tracker_ui.cpp" "%TSRC%\ps3eye.cpp" ^
  /I "%TSRC%" /I "C:\opencv\build\include" ^
  /I "%USERPROFILE%\Downloads\libusb-1.0.30\include" ^
  /I "%USERPROFILE%\Downloads\hidapi-win\include" ^
  /link ^
  /LIBPATH:"C:\opencv\build\x64\vc16\lib" opencv_world500.lib ^
  /LIBPATH:"%USERPROFILE%\Downloads\libusb-1.0.30\VS2022\MS64\dll" libusb-1.0.lib ^
  /LIBPATH:"%USERPROFILE%\Downloads\hidapi-win\x64" hidapi.lib ^
  ws2_32.lib winmm.lib user32.lib ^
  /OUT:"%TSRC%\ps_aim_tracker_debug.exe"
if %ERRORLEVEL% NEQ 0 ( echo ERROR: Debug tracker build failed. & exit /b 1 )

copy /Y "%TSRC%\ps_aim_tracker.exe"       "release\ps_aim_tracker.exe" >nul
copy /Y "%TSRC%\ps_aim_tracker_debug.exe" "release\ps_aim_tracker_debug.exe" >nul
copy /Y "%TSRC%\hidapi.dll"     "release\hidapi.dll" >nul 2>&1
copy /Y "%TSRC%\libusb-1.0.dll" "release\libusb-1.0.dll" >nul 2>&1
copy /Y "C:\opencv\build\x64\vc16\bin\opencv_world500.dll" "release\opencv_world500.dll" >nul 2>&1
echo       OK: release\ps_aim_tracker.exe + DLLs
echo.

REM ── 3. Electron UI ──────────────────────────────────────────────────────────
echo [3/4] Building Electron UI...
if exist ui\dist rmdir /s /q ui\dist
cd ui
set TEMP=%TEMP%
set ELECTRON_BUILDER_TMP_DIR=%~dp0ui\dist\tmp
call npm install
if %ERRORLEVEL% NEQ 0 ( echo ERROR: npm install failed. & cd .. & exit /b 1 )
call npm run build
if %ERRORLEVEL% NEQ 0 ( echo ERROR: Electron build failed. & cd .. & exit /b 1 )
cd ..
if exist "ui\dist\win-unpacked" (
    xcopy /E /I /Y "ui\dist\win-unpacked\*" "release\ui\" >nul
    echo       OK: release\ui\ (Electron app folder)
) else (
    for %%f in (ui\dist\*.exe) do (
        copy /Y "%%f" "release\%%~nxf" >nul
        echo       OK: release\%%~nxf
    )
)
echo.

echo ============================================================
echo  Done! release\ contents:
dir /b release\
echo ============================================================

REM ── 4. Shortcut ─────────────────────────────────────────────────────────────
echo [4/4] Creating launcher shortcut...
set "LNK=%~dp0release\PSVR Aim Driver Emulator.lnk"
set "TGT=%~dp0release\ui\PSVR Aim Driver Emulator.exe"
set "WRK=%~dp0release\ui"
powershell -NoProfile -Command "$ws=New-Object -ComObject WScript.Shell;$s=$ws.CreateShortcut($env:LNK);$s.TargetPath=$env:TGT;$s.WorkingDirectory=$env:WRK;$s.Save()"
if exist "%~dp0release\PSVR Aim Driver Emulator.lnk" (
    echo       OK: release\PSVR Aim Driver Emulator.lnk
) else (
    echo       WARNING: Shortcut creation failed
)
