# Nectar Settings Reference

All settings are stored in `settings.json` in the app config directory (`%APPDATA%/com.kua8.nectar/`, derived from the Tauri app identifier). The file is a flat JSON object with `nectar-` prefixed keys. Nectar watches this file for external changes and applies them in real-time.

## Quick Start

Edit `settings.json` with any text editor while Nectar is running. Changes are applied immediately — no restart required.

```json
{
  "nectar-dock-enabled": "true",
  "nectar-dock-mode": "smart",
  "nectar-theme-mode": "dark",
  "nectar-scale": "1.0"
}
```

## Settings Keys

### Dock

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `nectar-dock-enabled` | `"true"` / `"false"` | `"true"` | Show or hide the Nectar Dock (taskbar replacement). |
| `nectar-dock-mode` | `"fixed"` / `"smart"` / `"peek"` / `"hybrid"` | `"smart"` | Dock visibility behavior. **fixed** = always visible as AppBar. **smart** = auto-hide when overlapped by another window or a fullscreen app. **peek** = hidden until cursor approaches bottom edge. **hybrid** = reserves no strip (maximized windows reach the screen bottom) and stays on top of them; it still hides for fullscreen apps and Start/Search, and a lone tap of the `nectar-dock-hybrid-key` key hides or shows it (while hidden, the bottom edge does not reveal it and it takes no clicks). Smart/peek still reveal on hover and are clickable even over a fullscreen app — only a thin pre-emptive strip right at the screen edge stays click-through, so other apps' own edge-hugging UI (e.g. a Snipping Tool selection) is never intercepted. |
| `nectar-dock-preview-enabled` | `"true"` / `"false"` | `"true"` | Show window thumbnail previews when hovering dock icons. |
| `nectar-dock-hide-delay` | number string (ms) | `"800"` | Smart and peek dock only. How long the dock stays up after the cursor leaves. Options from `800` up to `10800000` (3 h). |
| `nectar-dock-hybrid-key` | `"altgr"` / `"rctrl"` / `"off"` | `"altgr"` | Hybrid dock only (`nectar-dock-mode` or a `nectar-dock-mode-by-monitor` entry is `"hybrid"`). Key that hides the dock when tapped on its own and shows it on the next tap. **altgr** = Right Alt / AltGr, **rctrl** = Right Ctrl, **off** = no key (the dock stays shown, except in fullscreen). Held together with any other key or modifier (AltGr+Q = @, AltGr+7 = {, Ctrl+Alt…), or held long enough to auto-repeat, it does nothing. One tap hides every hybrid dock together. The hidden state is not saved: the dock is shown after a restart, after leaving hybrid mode, or when the key is set to off. Only the key state is tracked by the keyboard hook, no other keys are read or stored. |
| `nectar-dock-preview-delay` | number string (ms) | `"300"` | How long the cursor rests on a dock icon before its window preview opens. Options `0`, `100`, `200`, `300`, `500`, `800`. Only used while previews are on (`nectar-dock-preview-enabled`). |
| `nectar-dock-attention-enabled` | `"true"` / `"false"` | `"true"` | Show a dot on running apps that want attention: the ones that flash their taskbar button (chat apps on a new message, for example), until you switch to them. When an app puts an unread count in its window title, like `(3) Inbox` or `Telegram (5)`, the dot shows the number. Off in Legacy Mode. |
| `nectar-dock-search-enabled` | `"true"` / `"false"` | `"true"` | Show a search icon next to Start. What it opens is set by `nectar-dock-search-provider`. |
| `nectar-dock-search-provider` | `"windows"` / `"cmdpal"` / `"ptrun"` / `"raycast"` / `"shortcut"` | `"windows"` | What the dock's search icon opens. **windows** = Windows Search (Win+S). **cmdpal** = PowerToys Command Palette and **ptrun** = PowerToys Run, opened the same way as their own hotkey, so they follow your PowerToys settings. **raycast** = presses Raycast's shortcut (`nectar-dock-search-shortcut`, or Alt+Space if empty), since Raycast for Windows can't be opened by another app yet. **shortcut** = presses `nectar-dock-search-shortcut` for any other launcher. If the chosen launcher isn't running, or the shortcut can't be read, Windows Search opens instead and the dock shows why above the search icon. Legacy Mode always uses Windows Search. |
| `nectar-dock-search-shortcut` | string | `""` | Shortcut pressed for `raycast` and `shortcut`, written like `"Alt+Space"` or `"Win+Ctrl+K"`: any of Win, Ctrl, Alt, Shift, then one key (a letter, digit, F1 to F24, Space, Enter, Tab, Esc, arrows, Home, End, PageUp, PageDown, Insert, Delete, Backspace, or one of `` ` - = , . / ; ' [ ] \ ``). |
| `nectar-dock-calendar-enabled` | `"true"` / `"false"` | `"true"` | Show a calendar icon next to the search icon that opens the Calendar window. The Calendar window is also in the dock's Nectar Options menu, the tray menu, and the notch calendar's open button. |
| `nectar-dock-tray-enabled` | `"true"` / `"false"` | `"true"` | Show a ^ button that opens the hidden tray icons popup. It sits in the dock's system area (right end, next to the network and volume icons). Hidden in Legacy Mode. |
| `nectar-dock-clock-enabled` | `"true"` / `"false"` | `"true"` | Show the time and date (two lines) in the dock's system area. Click opens the Calendar window. The date and time use the Windows regional format locale (Region > Formats, the locale tag only, custom short-format patterns are not read); the 12/24-hour setting (`nectar-time-format-24h`) overrides the hour cycle once it has been written. Updates once a minute and stops while the dock is hidden (auto-hide, fullscreen in smart/peek modes, collapsed). Hidden in Legacy Mode. |
| `nectar-dock-network-enabled` | `"true"` / `"false"` | `"true"` | Show a Wi-Fi (signal bars), Ethernet or not-connected icon in the system area. Click opens a Nectar flyout above the dock with the notch Control Center's Wi-Fi and Bluetooth tiles (toggle) and lists (networks with signal, connect/disconnect, password, forget; paired devices); click again, Esc or a click outside closes it. Refreshes every 5 s only while the dock is shown (not while auto-hidden, in fullscreen or collapsed) and once when it reappears. Hidden in Legacy Mode. |
| `nectar-dock-volume-enabled` | `"true"` / `"false"` | `"true"` | Show the volume level and mute state in the system area. Follows live volume events. Click opens a Nectar sound flyout above the dock: volume slider with mute, output devices and per-app volumes (polled only while it is open), plus a link to Windows sound settings. Hidden in Legacy Mode. |
| `nectar-dock-notifications-enabled` | `"true"` / `"false"` | `"true"` | Show a bell in the system area that presses Win+N (Windows notification center). Hidden in Legacy Mode. |
| `nectar-dock-media-enabled` | `"true"` / `"false"` | `"true"` | Show a mini player (album art and play/pause) in the system area while media is playing. Clicking the art brings the source app forward. Hidden in Legacy Mode. |
| `nectar-dock-taskview-enabled` | `"true"` / `"false"` | `"false"` | Show a Task View button next to the calendar icon (Win+Tab). Hidden in Legacy Mode. |
| `nectar-dock-show-desktop-corner` | `"true"` / `"false"` | `"false"` | Show a thin bar at the right end of the dock that shows the desktop (Win+D) when clicked, and do the same for a click in the very bottom-right corner of the screen (a 6 px square on the dock's monitor, not while a fullscreen app is in front). |
| `nectar-dock-recycle-enabled` | `"true"` / `"false"` | `"false"` | Show the Recycle Bin in the dock. Hidden in Legacy Mode. |
| `nectar-dock-console-windows` | `"true"` / `"false"` | `"false"` | Show Command Prompt, batch script and PowerShell console windows (hosted by conhost) as running apps in the dock. Windows Terminal windows always show. |
| `nectar-debug-battery` | `"level,charging"`, e.g. `"15,false"` | unset | Development builds only. Fakes a battery at that level, for testing the low battery label on a desktop PC. Ignored in releases. |
| `nectar-dock-drives` | JSON array, e.g. `["C","D"]` | `[]` | Drive letters to show as dock buttons that open the drive in Explorer. Hidden in Legacy Mode. |
| `nectar-dock-hidden-apps` | JSON array of `{"id","name"}` | `[]` | Running apps hidden from the dock with Hide from Dock in the right-click menu. Restore them in Settings > Dock. |
| `nectar-dock-icon-only` | `"true"` / `"false"` | `"false"` | Minimal icon-only style (no background/padding around icons). |
| `nectar-start-icon` | `"default"` / `"windows"` / `custom:<data URI>` | `"default"` | Start button icon in the dock: the Nectar logo, the Windows logo, or an uploaded image (`custom:` followed by its data URI). |
| `nectar-dock-adaptive` | `"true"` / `"false"` | `"false"` | Fixed dock only. Stretch the dock to full width like a traditional taskbar while a window is maximized, and contract back when it's restored. |
| `nectar-dock-win-number-enabled` | `"true"` / `"false"` | `"true"` | When the taskbar is replaced, Win+1 through Win+9 activate the matching pinned dock app (focus/minimize if running, launch otherwise) instead of the native taskbar slots. |
| `nectar-dock-mixed-reorder` | `"true"` / `"false"` | `"false"` | `"false"` = pinned and running icons stay in two separate groups with a divider between them. `"true"` = a single group — any icon, pinned or not, can be dragged anywhere. |

### Notch

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `nectar-notch-edge-delay` | number string (ms) | `"200"` | Smart and peek notch only. How long the cursor has to rest on the top edge before the notch peeks, so sweeping across a maximized window's tabs doesn't trigger it. `0` peeks instantly. Maximum `2000`. Set in Settings > Notch > Edge delay. |
| `nectar-notch-mode` | `"fixed"` / `"smart"` / `"peek"` | `"fixed"` | Notch (top bar) visibility behavior. **fixed** = always visible, never auto-hides (including over fullscreen apps — matches a real hardware notch, which doesn't disappear either). **smart** = hides when overlapped by another window/fullscreen app, reveals on hover. **peek** = hidden until cursor approaches the top edge, a media event, or a notification. Smart/peek reveal and are fully clickable on hover even over a fullscreen app (only a thin strip right at the screen edge stays click-through, so it never steals input meant for another app). Unlike the Dock, the notch never reserves desktop work-area space, so it never leaves a gap above maximized windows' content. |
| `nectar-notch-timer-progress` | `"true"` / `"false"` | `"true"` | Show an edge progress bar along the bottom of the notch while a timer or Pomodoro session is active. |
| `nectar-notch-scroll-volume` | `"true"` / `"false"` | `"false"` | Scroll with mouse wheel over the volume, brightness and mixer sliders to adjust them. |
| `nectar-agent-api` | `"true"` / `"false"` | `"false"` | Let AI agents on this computer show their status next to the notch. When on, Nectar listens on `127.0.0.1` only, on a random port, and writes the port and a token to `agent.json` in the settings folder. Agents report with `nectar.exe agent <working|needs-input|done|error|clear> [--name NAME] [--project PROJECT]`. Takes effect after a restart. While it is on, the notch gets an **AI Agents** panel (`agents`, a robot icon in the tab strip, ordered by `nectar-panel-order`): a chip row with how many agents need you, are working, failed or are done (Clear removes the finished and failed ones), then one row per session. A row shows the session title (the first line of your first prompt, about 60 characters, kept in memory only and never written to disk), what it is doing (tool name and target file or command, "Allow Bash?", "Finished just now", error), the run time and the project folder. A row waiting for permission is highlighted and has Allow / Deny buttons; clicking any other row brings that session's terminal window to the front. Tool names, targets and titles need the installed `nectar.exe` to be this version or newer. Not shown in Legacy Mode, where the single-agent panel is used. |
| `nectar-agent-style` | `"mascot"` / `"bar"` | `"mascot"` | How the notch shows a working AI agent. **mascot** = small pixel mascot next to the clock. **bar** = the collapsed notch widens into a status bar with a spark icon, the state (the tool name while one runs) and project, a session count when several agents are active, and the elapsed time of the run. Hovering or clicking the bar opens the AI Agents panel. With several agents the bar and mascot show the most urgent one (permission request, then waiting, failed, working, finished) and each session's status runs out on its own. |
| `nectar-claude-usage` | `"true"` / `"false"` | `"true"` | Claude plan usage meter (needs `nectar-agent-api`). Claude Code passes its status line command the plan's 5-hour and weekly limits (`rate_limits`, Pro/Max subscribers only, after the first reply of a session); while this is on, Nectar sets `statusLine` in `~/.claude/settings.json` to the installed `nectar.exe statusline`, which forwards the numbers to the local agent API (`POST /usage`) and prints Claude Code a short line. If you already had a status line, it is kept in `claude-statusline.json` in the settings folder, run after Nectar's with the same input, and its output is what Claude Code shows; turning this or `nectar-agent-api` off (or uninstalling) puts it back. Sessions in the Claude desktop app run no status line; for them Nectar reads the newest sample of the app's own `%APPDATA%\Claude\plan-usage-history.json` (percentages only, no reset time, written by the app about every 15 minutes while it is open) when the AI Agents panel or the agent bar appears, and the newer of the two sources wins per window. The AI Agents panel header then shows a ring with the percentage of the window closest to its limit and its time to reset (or how old the desktop app's sample is); hover for both windows. The collapsed agent bar adds a small ring from 75% on. Nothing polls Anthropic and no credentials are read; a window disappears once its reset time has passed (or, for a desktop sample, after the window's length). |
| `nectar-agent-sounds` | `"true"` / `"false"` | `"true"` | Play a sound when an AI agent needs input, finishes or fails. Each of the three sounds can be replaced with your own mp3, wav, ogg or m4a file (up to 3 MB) from Settings > Notch > AI agents; the files are kept in the `sounds` folder in the settings folder. |
| `nectar-mic-cam-indicator` | `"true"` / `"false"` | `"true"` | While an app uses the microphone, an orange dot shows at the right of the closed notch; a green dot while one uses the camera (read from Windows' privacy records, so it covers the apps that Settings > Privacy > Microphone/Camera lists as in use). In the open notch the dots sit at the right of the tab strip; hovering or clicking them names the apps. Off stops watching entirely. Settings > Notch > Microphone and camera. |
| `nectar-mic-mute-hotkey` | string | unset (= `"Win+Ctrl+K"`) | Meeting mode. Global shortcut, written like `nectar-notch-hotkey`, that mutes every active microphone (all capture devices, so it applies to every app) and unmutes them on the next press. While all microphones are muted, by the shortcut or anywhere else, the notch shows a red crossed-out microphone, also when closed; clicking it unmutes. After Nectar muted them, the next press always unmutes, even if one microphone did not take the mute; while that one is live the crossed-out microphone is yellow ("Some microphones are still on"). A peek notch shows itself briefly when the mute changes. Unset uses the default `Win+Ctrl+K` (Win+Alt+K is Windows' own call-app mute, Win+Alt+M Game Bar's, Win+Ctrl+M Magnifier's, Ctrl+Shift+M Teams'). An empty string turns meeting mode off (no shortcut, no red microphone); if the shortcut is cleared while Nectar's mute is on, the microphones stay muted and the red microphone stays until it is clicked. If another program already uses the shortcut, Nectar logs it and the shortcut does nothing. |
| `nectar-notch-notifications` | `"true"` / `"false"` | `"false"` | Show new Windows notifications (toasts) as a card in the notch instead of only the Windows banner. The notch opens to about panel size and shows the app icon and name, the title, up to three lines of the body and how long ago it arrived. A card stays about 5 s (4 s when more are waiting, shown as **+N**; click it to skip); hovering keeps it open. Clicking the card brings the app's window to the front (or starts the app); **×** dismisses it and also removes it from Notification Center. Nectar reads notifications through Windows' notification listener and needs **notification access** (Windows Settings > Privacy & security > Notifications); Settings > Notch > Notifications shows the state and a button for the Windows prompt or that settings page. Nectar never changes that setting. Notification text is only kept in memory and never written to disk. A permission request from an AI agent comes first; cards wait while a fullscreen app runs, the notch is hidden with the hotkey or another panel is open, and a new card never covers a notch you already opened. Nothing is shown during Windows Do Not Disturb / Focus, presentation mode or Nectar's own Do Not Disturb button. Only the primary notch shows cards. Off in Legacy Mode. CPU stays idle: Nectar waits for Windows to record a new notification (registry and notification-database change signals) instead of polling. |
| `nectar-notch-notifications-hide-toasts` | `"true"` / `"false"` | `"false"` | Opt-in. While notifications show in the notch, turn off the Windows banner for each app through Windows' own per-app setting (`HKCU\Software\Microsoft\Windows\CurrentVersion\Notifications\Settings\<app>`, value `ShowBanner` = 0). Notifications still go to Notification Center. Each app's previous value (or that it had none) is saved first in `notification-banners.json` next to `settings.json` and written back exactly when this is turned off, when `nectar-notch-notifications` is turned off or access is lost, when Nectar quits, and when Nectar is uninstalled; Nectar turns them off again on its next start. Nectar's own notifications, system notifications (`Windows.SystemToast.*`, `Microsoft.Windows.*`) and apps in `nectar-notch-notifications-exclude` keep their banners. An app whose banner setting was changed in Windows while this is on keeps the user's choice; only values still at Nectar's 0 are restored. If `notification-banners.json` can't be read, Nectar hides nothing and leaves the file alone. Buttons inside a notification (reply, answer a call) only work in Notification Center. |
| `nectar-notch-notifications-exclude` | JSON string, e.g. `"[\"Chrome\"]"` | unset | AppUserModelIDs of apps whose notifications never show in the notch (and keep their Windows banner). Set in Settings > Notch > Notifications > Apps, which lists the apps with notifications in Notification Center and the apps Windows keeps notification settings for. |
| `nectar-shelf-persist` | `"true"` / `"false"` | `"false"` | Keep the notch shelf's items after Nectar restarts. The shelf holds files (as links, not copies) and text dropped on the notch; at most 10, newest first. Off clears them when Nectar closes. Files that no longer exist are dropped on start. |
| `nectar-notch-downloads` | `"true"` / `"false"` | `"true"` | Show downloads in the notch. One thread watches the Downloads folder with `ReadDirectoryChangesW` (blocked in the kernel while nothing happens, no polling) and follows browser temp files (Chrome/Edge `.crdownload`, `.partial`, Firefox `.part`) and plain new files. Browsers don't expose the total size to other apps, so the notch shows bytes on disk and speed with an indeterminate bar, not a percentage; sizes are re-read once a second only while a download runs (every 5 s once it has stalled for 30 s). The collapsed notch shows a spinning ring in a free side slot (a check for about 6 s when a download finishes); an AI agent, notification card, update or album art keeps its place. Hovering while downloads are listed opens the Shelf, with the downloads on top (name, bytes, speed); click a finished one to open it, the folder button shows it in Explorer. A finished file is added to the Shelf. Temp files, files under 1 KB, files that disappear (cancelled) and bursts of more than 3 plain files within 2 s (a copy or unzip) are ignored. Off in Legacy Mode. |
| `nectar-notch-downloads-folder` | path string | unset (Windows Downloads known folder) | Folder watched for downloads instead of the Windows Downloads folder (`FOLDERID_Downloads`). Empty means the default. Set in Settings > Notch > Controls and shelf. Subfolders are not watched. |
| `nectar-notch-remember-mode` | `"true"` / `"false"` | `"false"` | When the cursor leaves the notch and comes back, reopen the panel you were on instead of the default. With `nectar-notch-hover-open` on, any panel is remembered; with it off (or in Legacy Mode), only the Control Center and Calendar are, and the default is status or music. |
| `nectar-notch-hover-open` | `"true"` / `"false"` | `"true"` | Moving the cursor anywhere over the notch opens a full panel (`nectar-notch-hover-panel`) instead of only widening the pill. The open notch shows a tab strip at the top: one small icon per enabled panel in `nectar-panel-order` order (a dot under the current one), then a gear that opens Nectar Settings. Scrolling and clicking the clock still switch panels. An AI agent waiting for you and a file dragged onto the Shelf take priority. Off keeps the old behaviour (status, or music while something plays); the tab strip still shows on the open notch. Ignored in Legacy Mode and during the tour. |
| `nectar-notch-hover-panel` | `"command-center"` / `"status"` / `"music"` / `"calendar"` / `"shelf"` / `"clipboard"` / `"agents"` | `"command-center"` | Panel the notch opens on when `nectar-notch-hover-open` is on. If that panel is turned off or unavailable (Music with nothing playing, Calendar with the calendar, timer and stopwatch all off), the notch opens on status or music as before. |
| `nectar-panel-control-center-enabled` | `"true"` / `"false"` | `"true"` | Include the Control Center when scrolling through the notch panels. |
| `nectar-panel-shelf-enabled` | `"true"` / `"false"` | `"true"` | Include the Shelf panel and accept files and text dropped on the notch. |
| `nectar-panel-clipboard-enabled` | `"true"` / `"false"` | `"false"` | Include the Clipboard panel, a row of cards for recently copied text, links, images and files in the notch (click a card to copy it back, pick All / Text / Links / Images / Files at the top to filter). Each card shows the app it was copied from (by its file description), or the folder for files. The clipboard listener only runs while this is on; turning it off also empties the list and deletes the saved history file and images. Text copied with the `ExcludeClipboardContentFromMonitorProcessing` format, or with `CanIncludeInClipboardHistory` or `CanUploadToCloudClipboard` set to 0 (password managers do this), is never recorded. Text (links count as text) is kept up to 100,000 characters per item. Files copied in Explorer are kept as their paths (up to 1,000 per item, the files themselves are not copied); copying the card back puts the same files on the clipboard. When something has files, text and an image, files win, then text, as that is what Ctrl+V pastes first. Images (PNG, or bitmaps from screenshots and "Copy image") are kept up to about 32 MB raw each and 120 MB in total, oldest images dropped first; copying one back puts it on the clipboard as an image. Hidden in Legacy Mode, where recording and the saved history carry on. |
| `nectar-clipboard-history-size` | number string, 5 to 100 | `"20"` | How many clipboard items to keep, newest first. Copying the same text again moves it to the front instead of adding a duplicate. Set in Settings > Notch. |
| `nectar-clipboard-persist` | `"true"` / `"false"` | `"false"` | Keep the clipboard history after Nectar restarts. Off keeps it in memory only. On saves it (text, file paths and the source app name) as plain, unencrypted text in `clipboard_history.json` next to `settings.json`, and each image as a PNG file in a `clipboard_images` folder there; turning it off deletes both. |
| `nectar-panel-order` | JSON string, e.g. `"[\"status\",\"music\"]"` | unset | Order of the notch panels when scrolling: `command-center`, `status`, `music`, `calendar`, `shelf`, `clipboard`, `agents`. Missing panels are added at the end. Unset keeps the default order, where Music moves in front of Status while something plays. Set in Settings > Notch > Panel Order. |
| `nectar-cc-layout` | JSON string | unset | Which Control Center items show, and their order: `{"tiles":[...],"buttons":[...],"sliders":[...]}`. Tiles: `network`, `dock-mode`, `bluetooth`, `notch-mode`. Buttons: `dnd`, `battery-saver`, `tray`, `notifications`, `settings`, `restart`. Sliders: `volume`, `brightness`. An item that is not listed is hidden. A missing list shows all its items in the default order. Set in Settings > Notch > Control Center layout, by dragging items in, out and around like the status widgets. |

### Monitors

The dock and the notch each choose their monitors separately. In Settings this is the **Show On** dropdown on the Dock and Notch tabs.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `nectar-dock-monitor-mode` | `"primary"` / `"all"` / `"specific"` | `"primary"` | Where the dock appears. **primary** = your primary display only. **all** = every connected display gets its own dock. **specific** = the single display named in `nectar-dock-monitor-id`. |
| `nectar-dock-monitor-id` | string | `""` | Windows display name of the monitor to use when the dock mode is `"specific"` (for example `\\.\DISPLAY2`). Set automatically when you pick a monitor in Settings. |
| `nectar-notch-monitor-mode` | `"primary"` / `"all"` / `"specific"` | `"primary"` | Where the notch appears. Same values as the dock. |
| `nectar-notch-monitor-id` | string | `""` | Windows display name of the monitor to use when the notch mode is `"specific"`. Set automatically when you pick a monitor in Settings. |

If the chosen monitor is unplugged, Nectar falls back to the primary display.

With **All Monitors** and more than one display, the **Per-Monitor Behavior** toggle on the Dock and Notch tabs gives each monitor its own Fixed / Smart / Peek. Turning it off goes back to one behavior for all of them.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `nectar-dock-mode-by-monitor` | JSON string | `"{}"` | Per-monitor dock behavior: a JSON object mapping each Windows display name (as in `nectar-dock-monitor-id`) to `"fixed"`, `"smart"`, `"peek"` or `"hybrid"`. A monitor without an entry uses `nectar-dock-mode`. |
| `nectar-notch-mode-by-monitor` | JSON string | `"{}"` | Same as above for the notch, falling back to `nectar-notch-mode`. |

### Weather

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `nectar-weather-enabled` | `"true"` / `"false"` | `"true"` | Show weather widget in the notch status bar. |
| `nectar-weather-city` | string | `""` | Manually set city name for weather. Empty string = auto-detect via IP geolocation (HTTPS only). If detection fails and no city is set, no weather is shown. |
| `nectar-weather-lat` | number string | (auto) | Latitude coordinate for weather. Set automatically when a city is selected. |
| `nectar-weather-lon` | number string | (auto) | Longitude coordinate for weather. Set automatically when a city is selected. |
| `nectar-weather-cached-temp` | number string | (none) | Cached temperature value shown before next API fetch. |
| `nectar-weather-cached-condition` | string | (none) | Cached weather condition text (e.g. "Partly Cloudy"). |
| `nectar-weather-ip-location` | JSON `{"lat":0,"lon":0,"city":""}` | (none) | The last location found from the IP address, kept so the weather still loads when the lookup fails. Set automatically. |
| `nectar-temp-unit` | `"celsius"` / `"fahrenheit"` | `"celsius"` | Temperature display unit. |

### Modules

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `nectar-calendar-enabled` | `"true"` / `"false"` | `"true"` | Show the calendar in the notch's calendar panel. |
| `nectar-timer-enabled` | `"true"` / `"false"` | follows `nectar-calendar-enabled` | Show the countdown timer (presets and a custom time) in the notch's calendar panel. |
| `nectar-timer-sound-enabled` | `"true"` / `"false"` | `"true"` | Play a short chime when the countdown timer finishes. |
| `nectar-stopwatch-enabled` | `"true"` / `"false"` | follows `nectar-calendar-enabled` | Show the stopwatch (with laps) next to the timer. |
| `nectar-dual-clock-enabled` | `"true"` / `"false"` | `"false"` | Show a second clock for another time zone next to the time in the notch. Follows `nectar-time-format-24h`, and shows `+1` / `−1` when that zone is on a different day. |
| `nectar-dual-clock-zone` | IANA time zone string | `"UTC"` | Time zone of the second clock, for example `"Asia/Tokyo"`. Set by searching a city or time zone in Settings › Notch › Dual Clock. |
| `nectar-dual-clock-label` | string | `"UTC"` | Name shown beside the second clock, for example `"Tokyo"`. |
| `nectar-notch-cat-palette` | coat name | `"orange tabby"` | Coat color of the idle cat: `orange tabby`, `gray`, `black`, `white`, `calico`, `cream`, `chocolate`, `lilac`, `pink`, `mint`. Set in Settings › Notch › Cat color (shown when "When idle" is Cat or Cat + 24-hour clock). Pixel cat: SondeR cat by Verisonder (Apache-2.0), see `third_party/sonder-cat`. |
| `nectar-notch-idle` | `"clock"` / `"cat"` / `"hybrid"` | `"hybrid"` (unset) | What the collapsed notch shows when idle. `"cat"` shows a small animated cat (SondeR cat sprites and reactions) in place of the clock; the collapsed notch is 40 px wider. `"hybrid"` shows the cat beside the clock, and the clock is always 24-hour `HH:mm` there, whatever `nectar-time-format-24h` says; the collapsed notch is 80 px wider and the clock stays centred. The cat roams the whole collapsed notch: several times a minute it runs to a new spot at least twice its own width away. It never stops on the clock, a timer, the dual clock or music art; in hybrid it stops on either side of the clock and crosses to the other side by walking behind it. The cat sits and wags its tail, blinks, grooms, sometimes hides and peeks, and follows the cursor with its eyes, also while it walks; a cursor near the notch does not stop it from roaming. It reacts to the user (only input timing is used, never which keys): it types along while you type anywhere, bats at the page while you scroll, ducks and peeks when the cursor comes close, stretches when you come back after 45 s without input, naps after a while awake, and after 3 min without input sleeps until you are back. `"cat"`: the clock comes back on hover, during timers, a low battery and AI agent activity; the cat stays during music. `"hybrid"`: the cat also stays during music and timers (the timer replaces only the clock); it leaves on hover, AI agent activity on the notch, the low-battery label and the power/update pulses. During AI agent activity the cat moves into the agent's place when `nectar-cat-claude` is on. The cat stands still and the input watch is off when the notch is hidden, in fullscreen or when reduced motion is on. An unset value means `"hybrid"`, so installs that never picked a value switch from the clock to hybrid. Set in Settings › Notch › When idle. |
| `nectar-cat-claude` | `"true"` / `"false"` | `"true"` | The cat reacts to AI agents (Claude Code). Only when `nectar-notch-idle` is `"cat"` or `"hybrid"` and Legacy Mode is off. While an agent shows on the collapsed notch, the cat takes the place of the agent mascot (`nectar-agent-style` `"mascot"`, beside the clock) or of the spark icon in the compact bar (`"bar"`, which gets wider by the cat's drawn width minus the 16 px spark: 10 px at 100% / 200% display scaling, about 5 px at 125%, 2 px at 150%). It reacts to the session the notch shows (permission request > needs input > error > working > done): it types while the agent works, cheers with hearts for 2.4 s when the agent finishes and then sits content, meows and holds a "!" until a permission request or a needs-input wait is answered, and jumps then hides after an error. Only fresh news plays a reaction (within 5 s), and the same reaction does not start again within 6 s, however many sessions change. The sound is not rate-limited: like the chime before, every permission request and every new wait makes one sound, and while the cat is on the notch (not under a notification card or a battery/update pulse) that sound is a meow instead of the needs-input chime; it plays only when `nectar-agent-sounds` is on, and a custom needs-input sound picked in Settings plays instead of the meow. Only the typing loop runs for long; reactions stop with the notch hidden, and reduced motion shows still poses. Set in Settings › Notch › Cat reacts to AI agents (shown when "When idle" is Cat or Cat + 24-hour clock). |
| `nectar-calendar-view` | `"day"` / `"week"` / `"month"` / `"agenda"` | `"month"` | Which view the notch calendar shows once a calendar source is connected. Set by the Day / Week / Month / Agenda switcher in the notch. |
| `nectar-calendar-window-view` | `"day"` / `"week"` / `"month"` / `"agenda"` | `"month"` | Which view the Calendar window opens on. It remembers the last one you used. |
| `nectar-calendar-hidden` | JSON array of strings | `[]` | Calendars you switched off in the Calendar window. Set by the window. |
| `nectar-clock-tab` | `"timer"` / `"stopwatch"` | `"timer"` | Which tab the notch clock panel opens on. It remembers the last one you used. |
| `nectar-music-mode-enabled` | `"true"` / `"false"` | `"true"` | Enable interactive music media widget. |
| `nectar-music-compact-notch` | `"true"` / `"false"` | `"true"` | Show compact music display (visualizer + artwork) in collapsed notch. |

### Calendar Sync (links and CalDAV)

Read-only sync of your calendars into the notch calendar. Open Settings › Notch › Calendar Sync (shown while Calendar is on). There are two ways to connect, and you can use both at once. Nectar never writes to a server.

- **Calendar links** need no password: paste a Google, iCloud, Outlook, Proton, Nextcloud or any other `.ics` / `webcal://` link. Repeating events, exceptions and time zones are expanded locally.
- **CalDAV account** (Nextcloud, iCloud, or any CalDAV server) also reads private calendars, using an app password.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `nectar-caldav-enabled` | `"true"` / `"false"` | `"false"` | Whether calendar sync is active. Set to `"true"` when you connect and `"false"` when you disconnect. |
| `nectar-caldav-url` | string | `""` | Server address, for example `cloud.example.com`. A bare host is expanded to `/remote.php/dav/`; you can also give a full CalDAV URL. iCloud uses `https://caldav.icloud.com` (filled in for you) with an app-specific password. |
| `nectar-caldav-username` | string | `""` | Account username. |
| `nectar-caldav-interval` | number string | `"15"` | Minutes between background syncs. Minimum `5`. |

`nectar-caldav-interval` applies to every source, links included. After Nectar restarts or crashes it syncs right away instead of waiting out the interval, and retries a few times if the network isn't back yet.

Secrets are **not** in `settings.json`. The CalDAV app password lives in Windows Credential Manager as `Nectar CalDAV`, and each calendar link (which acts like a password, since anyone holding it can read the calendar) lives there as `Nectar Calendar Link`. Everything else is local to the app config directory:

| File | Contents |
|------|----------|
| `calendar_links.json` | The names and provider of your calendar links (not the links themselves). |
| `calendar_cache.json` | Synced events, so the calendar works offline. Nothing is sent to any third-party service. |

Removing a link or disconnecting the account deletes its stored secret and cached events. If a CalDAV server rejects the login, automatic syncing of that account pauses until you reconnect, so a wrong password can't trigger a lockout. Events from 35 days back to 95 days ahead are synced.

### Music Appearance

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `nectar-media-layout` | `"classic"` / `"compact"` | `"classic"` | Expanded player style. **classic** = large album art. **compact** = small thumbnail + controls. |
| `nectar-media-ambience-enabled` | `"true"` / `"false"` | `"true"` | Colored ambient glow behind expanded album art. |
| `nectar-media-compact-glow-enabled` | `"true"` / `"false"` | `"true"` | Glow effect around the collapsed compact thumbnail. |
| `nectar-media-visualizer-enabled` | `"true"` / `"false"` | `"true"` | Audio visualizer bars in music mode. Also accepts `nectar-visualizer-enabled` (legacy alias). |
| `nectar-media-album-art-enabled` | `"true"` / `"false"` | `"true"` | Show album artwork in the notch music display. |
| `nectar-media-artwork-theme` | `"true"` / `"false"` | `"false"` | Tint the notch music panel with colors extracted from the album art. |

### Overlays

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `nectar-volume-overlay-enabled` | `"true"` / `"false"` | `"true"` | Show Nectar volume HUD when volume changes (replaces native Windows OSD). |
| `nectar-volume-edge-enabled` | `"true"` / `"false"` | `"true"` | Trigger volume HUD by hovering the left screen edge. |
| `nectar-brightness-overlay-enabled` | `"true"` / `"false"` | `"true"` | Show Nectar brightness HUD when brightness changes. |
| `nectar-brightness-edge-enabled` | `"true"` / `"false"` | `"true"` | Trigger brightness HUD by hovering the right screen edge. |
| `nectar-edge-delay` | number string (ms) | `"300"` | How long the cursor has to rest on the left or right screen edge before the volume or brightness HUD appears. `0` shows it instantly. Only the edge strip of the primary monitor counts, so being on another monitor never opens the HUD. |
| `nectar-volume-side` | `"left"` / `"right"` | `"left"` | Screen edge the volume slider appears on. The brightness slider is moved to the opposite edge when both would share one. |
| `nectar-brightness-side` | `"left"` / `"right"` | `"right"` | Screen edge the brightness slider appears on. |

### Appearance

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `nectar-theme-mode` | `"auto"` / `"dark"` / `"light"` / `"custom"` / `"adaptive"` | `"dark"` | Theme mode. **auto** = dark or light, following the Windows app mode. **dark** = dark translucent. **light** = light translucent. **custom** = user-picked color. **adaptive** = follows Windows system accent color. The Presets in Settings › Appearance set `custom` mode with a matching color, saturation, brightness and accent. |
| `nectar-theme-color` | hex string | `"#007aff"` | Custom theme color (used in `custom` and `adaptive` modes). |
| `nectar-theme-opacity` | float string | `"0.80"` | Background opacity (0.1 to 1.0). |
| `nectar-theme-saturation` | float string | `"0.50"` | Color saturation for custom/adaptive themes (0.0 to 1.0). |
| `nectar-theme-brightness` | float string | `"0.15"` | Background brightness for custom/adaptive themes (0.0 to 1.0). |
| `nectar-theme-style` | `"solid"` / `"frosted"` / `"liquid"` / `"material"` | `"solid"` | How the notch, dock and volume/brightness overlays are drawn. **solid** = plain fill. **frosted** = milky haze with fine grain. **liquid** = clear glass with bright rims and a sheen. **material** = Material You tonal palette built from the accent color (the Windows accent when the accent is `mono`). Picking a style in Settings also sets `nectar-theme-opacity` to the value it is tuned for (0.8, 0.6, 0.45, 0.96). The notch and dock windows are transparent, so the glass styles show a blurred copy of the wallpaper on their monitor behind them, lined up with the screen (a solid color desktop shows that color). When an app window is behind them, they switch to a see-through tint so that window shows through instead. The volume/brightness overlays always use the tint. The notch uses a denser fill (at least 82%) while the calendar, command center or shelf is open, so the text stays readable. Legacy Mode always uses Solid with monochrome highlights. |
| `nectar-glass-wallpaper` | `"true"` / `"false"` | `"false"` | Frosted and Liquid only. Show a blurred copy of the desktop wallpaper behind the notch and dock, and the window behind them when there is one. Off keeps the plain tint. Animated wallpapers (Wallpaper Engine) are not shown. |
| `nectar-sync-windows-accent` | `"true"` / `"false"` | `"false"` | Only with a custom accent (`nectar-theme-accent` = `custom`). Also sets the Windows system accent to `nectar-theme-accent-color`: the DWM `AccentColor`, `ColorizationColor` and `ColorizationAfterglow` values and the Explorer `AccentColorMenu`, `StartColorMenu` and `AccentPalette` values under `HKCU`. The first time it is turned on Nectar saves the previous values to `windows-accent-backup.json` in the settings folder, and turning it off writes them back and deletes the file. |
| `nectar-theme-accent` | `"mono"` / `"system"` / `"custom"` | `"mono"` | Color for toggles, active buttons, sliders, progress bars and today's date. **mono** = the text color (the classic look). **system** = Windows accent. **custom** = `nectar-theme-accent-color`. Ignored in Legacy Mode. |
| `nectar-theme-accent-color` | hex string | `"#0a84ff"` | Accent used when `nectar-theme-accent` is `custom`. |
| `nectar-corners-enabled` | `"true"` / `"false"` | `"false"` | Render rounded screen corner overlays on top edges. |

### Status Widgets

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `nectar-net-widget-mode` | `"speed"` / `"status"` | `"speed"` | What the `net` status widget shows. **speed** = live upload and download speed. **status** = an icon for Wi-Fi, Ethernet or offline, with a tooltip. Set in Settings > Notch > Network widget. |
| `nectar-notch-hotkey` | string | `""` | Global shortcut that hides the notch until pressed again, written like `"Ctrl+Alt+N"`: any of Win, Ctrl, Alt, Shift, then one key (same keys as `nectar-dock-search-shortcut`). Empty is off. If another program already uses the shortcut, Nectar logs it and the shortcut does nothing. Set in Settings > Notch > Shortcut by clicking the box and pressing the keys. The hidden state is not saved: the notch is back after a restart. |
| `nectar-status-widgets` | JSON string | `{"left":["weather"],"right":["battery"]}` | Widget layout in collapsed notch. Available: `"weather"`, `"battery"`, `"cpu"`, `"ram"`, `"disk"`, `"net"`. The `"battery"` widget (and its low-battery/charging pulses) is automatically suppressed on desktops with no physical battery — detected via Windows' `PowerManager.BatteryStatus`, not the browser's Battery API (which fakes a permanent "100%, charging" reading on desktops). It's also silently dropped from a placed config on such machines, even if set manually or imported from a laptop's settings file. |

Example:
```json
{
  "nectar-status-widgets": "{\"left\":[\"cpu\",\"ram\"],\"right\":[\"battery\",\"net\"]}"
}
```

### System

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `nectar-scale` | float string | `"1.0"` | UI scale factor (0.8 to 1.3). Changing this re-registers AppBars to resize the reserved screen area. |
| `nectar-low-battery-threshold` | integer string | `"20"` | Battery percentage that triggers the low-battery alert pulse (5 to 50, step 5). |
| `nectar-auto-update` | `"true"` / `"false"` | `"false"` | Check for and download updates automatically on startup. No effect in this fork: update checks and installs are off until the updater points at our own releases (`updater.rs` `UPDATES_ENABLED`, D-037); Settings greys the switch out. |
| `nectar-update-channel` | `"stable"` / `"prerelease"` | `"stable"` | Which releases the updater offers. `"prerelease"` also offers releases published with the pre-release flag, the newest release wins. Never offers a version lower than the running one. Settings > About > Get pre-releases. No effect in this fork while updates are off (D-037); Settings greys the switch out. |
| `nectar-show-update-indicator` | `"true"` / `"false"` | `"true"` | Show a green dot on the notch when an update is available. Never shown in this fork while updates are off (D-037); Settings greys the switch out. |
| `nectar-time-format-24h` | `"true"` / `"false"` | `"false"` | Use 24-hour clock format in the notch. When `"false"`, displays 12-hour format with AM/PM. |
| `nectar-week-start` | `"auto"` / `"sunday"` / `"monday"` | `"auto"` | First day of the week in the notch and Calendar window. `"auto"` follows the language's region. |
| `nectar-language` | `"system"` / language code, e.g. `"en"` | `"system"` | UI language: `en`, `tr`, `de`, `es`, `fr` and `ar` (Arabic mirrors the Settings and Calendar windows right to left). `"system"` follows the Windows display language and falls back to English when that language has no translation. Missing keys in a translation fall back to English. Translations live in `src/locales/<code>.json` and are registered in `src/i18n.ts` and `src-tauri/src/i18n.rs` (tray menu). Calendar dates and times follow this language too. |
| `nectar-tour-seen` | `"1"` | unset | Set once the first-run tour has been finished or skipped. Delete it to see the tour again, or use Settings > About > Replay Tour. |
| `nectar-legacy-mode` | `"true"` / `"false"` | `"false"` | Keep the classic notch and dock. Turns off features added after 1.4 (dock Task View, Recycle Bin and drive buttons, and later additions). Settings > General > Legacy Mode. |
| `nectar-ram-saver` | `"true"` / `"false"` | `"false"` | Hand unused pages back to Windows a few seconds after you stop interacting, and re-trim while the app stays idle. The app is a WebView2 host, so most of its footprint is cache that Chromium would otherwise keep resident forever; this is what brings it from ~125 MB down to ~20 MB of private working set. Applied live, no restart. Settings > General > Memory Saver. |
| `nectar-ram-ultra` | `"true"` / `"false"` | `"false"` | Adds `--disable-gpu` to the WebView2 arguments, which removes the GPU process and about 140 MB of commit at the cost of software rasterising the blur effects. **Requires a restart** (WebView2 reads its arguments once at startup). Settings > General > Maximum Memory Saving. |

### Phone link

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `nectar-phone-link` | `"true"` / `"false"` | `"false"` | Turn the phone link on (Settings > Phone). When off, Nectar makes no connection to the relay and runs no phone link thread, socket or timer; turning it on or off applies at once, without a restart. When on, Nectar connects **out** to the fixed Supabase relay of this build (`phone/relay.config.json`; there is no setting to change it), through the Windows proxy if one is configured, and opens no listening port. Paired phones, their permissions and every key live in Windows Credential Manager (`Nectar Phone Link/*`, this machine only, never roaming), not in `settings.json`. Download folder, file size limits and pending relay cleanup are in `phone-link.json` next to `settings.json`; the audit log is `phone-link-audit.jsonl` in the same folder. **Settings import ignores every `nectar-phone-*` key** and keeps the current value, so an imported file can neither turn the phone link on nor change it. Uninstalling Nectar deletes the phone link credentials. |
| `nectar-phone-clipboard-sync` | `"true"` / `"false"` | `"false"` | Settings > Phone > "Send what I copy to my phone" (T-016). When on and the phone link runs, text copied on this PC goes to every paired phone that has the per-phone permission **Get the PC clipboard** (`clipboardOut`; on for phones paired after this feature, off for phones paired before, changes are audited). It reuses the clipboard history listener (`AddClipboardFormatListener`, no polling); with both this and the clipboard history off, no listener runs. Only text, at most about 64 KB (the encrypted message holding it must fit 64 KB; larger copies are skipped with a line in the diagnostics log); copies Windows marks private (`ExcludeClipboardContentFromMonitorProcessing`, `CanIncludeInClipboardHistory = 0`, `CanUploadToCloudClipboard = 0`, set by password managers) are never sent. Rapid copies are sent once, as the last one after 300 ms; text that came from the phone is not sent back. Over a live session the text goes at once; when the phone is not connected, only the newest copy waits in its relay mailbox (end-to-end encrypted, replaced in place, deleted after 24 h). The phone page writes it to the phone clipboard when it arrives while the page is open and focused, otherwise keeps it with a Copy button (never written later, so it cannot overwrite something copied on the phone since). The audit log records the length, never the text. Ignored by settings import, like every `nectar-phone-*` key. |

### Internal (Do Not Edit Manually)

| Key | Type | Description |
|-----|------|-------------|
| `nectar-first-run` | sentinel | Set to `"done"` after first launch. Triggers splash screen if absent. |
| `nectar-app-version` | string | Last known app version. If it differs from current, splash screen is shown on update. |

## Event System

Nectar uses two Tauri events for settings synchronization:

### `settings-changed`
- **Emitted by:** `save_setting` command (frontend or backend)
- **Payload:** `{ "key": "nectar-...", "value": ... }`
- **Purpose:** Broadcasts changes made through the Nectar UI to all windows
- **Key format:** Nectar-prefixed keys as-is (e.g. `"nectar-dock-mode"`)

### `settings-external-changed`
- **Emitted by:** File watcher (detects external edits to `settings.json`)
- **Payload:** `{ "key": "nectar-...", "value": ... }` or `{ "key": "nectar-...", "value": null }` for removed keys
- **Purpose:** Broadcasts changes made by external editors (VS Code, notepad, scripts)
- **Key format:** Nectar-prefixed keys as-is
- **Behavior:** Also syncs values to `localStorage` for instant frontend reads

### Flow
```
External editor saves settings.json
    ↓
File watcher detects change (ReadDirectoryChangesW)
    ↓
Diffs against SETTINGS_CACHE
    ↓
Emits settings-external-changed for each changed/removed key
    ↓
useSettingsSync hook updates React state + localStorage
    ↓
UI re-renders with new values
```

## Example: Changing Dock Mode via Script

```powershell
# PowerShell: switch dock to smart mode
$json = Get-Content "$env:APPDATA\com.kua8.nectar\settings.json" | ConvertFrom-Json
$json.'nectar-dock-mode' = 'smart'
$json | ConvertTo-Json | Set-Content "$env:APPDATA\com.kua8.nectar\settings.json"
```

```python
# Python: disable dock
import json, os
path = os.path.join(os.environ['APPDATA'], 'com.kua8.nectar', 'settings.json')
with open(path) as f: settings = json.load(f)
settings['nectar-dock-enabled'] = 'false'
with open(path, 'w') as f: json.dump(settings, f)
```

## Notes

- All boolean values are strings (`"true"` / `"false"`) for consistency with `localStorage`.
- The `useSettingsSync` hook auto-converts `"true"` / `"false"` strings to booleans.
- `auto-hide` mode values in `nectar-dock-mode` and `nectar-notch-mode` are legacy aliases for `smart` — they are mapped automatically.
- Changing `nectar-scale` triggers AppBar re-registration to adjust reserved screen space.
- Theme changes (`nectar-theme-*`) are applied by reading all theme values from `localStorage` and calling `applyTheme()` — the theme system depends on all the theme keys being in sync.
