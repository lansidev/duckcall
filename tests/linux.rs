//! Runs `duckcall run` on Linux in a sandbox. The only `PATH` entry holds stand-ins for pw-dump and wpctl,
//! so nothing here touches the real microphones or the real volume. The processes behind the microphone
//! streams are real, though: duckcall recognizes a call app by its command line in /proc.
#![cfg(target_os = "linux")]

use serde_json::{Value, json};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

const STARTED: &str = "[2026-11-17][08:40:29][handy_app_lib::actions][DEBUG] TranscribeAction::start called for binding: transcribe";
const STOPPED: &str = "[2026-11-17][08:40:31][handy_app_lib::actions][DEBUG] TranscribeAction::stop called for binding: transcribe";

struct Sandbox {
    root: PathBuf,
    /// Stand-ins for the apps behind the streams, killed when the test ends.
    apps: Vec<Child>,
}

impl Sandbox {
    fn new(name: &str) -> Sandbox {
        let root = std::env::temp_dir().join(format!("duckcall-linux-{name}"));
        fs::remove_dir_all(&root).ok();
        fs::create_dir_all(root.join("bin")).unwrap();
        let note = r#"echo "${0##*/} $*" >> "$SANDBOX/calls""#;
        let stand_in = |name: &str, script: &str| {
            let path = root.join("bin").join(name);
            fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        };
        // Like the real wpctl, get-volume fails without a default output.
        stand_in("wpctl", &format!("{note}\n[ \"$1\" != get-volume ] && exit\nread -r volume < \"$SANDBOX/volume\" && echo \"Volume: $volume\""));
        stand_in("pw-dump", r#"read -r graph < "$SANDBOX/graph.json"; printf '%s\n' "$graph""#);
        let sandbox = Sandbox { root, apps: Vec::new() };
        sandbox.volume(Some("0.40"));
        sandbox
    }

    /// What the stand-in for wpctl reports as the output volume; `None`: there is no default output.
    fn volume(&self, volume: Option<&str>) {
        let file = self.root.join("volume");
        match volume {
            Some(volume) => fs::write(file, format!("{volume}\n")).unwrap(),
            None => fs::remove_file(file).unwrap(),
        }
    }

    /// What the stand-in for pw-dump reports.
    fn graph(&self, nodes: &[Value]) {
        fs::write(self.root.join("graph.json"), serde_json::to_string(nodes).unwrap()).unwrap();
    }

    /// Starts a process whose command line is `command`, and a child of it that does not mention it,
    /// the way Electron records in a child process. Returns the pids of both.
    fn app(&mut self, command: &str) -> (u32, u32) {
        let mut parent = Command::new("sh")
            .arg0(command)
            .process_group(0) // so that the child goes with it
            .args(["-c", "sleep 600 & echo $!; wait"])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut child = String::new();
        BufReader::new(parent.stdout.take().unwrap()).read_line(&mut child).unwrap();
        let child = child.trim().parse().unwrap();
        let pids = (parent.id(), child);
        self.apps.push(parent);
        pids
    }

    /// The changes the stand-in for wpctl was asked for, in order.
    fn changes(&self) -> Vec<String> {
        let calls = fs::read_to_string(self.root.join("calls")).unwrap_or_default();
        calls.lines().filter_map(|call| call.strip_prefix("wpctl set-")).map(String::from).collect()
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        for app in &mut self.apps {
            unsafe { libc::kill(-(app.id() as libc::pid_t), libc::SIGKILL) };
            app.wait().ok();
        }
    }
}

/// A PipeWire node the way pw-dump reports it.
fn node(id: u64, class: &str, pid: u32, muted: bool) -> Value {
    json!({
        "id": id,
        "type": "PipeWire:Interface:Node",
        "info": {
            "props": { "media.class": class, "application.name": "Chromium", "application.process.id": pid },
            "params": { "Props": [{ "volume": 1.0, "mute": muted }] },
        },
    })
}

/// A `duckcall run` that is killed when the test ends, however it ends.
struct Service(Child);

impl Service {
    fn start(sandbox: &Sandbox, level: &str) -> (Service, impl Fn(&str)) {
        let log = sandbox.root.join("handy.log");
        fs::write(&log, "").unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_duckcall"))
            .arg("run")
            .env_clear()
            .env("PATH", sandbox.root.join("bin"))
            .env("SANDBOX", &sandbox.root)
            .env("HANDY_LOG", &log)
            .env("DUCK_VOLUME", level)
            .spawn()
            .unwrap();
        let handy_logs = move |line: &str| {
            writeln!(fs::OpenOptions::new().append(true).open(&log).unwrap(), "{line}").unwrap();
        };
        (Service(child), handy_logs)
    }

    /// Stops it the way systemd does.
    fn stop(mut self) -> ExitStatus {
        unsafe { libc::kill(self.0.id() as libc::pid_t, libc::SIGTERM) };
        let mut status = None;
        until("duckcall run to stop", || {
            status = self.0.try_wait().unwrap();
            status.is_some()
        });
        status.unwrap()
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        self.0.kill().ok();
        self.0.wait().ok();
    }
}

fn until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn run_mutes_only_the_call_apps_microphones_and_ducks_until_handy_stops_and_on_sigterm() {
    let mut sandbox = Sandbox::new("run");
    let (_, vesktop_audio) = sandbox.app("/nix/store/x-electron/bin/electron /nix/store/y-vesktop/app.asar");
    let (discord, _) = sandbox.app("/opt/Discord/Discord");
    let handy = std::process::id(); // a process that is no call app
    sandbox.graph(&[
        node(50, "Stream/Input/Audio", vesktop_audio, false),
        node(51, "Stream/Output/Audio", vesktop_audio, false), // what the call plays stays audible
        node(52, "Stream/Input/Audio", vesktop_audio, true),   // muted by the user, and so it stays
        node(53, "Stream/Input/Audio", handy, false),          // Handy's own recording
        node(54, "Stream/Input/Audio", discord, false),
    ]);
    let (service, handy_logs) = Service::start(&sandbox, "7");

    // A repeated line changes nothing, so each one is written until the service has reacted.
    until("the call to be muted", || {
        handy_logs(STARTED);
        sandbox.changes().len() == 3
    });
    until("the call to be unmuted", || {
        handy_logs(STOPPED);
        sandbox.changes().len() == 6
    });
    until("the call to be muted again", || {
        handy_logs(STARTED);
        sandbox.changes().len() == 9
    });
    let status = service.stop();

    assert!(status.success(), "{status}");
    let once = [
        "mute 50 1",
        "mute 54 1",
        "volume @DEFAULT_AUDIO_SINK@ 0.07",
        "mute 50 0",
        "mute 54 0",
        "volume @DEFAULT_AUDIO_SINK@ 0.4",
    ];
    assert_eq!(sandbox.changes(), [once, once].concat());
}

#[test]
fn run_unmutes_the_call_also_without_a_default_output() {
    let mut sandbox = Sandbox::new("run-no-output");
    let (_, vesktop_audio) = sandbox.app("vesktop");
    sandbox.graph(&[node(60, "Stream/Input/Audio", vesktop_audio, false)]);
    sandbox.volume(None);
    let (service, handy_logs) = Service::start(&sandbox, "5");

    until("the call to be muted", || {
        handy_logs(STARTED);
        sandbox.changes().len() == 1
    });
    until("the call to be unmuted", || {
        handy_logs(STOPPED);
        sandbox.changes().len() == 2
    });
    service.stop();

    assert_eq!(sandbox.changes(), ["mute 60 1", "mute 60 0"]);
}

#[test]
fn an_unknown_command_prints_the_usage_and_changes_nothing() {
    let sandbox = Sandbox::new("usage");

    let output = Command::new(env!("CARGO_BIN_EXE_duckcall"))
        .arg("install")
        .env_clear()
        .env("PATH", sandbox.root.join("bin"))
        .env("SANDBOX", &sandbox.root)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("usage: duckcall run"));
    assert!(sandbox.changes().is_empty());
}
