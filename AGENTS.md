# AGENTS.md

Notes for LLM agents working on duckcall.

## What this is

A small Rust CLI for one workflow: the user dictates to an LLM with [Handy](https://github.com/cjpais/Handy)
while sitting in a voice call (Fluxer or Discord). While Handy records,

- the call app mutes the microphone itself, through its "push to mute" shortcut on Handy's key (Option+Space),
- `duckcall run` lowers the system output volume, so the call stays out of the transcript, and restores it afterwards.

On macOS that is all of it. On Linux the call apps cannot see Handy's key under Wayland, so `run` mutes the call
app's microphone stream in PipeWire itself, and there is no `install`. Everything platform-specific lives in
`src/macos.rs` and `src/linux.rs`; the macOS-only commands are behind `cfg` in `main.rs`.

## How it works

- **Trigger.** `run` follows Handy's log (`~/Library/Logs/com.pais.handy/handy.log`) and reacts to
  `TranscribeAction::start called`, `TranscribeAction::stop called` and `Operation cancellation completed`.
  These are DEBUG lines, so Handy's `log_level` must be `debug`.
- **Mute (macOS).** duckcall mutes nothing. `install` writes a push-to-mute keybind into the call app's settings store.
- **Why Handy is switched to `keyboard_implementation = tauri`.** Handy's macOS default, `handy_keys`, runs an
  active event tap and deletes the shortcut's key event. Fluxer only listens and never sees the key. With `tauri`,
  Handy registers a system hotkey and the event stays visible to other listeners.
- **Service.** `install` builds `/Applications/Duckcall.app` around a copy of the executable and starts the app
  once with `register`. The app carries its launch agent (`Contents/Library/LaunchAgents/<label>.plist`: its own
  executable with `run`) and registers it through `SMAppService`. That is what makes macOS file the service under
  an app, so that System Settings lists "Duckcall" with the logo. A plain LaunchAgent of an unsigned developer is
  listed by the name of its executable and without an icon, also when the executable sits in an app, and
  `AssociatedBundleIdentifiers` is ignored without a Team ID.
- **The agent's label changes with the app's code** (`label` in `macos.rs`). The app is signed ad hoc, so macOS
  pins a registration to the exact code and remembers that for the label after `unregister`. Changed code under a
  known label gives a job that never starts: `last exit code = 78: EX_CONFIG`, `job state = spawn failed`. So
  `install` unregisters, rebuilds the app, and registers it under the new label.
- **Linux.** `run` is the only command. It follows `$XDG_DATA_HOME/com.pais.handy/logs/handy.log` (Tauri's log
  directory; Handy's `log_level` already defaults to `debug`). On start it mutes every unmuted
  `Stream/Input/Audio` node of a call app with `wpctl set-mute`, and ducks `@DEFAULT_AUDIO_SINK@`; on stop it
  unmutes exactly those nodes and restores the volume. While it holds a mute, WirePlumber's stream memory
  (`node.stream.restore-props`) is off (`wpctl settings`, not saved), unless the user had switched it off: WirePlumber
  stores a stream's mute per app and applies it to the app's next stream, so a mute that was never undone (the call
  app closed during the dictation, duckcall killed) would mute every later call.
  A stream belongs to a call app when `vesktop` or `discord`
  is in the command line of its `application.process.id` or of a parent process: Electron records in a child
  process that may call itself "Chromium". Matching on Handy instead (mute every other input) would also mute
  screen-share audio, OBS and Sunshine's capture of the output.
  Only PulseAudio clients (Chromium and Electron are) carry `application.process.id` on the stream
  node; a native PipeWire client such as `pw-record` has it on its client object only and is never muted. Check by
  hand with `parecord` from a parent whose command line says `vesktop`.
- **The service on Linux** is not duckcall's business: a systemd user service of `graphical-session.target`
  runs `duckcall run`, written by hand or by the system configuration (NixOS).

## Code map

| File | Holds |
|---|---|
| `src/main.rs` | CLI; on macOS `install` / `uninstall` and consent before restarting apps (`apply`, `confirm`) |
| `src/duck.rs` | `run`: `Tail` (a polling `tail -F`) and `Duck` (remembers what `platform::duck` changed) |
| `src/apps.rs` | Reading and writing the settings of Handy, Fluxer and Discord (macOS only) |
| `src/macos.rs` | Paths, volume (`osascript`), process control, Duckcall.app and its registration; imported as `platform` |
| `src/linux.rs` | Handy's log path, microphone mute and volume through `pw-dump` and `wpctl`; imported as `platform` |
| `tests/cli.rs` | macOS end-to-end tests: the real binary in a sandbox (`Sandbox`) |
| `tests/linux.rs` | Linux end-to-end tests of `run`, with its own `Sandbox` |
| `assets/logo.svg` | The logo, shown in the README |
| `assets/duckcall.icns` | The app bundle's icon, compiled into the binary. After changing the logo, run `assets/icns.sh` |

## Rules

- **Never write an app's store while the app is running.** It would not pick the change up and may overwrite it.
  Stop the app, write, start it again; `apply` in `main.rs` does this.
- **Never restart an app without consent.** A restart ends the user's active call. `confirm` asks, `--yes` skips
  the question, and without a terminal the app is skipped and the exit code is 1.
- **Never open Discord's LevelDB in place just to read it.** Opening replays the log and rewrites the manifest.
  `discord_read` works on a copy. Before writing, `discord_configure` copies the directory to
  `leveldb.duckcall-backup`, because that LevelDB also holds the Discord login.
- **Do not test against the real apps or stores.** Unit tests build their own stores, and `tests/cli.rs` runs the
  binary in a sandbox: `HOME` and `DUCKCALL_APPLICATIONS` are scratch directories and the only `PATH` entry holds
  stand-ins for `launchctl`, `pgrep`, `pkill`, `open`, `osascript` and `codesign`. The stand-in for `open` is what
  keeps a test from registering a real service. If you copy a real Fluxer or Discord store for an experiment, it
  contains login tokens: delete it afterwards and keep it out of the repository. `tests/linux.rs` does the same
  with stand-ins for `pw-dump` and `wpctl`; only the processes behind the streams are real, because duckcall reads
  their command lines from `/proc`.
- **Assume the user is in a live call.** Do not play sound, do not restart Handy, Fluxer or Discord, and change
  the real output volume only for a short check that you announce.
- **Keep the simplifications marked.** Comments starting with `ponytail:` name a deliberate shortcut and its
  upgrade path (system-wide ducking, polling, only `option+space`). Update the comment when you lift one.
- **Keep dependencies few.** `libc`, `serde_json`, and on macOS `rusqlite` (bundled SQLite) and `rusty-leveldb`;
  the command line is parsed by hand.

## The settings stores

None of these formats is documented; they were read from the apps' data and client code. Preserve every field
you do not understand, and fail with a message instead of guessing when something looks different.

| App | Store | Entry |
|---|---|---|
| Handy | `~/Library/Application Support/com.pais.handy/settings_store.json` | `settings.keyboard_implementation`, `settings.log_level`; the shortcut is `settings.bindings.transcribe.current_binding` |
| Fluxer | `~/Library/Application Support/fluxer/desktop-app-store.sqlite3`, table `scoped_storage` | row `store='app'`, `scope='global'`, `entry_key='Keybind'`: JSON with `customKeybinds` |
| Discord | `~/Library/Application Support/discord/Local Storage/leveldb` (Chromium localStorage) | key `_https://discord.com\x00\x01keybinds`: JSON with `_state` |

- Fluxer persists its row with mobx-persist-store and discards it when `__mps__.version` is not the version the
  client expects. So duckcall only extends an existing row and never creates one.
- Discord values start with a marker byte: `0` = UTF-16LE, `1` = Latin-1. Keybind ids must be numeric strings equal
  to their key in `_state`; a new one is the highest id + 1. A shortcut is a list of
  `[device, key code, environment]`: on macOS the key codes are USB HID usages and the environment is `2`
  (`DISCORD_OPTION_SPACE` in `macos.rs`). Other systems use other key codes and another environment value.
- Push to mute only takes effect in voice-activity mode, in both apps.
- Only Handy's default shortcut `option+space` is translated into the call apps' formats.
- Both call apps need the macOS "Input Monitoring" permission to see keys outside their own window, and they only
  pick it up after a restart. Accessibility is not needed (checked with Discord 0.0.422).
- Discord reads the keyboard through IOHIDManager (`discord_utils.node`). Synthetic key events (`CGEvent`) should
  never reach it, so plan on a real key press when checking its push to mute. Handy does react to synthetic events.

## Tests

```sh
cargo test                  # everything below; the real apps, service and volume stay untouched
cargo clippy --all-targets  # must stay free of warnings
```

Every change comes with a test for the behaviour it changes.

| Where | Covers |
|---|---|
| `src/duck.rs` | `DUCK_VOLUME` parsing, the ducking rules (`Duck`), following a log (`Tail`) |
| `src/apps.rs` | Per app: what counts as set up, what gets written, what is refused and left as it was |
| `src/macos.rs` | Nothing of its own: the app and its plists are checked through `tests/cli.rs` |
| `tests/cli.rs` | macOS: `install`, `uninstall` and `run` end to end, through the real binary in the sandbox |
| `tests/linux.rs` | Linux: `run` mutes only call apps' microphones, ducks, undoes both on stop and on SIGTERM, and keeps WirePlumber's stream memory off meanwhile |

- For new end-to-end behaviour, add a fixture or a stand-in to `Sandbox`. A stand-in must never fall through to
  the real tool; that is why the sandbox `PATH` contains nothing else.
- Tests of `run` never sleep and hope. They write a log line until the service has reacted (`until`); the two
  `run_…` tests show the pattern.
- Test names say what must hold (`install_never_restarts_a_running_app_without_consent`). Assert effects (stores,
  calls of the stand-ins, exit codes) rather than the wording of messages.
- After changing the LevelDB code, also modify a copy of a real Discord store and read it with a second LevelDB
  implementation (for example Node's `classic-level`): every key must still be there, with only `keybinds` changed.
- `register` and `unregister`, the `SMAppService` calls in `app_service`, have no test: they only work from inside
  the installed app and change the real system. After touching them, `label` or the layout of the app, install
  for real twice with different `DUCK_VOLUME` values (`cargo install --path . --root ~/.local`, then
  `duckcall install`) and check both times that `launchctl print gui/$(id -u)/<label>` says `state = running`.
  The label is the name of the plist in `/Applications/Duckcall.app/Contents/Library/LaunchAgents`.
- What macOS has on record is readable without root: `plutil -p` on the newest
  `/private/var/db/com.apple.backgroundtaskmanagement/BackgroundItems-v*.btm`. Duckcall must be there as an app
  with the agent in its container, not as a `legacy agent` under `Unknown Developer`.
- `HANDY_LOG=<file> duckcall run` follows another file; appending the trigger lines to it exercises `run`
  without Handy. Outside the sandbox this changes the real output volume, and on Linux mutes the real call.
- The macOS side builds and lints from Linux: with `rustup target add aarch64-apple-darwin` and `zig`, point
  `CC_aarch64_apple_darwin` at a script that drops `--target=*` and runs `zig cc -target aarch64-macos`, then
  `cargo clippy --target aarch64-apple-darwin --all-targets`. Linking and the tests need a Mac.

## Slack huddles: looked into, not built

Checked with Slack 4.53 in a live huddle:

- Slack has no push to mute and no custom shortcuts. Its only handle is a mute toggle on Cmd+Shift+Space, which
  Slack's shell registers as a system-wide shortcut.
- A synthetic Cmd+Shift+Space toggles the mute from any app, but the huddle window comes to the front. The
  frontmost app stays the same, so a check of the frontmost app misses that.
- A toggle needs the current state. The microphone button in the huddle window has it (a class starting with
  `buttonToggledOn` while unmuted) and is readable through the Accessibility API, but only while that window is on
  the current desktop.
- Pressing that button through the Accessibility API toggles the mute too. The user saw it switch, while the class
  read back unchanged for 2.6 s: do not rely on reading the state right after a press.
- A running huddle is visible without Accessibility: Slack's helper process captures audio (Core Audio,
  `kAudioProcessPropertyIsRunningInput`), also while muted.
- Building this takes the Accessibility permission for Duckcall.app and remote control of a UI that changes with
  Slack updates.

## Conventions

- Documentation, comments and command-line output are in English.
- Commit messages follow Conventional Commits in the imperative (`feat: add …`, `fix(discord): …`).
