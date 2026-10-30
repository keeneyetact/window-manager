<div align="center">

# Window Manager

**Make any window see-through, keep it on top, resize it, or float a live copy of it anywhere on your screen.**

A small, fast tray app for Windows 10 and 11. One file, no installer, no account, no telemetry.

[**⬇ Download the latest version**](https://github.com/keeneyetact/window-manager/releases/latest/download/windows-manager.exe)
&nbsp;·&nbsp; [All releases](https://github.com/keeneyetact/window-manager/releases)
&nbsp;·&nbsp; [Report a problem](https://github.com/keeneyetact/window-manager/issues)

</div>

---

## What it does

Pick any open window from a searchable list, then:

| | Feature | What it's good for |
|---|---|---|
| 🌫️ | **Opacity** | Fade a window from 100% down to 10% to see what's behind it. Watch a video through your notes, or trace over a reference image. |
| 📌 | **Always on top** | Pin a window so other windows can't cover it. |
| 🗂️ | **Stack order for pinned windows** | When several windows are pinned, choose which one sits on top. Windows normally reshuffle pinned windows every time you click one. Window Manager keeps the order you set. |
| ↔️ | **Exact width** | Drag a slider to make a window narrower or wider, with its left edge kept in place. Handy for side panels, chat windows and dashboards. |
| ⧉ | **Live copies** | Make a live picture of a window and put it anywhere, at any size. Crop it to just the part you care about, like a chart, a video or a status bar. |
| 🙈 | **Hidden from screen capture** | Window Manager and its live copies don't show up in screenshots, screen recordings or screen sharing. |
| 🔄 | **Automatic updates** | Checks for new versions and updates itself in one click. |

**Reset window** sets a window back to full opacity and unpins it.

---

## Get started

### 1. Download

Get **[windows-manager.exe](https://github.com/keeneyetact/window-manager/releases/latest/download/windows-manager.exe)** from the latest release. It's a single program file, so there's nothing to install.

> **"Windows protected your PC"?** The app isn't code-signed yet, so Microsoft SmartScreen may warn you the first time you run it. Click **More info**, then **Run anyway**. You can always [build it yourself](#build-from-source) from this source code instead.

### 2. Run it

Double-click `windows-manager.exe`. The main window opens, and an icon appears in the notification area (the tray) at the bottom right of your screen.

> **Can't find the tray icon?** Click the **^** arrow next to the clock to show hidden icons. You can drag the icon onto the taskbar to keep it visible.

### 3. Adjust a window

1. Click a window in the list. Type in the search box to filter by window title or app name.
2. Use the controls on the right.
3. Close Window Manager when you're done. It keeps running in the tray so pinned windows stay in order.

---

## Using it

### The tray icon

Window Manager lives in the tray and has no taskbar button.

- **Left-click** the icon to open the window.
- **Right-click** it for **Open Window Manager** and **Quit**.
- **Closing or minimizing** the window only hides it. To exit completely, choose **Quit** from the tray menu.

### The window list

Each row shows the window's title and the app it belongs to. Badges on the right tell you what's been changed:

- **`60%`** means the window's opacity has been changed.
- **`📌 TOP`** or **`📌 #2`** means it's pinned on top, with its position in the stack.
- **`⧉ 3`** is the number of live copies that are open.

The list refreshes by itself every couple of seconds. You can also click **⟳ Refresh**.

### Opacity

Drag the slider, or click **25%**, **50%**, **75%** or **100%**. The lowest setting is 10% so you can't lose a window completely.

### Width

Drag the slider to resize the window. Its left edge and height stay where they are.

Some apps won't go below a minimum width, and Window Manager will tell you where a window stopped. Chrome browser windows, for example, stop at around 500 px.

> **Tip:** A website opened as a Chrome app window can get much narrower, down to about 180 px. To open one, run:
> ```
> chrome --app=https://example.com
> ```

### Always on top and stack order

Turn on **Always on top** to pin a window above everything else.

Once two or more windows are pinned, you'll see **Stack order #1 of N** with **⏶** and **⏷** buttons:

- **⏶ / ⏷** move a window up or down the stack.
- Windows can **share a position**. Windows in the same position act the way Windows normally does with each other, so whichever you clicked last is on top. Pressing ⏶ or ⏷ on a window that shares a position gives it a position of its own, just above or below the others.

Window Manager checks the order several times a second and fixes it if something gets out of place.

### Live copies

A live copy is a moving picture of a window that updates as the original changes. It's only a picture, so clicks and typing never reach the original. That means you can move, resize, crop and close copies without any risk to the real window.

Click **⧉ New copy** to make one. Then:

| Do this | To |
|---|---|
| Drag the copy | Move it |
| Drag an edge or corner | Resize it |
| Scroll the mouse wheel | Zoom it in or out |
| Double-click it | Bring the original window to the front |
| Right-click it | Open the copy menu |

The **right-click menu** has:

- **Always on top.** Copies start pinned on top. Turn this off to let other windows cover them.
- **Select region…** Drag a rectangle over the part you want to keep. Press **Esc** or right-click to cancel.
- **Show whole window** to undo a crop.
- **Actual size** to show the copy at the same pixel size as the original.
- **Bring original to front**
- **Close copy**

You can open as many copies of a window as you like. Close them all at once with **Close all**. A copy closes by itself when its original window closes.

> **Tip:** A minimized window has nothing to draw, so its copies go blank. To watch a window you don't want in the way, keep it open and move it behind other windows or onto another monitor.

### Hidden from screen capture

Window Manager's own window and all its live copies are excluded from screen capture. They still show on your screen, but screenshots, screen recordings and screen-sharing apps don't see them. This works the same way as the "Screen security" setting in privacy-focused apps like Signal.

The windows you adjust are **not** hidden. Only Window Manager's own windows are.

### Updates

Window Manager checks GitHub for a new release when it starts and every 6 hours after that. When one is available, a banner appears at the top of the window:

- **⬇ Update now** downloads the new version, replaces the program file and restarts. Opacity and pinning stay on your windows. Live copies close, and you may need to set the stack order again.
- **What's new** opens the release notes.
- **Later** hides the banner until the next check.

The update check only contacts `api.github.com` and `github.com`. Nothing else is sent anywhere.

---

## Start with Windows

To have Window Manager start in the tray every time you sign in, run the install script from a copy of this repository. It builds the app from source, so you'll need [Rust](https://rustup.rs) installed:

```powershell
powershell -ExecutionPolicy Bypass -File install.ps1
```

This copies the app to `%LOCALAPPDATA%\Programs\WindowManager`, adds it to your startup apps, and starts it in the tray. No administrator rights are needed.

To remove it:

```powershell
powershell -ExecutionPolicy Bypass -File install.ps1 -Uninstall
```

**Already using the downloaded `.exe`?** You can add it to startup by hand:

1. Press **Win + R**, type `shell:startup`, and press **Enter**.
2. Right-click in the folder and choose **New → Shortcut**.
3. Browse to `windows-manager.exe`, then add ` --background` to the end of the target so it starts in the tray without opening its window.

---

## Troubleshooting

**A setting doesn't work on some windows and I see "try running as administrator".**
Windows doesn't let normal programs change windows that belong to programs running as administrator, like Task Manager or an elevated terminal. Right-click `windows-manager.exe` and choose **Run as administrator** to adjust those too.

**A window won't get any narrower.**
The app has its own minimum width, and Window Manager can't go below it. The panel shows the width where the window stopped.

**My pinned windows lost their order after I quit.**
Window Manager keeps the stack order only while it's running. Windows stay pinned after you quit, but Windows goes back to its usual "last clicked is on top" order.

**A live copy is blank or black.**
The original window is probably minimized. Restore it. Some apps that use hardware video protection (DRM) may also show up blank.

**An app I'm looking for isn't in the list.**
The list shows visible windows that have a title. Hidden, untitled and tool windows are left out. Click **⟳ Refresh** if a window has just opened.

**The update failed.**
Check your internet connection and try again later. You can also download the [latest release](https://github.com/keeneyetact/window-manager/releases/latest) by hand and replace your copy.

---

## Requirements

- Windows 10 version 2004 (May 2020 Update) or later, or Windows 11
- 64-bit PC

---

## Build from source

You'll need [Rust](https://rustup.rs) 1.85 or later.

```powershell
git clone https://github.com/keeneyetact/window-manager.git
cd window-manager
cargo build --release
```

The program is written to `target\release\windows-manager.exe`.

### How it's put together

| File | What's in it |
|---|---|
| [src/main.rs](src/main.rs) | The window list, controls, tray icon and stack-order keeper, built with [egui](https://github.com/emilk/egui) |
| [src/mirror.rs](src/mirror.rs) | Live copies, drawn by the Windows compositor (DWM thumbnails) |
| [src/update.rs](src/update.rs) | Checks GitHub Releases and swaps in the new program file |
| [install.ps1](install.ps1) | Builds, installs for the current user and adds the app to startup |
| [.github/workflows/release.yml](.github/workflows/release.yml) | Builds and publishes a release when a version tag is pushed |

### Making a release

1. Bump `version` in [Cargo.toml](Cargo.toml).
2. Commit, then tag and push:
   ```powershell
   git tag v0.2.0
   git push origin v0.2.0
   ```

GitHub Actions checks that the tag matches `Cargo.toml`, builds the app and publishes `windows-manager.exe` to a new release. Installed copies pick it up on their next update check. The release asset must be named exactly `windows-manager.exe`.

---

## Contributing

Bug reports and pull requests are welcome. When reporting a problem, please include:

- your Windows version (press **Win + R** and run `winver`)
- the app whose window misbehaved
- the Window Manager version, shown next to the title

---

## Privacy

Window Manager runs entirely on your PC. It doesn't collect, store or send any information about you or your windows. The only network request it makes is the update check to GitHub.
