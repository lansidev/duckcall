//! Everything Linux-specific: Handy's log, and the call and the volume through PipeWire (`pw-dump`, `wpctl`).
//!
//! Unlike on macOS, duckcall mutes the call itself: PipeWire mutes each call app's microphone stream on
//! its own, which leaves Handy's recording alone. Nothing has to be set up in the call app or in Handy.

use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

/// The call apps whose microphone is muted while Handy records, as they appear in their command line.
const CALL_APPS: [&str; 2] = ["vesktop", "discord"];

const OUTPUT: &str = "@DEFAULT_AUDIO_SINK@";

pub fn handy_log() -> PathBuf {
    if let Some(log) = std::env::var_os("HANDY_LOG") {
        return log.into();
    }
    // Tauri's log directory on Linux: $XDG_DATA_HOME/<identifier>/logs.
    let data = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).filter(|dir| dir.is_absolute());
    let data = data.unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share"));
    data.join("com.pais.handy/logs/handy.log")
}

/// What `duck` changed, for `restore`.
pub struct Ducked {
    /// The output volume from before; `None` without a default output.
    volume: Option<f64>,
    /// The call microphones duck muted. One the user had muted already is not in here and stays muted.
    muted: Vec<u64>,
}

/// Mutes the call apps' microphones and lowers the output volume to `level`.
// ponytail: a microphone stream the call app opens during the dictation stays live; follow the
// graph with `pw-mon` if that ever matters.
pub fn duck(level: u8) -> Ducked {
    let muted = call_microphones();
    for id in &muted {
        wpctl(&["set-mute", &id.to_string(), "1"]);
    }
    // "Volume: 0.40", followed by " [MUTED]" while the output is muted.
    let volume = wpctl(&["get-volume", OUTPUT])
        .and_then(|answer| answer.strip_prefix("Volume: ")?.split_whitespace().next()?.parse::<f64>().ok());
    let level = f64::from(level) / 100.0;
    if volume.is_some_and(|volume| volume > level) {
        wpctl(&["set-volume", OUTPUT, &level.to_string()]);
    }
    Ducked { volume, muted }
}

pub fn restore(ducked: Ducked) {
    for id in ducked.muted {
        wpctl(&["set-mute", &id.to_string(), "0"]);
    }
    if let Some(volume) = ducked.volume {
        wpctl(&["set-volume", OUTPUT, &volume.to_string()]);
    }
}

/// wpctl's answer, if it succeeded.
fn wpctl(args: &[&str]) -> Option<String> {
    let output = Command::new("wpctl").args(args).output().ok()?;
    output.status.success().then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// The PipeWire node ids of the call apps' microphone streams that are not muted.
fn call_microphones() -> Vec<u64> {
    let graph = Command::new("pw-dump").output().ok().and_then(|output| serde_json::from_slice::<Value>(&output.stdout).ok());
    let objects = graph.as_ref().and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
    objects
        .iter()
        .filter(|object| {
            let props = &object["info"]["props"];
            props["media.class"] == "Stream/Input/Audio"
                && object["info"]["params"]["Props"][0]["mute"] != true
                && props["application.process.id"].as_u64().is_some_and(in_call_app)
        })
        .filter_map(|object| object["id"].as_u64())
        .collect()
}

/// Whether the process or one of its parents is a call app. Electron apps such as Vesktop record in a
/// child process, which may tell PipeWire that it is "Chromium".
fn in_call_app(pid: u64) -> bool {
    let mut pid = pid;
    while pid > 1 {
        let Ok(cmdline) = fs::read(format!("/proc/{pid}/cmdline")) else { return false };
        let cmdline = String::from_utf8_lossy(&cmdline).to_lowercase();
        if CALL_APPS.iter().any(|app| cmdline.contains(app)) {
            return true;
        }
        let Some(parent) = parent(pid) else { return false };
        pid = parent;
    }
    false
}

fn parent(pid: u64) -> Option<u64> {
    let status = fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    status.lines().find_map(|line| line.strip_prefix("PPid:"))?.trim().parse().ok()
}
