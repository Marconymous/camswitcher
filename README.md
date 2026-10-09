# CamSwitch

Pick several connected cameras, switch between them with global hotkeys, and expose the result as one
virtual camera ("CamSwitch Camera") that Teams, Zoom, browsers and the Windows Camera app can select.
Windows 11 only (uses `MFCreateVirtualCamera`).

## Layout

- `crates/app` – `camswitch.exe`: egui UI, camera capture (Media Foundation), compositor, global hotkeys,
  virtual camera registration.
- `crates/vcam-source` – `vcam_source.dll`: COM media source loaded by the Windows Frame Server service.
- `crates/shared` – shared-memory frame protocol and image helpers used by both.

## Build & run

```powershell
cargo build                      # or: cargo build --release
.\scripts\register.ps1           # elevated; re-run after every DLL change (-Release for release build)
.\target\debug\camswitch.exe
```

`register.ps1` copies the DLL to `C:\Program Files\CamSwitch` (Frame Server runs as LocalService and
must be able to read it), registers the COM class under HKLM and stops Frame Server so an old DLL is
released. `.\scripts\register.ps1 -Unregister` removes it.

The virtual camera exists only while `camswitch.exe` runs.

## Default hotkeys

| Action           | Keys                    |
|------------------|-------------------------|
| Camera 1–9       | Ctrl+Alt+Shift+1…9      |
| Next / previous  | Ctrl+Alt+Shift+→ / ←    |
| Mute (black)     | Ctrl+Alt+Shift+M        |

Shift is included because Ctrl+Alt equals AltGr on many keyboard layouts. All bindings can be changed in
the app (Hotkeys section).

## Tray, autostart, notifications

- Closing the window hides it to the tray icon (blue, red while muted). Left-click shows the window, the
  right-click menu switches cameras, mutes, toggles **Start with Windows** and quits.
- **Start with Windows** adds `camswitch.exe --minimized` to `HKCU\...\CurrentVersion\Run`; `--minimized` (or
  "Start minimized to tray") starts hidden in the tray.
- After a hotkey or tray action a short on-screen notification shows the new camera or mute state.

All of these can be switched off in the **General** section.

## Picture adjustments

The ⚙ button next to a selected camera opens per-camera settings: mirror, rotate (90° steps), digital
zoom with pan, and fit (whole picture with black bars, or crop to fill the output). They apply to the
preview and the virtual camera and are saved per camera.

## Switch modes

- **Instant**: all selected cameras stream, switching is immediate (more USB bandwidth/CPU).
- **On demand**: only the active camera streams; on a switch the previous camera keeps feeding the output
  until the new one delivers its first frame.

## Debug info

Press **F12** (or "🐞 Debug info" in the window) for a separate window with live stats: per-camera native
format, fps, throughput and timings, compositor loop timings and latency, shared-memory state, virtual
camera (DLL) delivery counters, and process CPU/memory. "Copy as text" puts everything on the clipboard
for bug reports. DLL counters need a DLL built from the same version (re-run `register.ps1`).

## Logs

- App: `%APPDATA%\CamSwitch\config\camswitch.log`, settings in `config.toml` next to it.
- DLL: `C:\ProgramData\CamSwitch\vcam.log` (or `vcam-<pid>.log` when the host process can't share it).
