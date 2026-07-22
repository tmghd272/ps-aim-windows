const { app, BrowserWindow, ipcMain, Tray, Menu, nativeImage, dialog } = require('electron');
const path = require('path');
const fs = require('fs');
const os = require('os');
const { spawn, exec } = require('child_process');

const configDir = path.join(os.homedir(), 'AppData', 'Roaming', 'ps-aim-windows');
const configPath = path.join(configDir, 'config.txt');
const uiConfigPath = path.join(configDir, 'ui-config.json');

let mainWindow = null;
let tray = null;
let driverProcess = null;
let logLines = [];
let isQuitting = false;

// ── Single instance ───────────────────────────────────────────────────────────
const gotLock = app.requestSingleInstanceLock();
if (!gotLock) { app.quit(); }
else {
  app.on('second-instance', () => {
    if (mainWindow) {
      if (mainWindow.isMinimized()) mainWindow.restore();
      mainWindow.show();
      mainWindow.focus();
    }
  });
}

// ── Config ────────────────────────────────────────────────────────────────────
function readConfig() {
  const defaults = {
    recoil_mode: 'single_kick',
    lightgun_sensitivity: 75.0,
    lightgun_accel_threshold: 2500.0,
    lightgun_accel_gain: 0.0006,
    lightgun_recoil_intensity: 255,
    lightgun_recoil_duration_ms: 140,
    lightgun_rapidfire_interval_ms: 200,
    lightgun_led_r: 0, lightgun_led_g: 128, lightgun_led_b: 0,
    lightgun_raw_led_r: 255, lightgun_raw_led_g: 100, lightgun_raw_led_b: 0,
    lightgun_translation_gain: 12.0,
    lightgun_translation_decay: 0.85,
  };
  try {
    for (const line of fs.readFileSync(configPath, 'utf8').split('\n')) {
      const t = line.trim();
      if (!t || t.startsWith('#')) continue;
      const i = t.indexOf('=');
      if (i < 0) continue;
      const k = t.slice(0, i).trim(), v = t.slice(i + 1).trim();
      if (k in defaults) defaults[k] = isNaN(Number(v)) ? v : Number(v);
    }
  } catch (_) {}
  return defaults;
}

function writeConfig(cfg) {
  fs.mkdirSync(configDir, { recursive: true });
  fs.writeFileSync(configPath, [
    '# ps-aim-windows persistent settings',
    `recoil_mode=${cfg.recoil_mode}`,
    `lightgun_sensitivity=${cfg.lightgun_sensitivity}`,
    `lightgun_accel_threshold=${cfg.lightgun_accel_threshold}`,
    `lightgun_accel_gain=${cfg.lightgun_accel_gain}`,
    `lightgun_recoil_intensity=${cfg.lightgun_recoil_intensity}`,
    `lightgun_recoil_duration_ms=${cfg.lightgun_recoil_duration_ms}`,
    `lightgun_rapidfire_interval_ms=${cfg.lightgun_rapidfire_interval_ms}`,
    `lightgun_led_r=${cfg.lightgun_led_r}`,
    `lightgun_led_g=${cfg.lightgun_led_g}`,
    `lightgun_led_b=${cfg.lightgun_led_b}`,
    `lightgun_raw_led_r=${cfg.lightgun_raw_led_r}`,
    `lightgun_raw_led_g=${cfg.lightgun_raw_led_g}`,
    `lightgun_raw_led_b=${cfg.lightgun_raw_led_b}`,
    `lightgun_translation_gain=${cfg.lightgun_translation_gain}`,
    `lightgun_translation_decay=${cfg.lightgun_translation_decay}`,
  ].join('\n') + '\n');
}

function readUiConfig() {
  try {
    const raw = JSON.parse(fs.readFileSync(uiConfigPath, 'utf8'));
    return {
      mode: raw.mode || 'lightgun',
      pseye: raw.pseye || false,
      autoload: raw.autoload || false,
      startMinimized: raw.startMinimized || false,
      minimizeToTray: raw.minimizeToTray || false,
      driverPath: raw.driverPath || '',
    };
  } catch (_) {
    return { mode: 'lightgun', pseye: false, autoload: false, startMinimized: false, minimizeToTray: false, driverPath: '' };
  }
}

function writeUiConfig(cfg) {
  fs.mkdirSync(configDir, { recursive: true });
  fs.writeFileSync(uiConfigPath, JSON.stringify(cfg, null, 2));
}

// ── Driver ────────────────────────────────────────────────────────────────────
function findDriverExe(hint) {
  // dir build (not portable): process.execPath is the real exe path
  const exeDir = path.dirname(process.execPath);
  const appPath = app.getAppPath();
  const candidates = [
    hint,
    path.join(exeDir, 'ps-aim-windows.exe'),         // same dir
    path.join(exeDir, '..', 'ps-aim-windows.exe'),    // one level up (ui/ subdir layout)
    path.join(appPath, '..', '..', 'target', 'release', 'ps-aim-windows.exe'),
    path.join(appPath, '..', '..', 'target', 'debug', 'ps-aim-windows.exe'),
    path.join(appPath, '..', 'release', 'ps-aim-windows.exe'),
  ].filter(Boolean);
  return candidates.find(p => { try { return fs.existsSync(p); } catch(_) { return false; } }) || '';
}

function pushLog(line) {
  const ts = new Date().toLocaleTimeString('en-US', { hour12: false });
  logLines.push(`[${ts}] ${line}`);
  if (logLines.length > 200) logLines.shift();
  if (mainWindow && !mainWindow.isDestroyed()) {
    mainWindow.webContents.send('log-line', `[${ts}] ${line}`);
  }
}

function startDriver(uiCfg) {
  if (driverProcess) stopDriver();
  const exePath = findDriverExe(uiCfg.driverPath);
  if (!exePath) {
    const msg = 'ps-aim-windows.exe not found. Browse to set the path.';
    pushLog('ERROR: ' + msg);
    return { ok: false, error: msg };
  }
  const args = [];
  if (uiCfg.mode === 'lightgun') args.push('--lightgun');
  else if (uiCfg.mode === 'lightgun-raw') args.push('--lightgun-raw');
  if (uiCfg.pseye && uiCfg.mode !== 'ds4') args.push('--pseye');
  pushLog(`Starting: ${path.basename(exePath)} ${args.join(' ')}`);
  try {
    driverProcess = spawn(exePath, args, { stdio: ['ignore', 'pipe', 'pipe'] });
    driverProcess.stdout.on('data', d => String(d).split('\n').filter(Boolean).forEach(pushLog));
    driverProcess.stderr.on('data', d => String(d).split('\n').filter(Boolean).forEach(pushLog));
    driverProcess.on('exit', (code) => {
      pushLog(`Driver exited (code ${code})`);
      driverProcess = null;
      if (mainWindow && !mainWindow.isDestroyed()) mainWindow.webContents.send('driver-exited');
    });
    return { ok: true, exePath };
  } catch (e) {
    pushLog('ERROR: ' + e.message);
    return { ok: false, error: e.message };
  }
}

function stopDriver() {
  if (driverProcess) { driverProcess.kill(); driverProcess = null; }
}

function killTracker() {
  try { require('child_process').execSync('taskkill /F /IM ps_aim_tracker.exe', { stdio: 'ignore' }); } catch(_) {}
}

// ── Tray ──────────────────────────────────────────────────────────────────────
function createTray() {
  if (tray) return;
  const iconPath = path.join(__dirname, 'icon.ico');
  const icon = fs.existsSync(iconPath)
    ? nativeImage.createFromPath(iconPath)
    : nativeImage.createFromDataURL('data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAYAAAAf8/9hAAAASElEQVQ4jWNgGAWkgv8MDAz/ScT/GRiYGEiw/z8DA8P/gXYBiQaQ6gKSDSDZBSQbQLILSDaAZBdIMoBkF5BsAMkuINkAkl1AsgEAw+ACBM5JIBcAAAAASUVORK5CYII=');
  tray = new Tray(icon);
  tray.setToolTip('PS Aim Controller');
  tray.on('click', () => {
    if (!mainWindow || mainWindow.isDestroyed()) {
      createWindow();
    } else {
      mainWindow.show();
      mainWindow.focus();
    }
  });
  tray.setContextMenu(Menu.buildFromTemplate([
    { label: 'Open', click: () => {
      if (!mainWindow || mainWindow.isDestroyed()) { createWindow(); }
      else { mainWindow.show(); mainWindow.focus(); }
    }},
    { label: 'Stop Driver', click: () => stopDriver() },
    { type: 'separator' },
    { label: 'Quit', click: () => { isQuitting = true; stopDriver(); killTracker(); app.quit(); } },
  ]));
}

function destroyTray() {
  if (tray) { tray.destroy(); tray = null; }
}

// ── Window ────────────────────────────────────────────────────────────────────
function createWindow() {
  mainWindow = new BrowserWindow({
    width: 520, height: 540,
    resizable: false,
    frame: false,
    backgroundColor: '#0f0f13',
    show: false,
    webPreferences: {
      nodeIntegration: false,
      contextIsolation: true,
      preload: path.join(__dirname, 'preload.js'),
    },
  });

  mainWindow.loadFile(path.join(__dirname, 'index.html'));

  mainWindow.once('ready-to-show', () => {
    const cfg = readUiConfig();
    // Start minimized: only hide if BOTH startMinimized AND autoload are on
    if (cfg.startMinimized && cfg.autoload) {
      // Stay hidden -- already in tray
    } else {
      mainWindow.show();
    }
  });

  mainWindow.on('closed', () => { mainWindow = null; });

  mainWindow.on('close', (e) => {
    if (isQuitting) return; // let it close for real
    const cfg = readUiConfig();
    if (cfg.minimizeToTray) {
      // Hide to tray instead of closing
      e.preventDefault();
      mainWindow.hide();
    } else {
      // No tray mode: actually quit everything
      isQuitting = true;
      stopDriver();
      killTracker();
      // Don't preventDefault -- window closes, then app quits
      app.quit();
    }
  });
}

// ── IPC ───────────────────────────────────────────────────────────────────────
ipcMain.handle('read-config',     () => readConfig());
ipcMain.handle('reset-config',    () => {
  try { fs.unlinkSync(configPath); } catch(_) {}
  return readConfig();
});
ipcMain.handle('write-config',    (_, c) => writeConfig(c));
ipcMain.handle('read-ui-config',  () => readUiConfig());
ipcMain.handle('write-ui-config', (_, c) => {
  writeUiConfig(c);
  if (c.minimizeToTray) createTray();
  else destroyTray();
  // Windows startup
  if (app.isPackaged) {
    app.setLoginItemSettings({
      openAtLogin: c.startupOnBoot || false,
      path: process.execPath,
    });
  }
});
ipcMain.handle('start-driver',    (_, c) => startDriver(c));
ipcMain.handle('stop-driver',     () => { stopDriver(); return { ok: true }; });
ipcMain.handle('driver-status',   () => ({ running: !!(driverProcess && driverProcess.exitCode == null) }));
ipcMain.handle('get-log',         () => logLines);
ipcMain.handle('find-driver-exe', (_, hint) => {
  // Always try auto-detect first -- ignore saved hint if auto-detect
  // finds something valid, so moving the folder doesn't get stuck on
  // the old path. Only use the saved hint as last resort.
  const autoFound = findDriverExe('');
  if (autoFound) return autoFound;
  return hint ? findDriverExe(hint) : '';
});
ipcMain.handle('debug-paths', () => ({
  execPath: process.execPath,
  exeDir: path.dirname(process.execPath),
  appPath: app.getAppPath(),
}));
ipcMain.handle('hidhide',         (_, action, uiCfg) => {
  const exePath = findDriverExe(uiCfg.driverPath);
  if (!exePath) return { ok: false, error: 'Driver exe not found' };
  const flag = action === 'setup' ? '--setup-hidhide' : '--wipe-hidhide';
  return new Promise((resolve) => {
    exec(`"${exePath}" ${flag}`, (err, stdout, stderr) => {
      const out = (stdout + stderr).trim();
      pushLog(`HidHide ${action}: ${out || (err ? err.message : 'done')}`);
      resolve({ ok: !err, output: out });
    });
  });
});
ipcMain.handle('browse-exe', async () => {
  const r = await dialog.showOpenDialog(mainWindow, {
    title: 'Locate ps-aim-windows.exe',
    filters: [{ name: 'Executable', extensions: ['exe'] }],
    properties: ['openFile'],
  });
  return r.canceled ? null : r.filePaths[0];
});
ipcMain.handle('minimize', () => mainWindow?.minimize());
ipcMain.handle('resize-window', (_, _a, _b, height) => {
  if (!mainWindow || mainWindow.isDestroyed()) return;
  if (height) mainWindow.setContentSize(520, height, true);
});

// ── App lifecycle ─────────────────────────────────────────────────────────────
app.whenReady().then(() => {
  const cfg = readUiConfig();
  // Only create tray if minimizeToTray is enabled
  if (cfg.minimizeToTray) createTray();
  createWindow();
  if (cfg.autoload) setTimeout(() => startDriver(cfg), 800);
});

app.on('window-all-closed', () => {
  // Keep app alive only if tray exists
  if (!tray) app.quit();
});

app.on('before-quit', () => {
  isQuitting = true;
  stopDriver();
  killTracker();
});
