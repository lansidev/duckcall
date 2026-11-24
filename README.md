<p align="center">
  <img src="assets/logo.svg" width="180" alt="duckcall logo: a duck wearing headphones">
</p>

# duckcall

Dictate without the call talking over you. While [Handy](https://github.com/cjpais/Handy) records (hold ⌥Space),

- the call app (Fluxer or Discord) mutes your microphone through its own "push to mute" on the same key,
- duckcall lowers the system volume to 5 and restores it afterwards.

That is macOS. On Linux duckcall mutes the call itself, without any setup in the call app: see [Linux](#linux).

## Installation on macOS

```sh
git clone https://github.com/lansidev/duckcall
cargo install --path duckcall --root ~/.local
duckcall install
```

`--root ~/.local` puts the binary into `~/.local/bin`. `duckcall install` then sets everything up:

| What | Where | Why |
|---|---|---|
| Handy: `keyboard_implementation = tauri` | `settings_store.json` | Handy's default (`handy_keys`) swallows the key press, so Fluxer would never see it. |
| Handy: `log_level = debug` | `settings_store.json` | duckcall detects the start and end of a recording from Handy's DEBUG log lines. |
| Fluxer: "Push to mute" on ⌥Space | `desktop-app-store.sqlite3` | Mutes the microphone while the key is held. |
| Discord: "Push to Mute" on ⌥Space | localStorage (LevelDB) | Same as Fluxer. The previous state is copied to `leveldb.duckcall-backup` first. |
| Duckcall.app | `/Applications` | The background service. It starts `duckcall run` at login and shows up in System Settings as "Duckcall" with its icon. |

Settings that already match are left alone. If an app whose settings have to change is running,
duckcall asks before restarting it, because a restart ends active calls. `--yes` restarts without
asking; without a terminal and without `--yes` the app is skipped.

Requirements:

- Handy, Fluxer and Discord must each have been started once. A call app that is missing is skipped.
- Writing to `/Applications` needs an administrator account. `DUCKCALL_APPLICATIONS=~/Applications duckcall install` installs the app for you alone.
- Fluxer and Discord need the macOS "Input Monitoring" permission (System Settings → Privacy & Security); without it they only see keys in their own window. They pick it up after a restart.
- In both apps "push to mute" only works in voice-activity mode, not with push-to-talk.

## Configure and remove

```sh
DUCK_VOLUME=10 duckcall install   # another volume while dictating (0-100, default 5)
duckcall uninstall                # remove Duckcall.app, set Handy back to handy_keys
```

The push-to-mute shortcuts in Fluxer and Discord stay after `uninstall`.

After updating duckcall, run `duckcall install` again: Duckcall.app carries its own copy of the binary.

## Limits

- The whole system output is ducked, not only the call app. Per-app volume on macOS would need a
  Core Audio process tap with the audio-capture permission.
- Outputs without a volume control (HDMI) are left unchanged.
- Only Handy's default shortcut `option+space` is set up automatically. With another shortcut,
  `install` only changes Handy; you have to bind "push to mute" to it yourself.
- Fluxer and Discord do not document their settings stores. If an update changes the format,
  `install` reports an error or the app ignores the entry.
- If Handy changes the wording of its log lines (`TranscribeAction::start called` / `stop called`),
  the ducking stops working.
- Slack huddles are not supported: Slack has no push to mute and no custom shortcuts, only a mute toggle on ⌘⇧Space.

## Linux

While Handy records,

- duckcall mutes the microphone streams of Vesktop and Discord in PipeWire. The call receives silence,
  Handy's own recording stays live.
- duckcall lowers the default output to 5 and restores both afterwards.
- WirePlumber's memory of stream volumes and mutes (`node.stream.restore-props`) is off meanwhile. It would
  otherwise mute the call app's next microphones as well when a dictation is cut off before the restore.

Neither the call app nor Handy needs any setup, so there is no `install`: `duckcall run` is the whole
program. Run it as a user service of the graphical session, for example
`~/.config/systemd/user/duckcall.service`:

```ini
[Unit]
Description=duckcall: mute the call while Handy records
PartOf=graphical-session.target
After=graphical-session.target pipewire.service wireplumber.service

[Service]
ExecStart=%h/.local/bin/duckcall run
Environment=DUCK_VOLUME=5
Restart=on-failure

[Install]
WantedBy=graphical-session.target
```

```sh
cargo install --path duckcall --root ~/.local
systemctl --user enable --now duckcall
```

Requirements:

- PipeWire, with `pw-dump` and WirePlumber's `wpctl` on the service's `PATH`.
- Handy's log level at `debug`, its default. duckcall follows `~/.local/share/com.pais.handy/logs/handy.log`.
- On Wayland, Handy's own shortcut only works while an X11 window has the focus. Bind
  `handy --toggle-transcription` in the compositor instead (Handy's README shows how per desktop).

Limits:

- A call app is recognized by `vesktop` or `discord` in its command line or in one of its parent
  processes' (Electron records in a child process). Other call apps keep their microphone.
- A microphone stream the call app opens during a dictation stays live.
- The call app does not show the mute; it only receives silence.
- The whole default output is ducked, as on macOS.

## Development

```sh
cargo test                  # unit tests and end-to-end tests in a sandbox; the real apps and volume stay untouched
cargo clippy --all-targets
```

[AGENTS.md](AGENTS.md) explains how the pieces fit together and what to watch out for.
