# WaveKeys

<p>
  <img src="src/wavekeys/assets/waveinput.svg" alt="WaveInput logo" width="96">
</p>

WaveKeys is a lightweight Windows tray application that routes the system media keys to Elgato Wave Link. It can adjust the channel assigned to the focused application or control an entire Wave Link mix.
 
## Features

- Global volume up, volume down, and mute shortcuts
- Focused-app and full-mix control modes
- Personal, Stream, and Chat mix selection
- Configurable volume step size
- Native-style volume overlay on the active monitor
- Configurable overlay scale, icon size, text size, font, and hide delay
- Per-user Windows autostart
- Automatic Wave Link discovery and reconnect

## Requirements

- Elgato Wave Link installed and running
- Applications assigned to Wave Link channels when using focused-app mode

## Installation

1. Download the Windows archive from the latest GitHub release.
2. Extract to a permanent location.
3. Start the application. WaveKeys remains available from the system tray.
4. Optionally enable **Start with Windows** in the settings window.

The media keys work globally and do not require the settings window or another specific application to have focus.

## Settings

Open the settings window from the tray icon. Changes are saved automatically to:

```text
%APPDATA%\WaveKeys\config.toml
```

Runtime logs are stored below:

```text
%APPDATA%\WaveKeys\logs
```

## Command line

The graphical application starts when `WaveKeys.exe` is launched without arguments. The same executable also provides maintenance and diagnostic commands:

```text
wavekeys init
wavekeys serve
wavekeys status
wavekeys adjust --delta 3 [--mix monitor|stream]
wavekeys mute
wavekeys pause
wavekeys resume
wavekeys autostart enable|disable
wavekeys inspect
wavekeys listen
```

## Build from source

Install the stable Rust toolchain, then run:

```
cargo build --release --locked -p wavekeys
```

The executable is written to `target\release\wavekeys.exe`.

Run the complete local verification suite with:

```
scripts\Verify.bat Full
```

This checks formatting, runs Clippy with warnings treated as errors, executes all workspace tests, and creates a release build.

## Acknowledgements

Shoutout to [Agash/WaveLink.Client](https://github.com/Agash/WaveLink.Client). This Wave Link implementation was used as a reference while developing WaveKeys.

## License

Licensed under the [MIT License](LICENSE).
