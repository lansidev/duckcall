//! Everything macOS-specific. `linux.rs` provides the items `run` needs: `handy_log`, `duck`, `restore`.

use crate::Result;
use crate::apps::App;
use std::ffi::{CStr, CString, c_char, c_void};
use std::fs;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

/// Handy's keyboard implementation on macOS before duckcall changes it.
pub const HANDY_KEYBOARD: &str = "handy_keys";

/// Left Option + Space the way Discord stores a shortcut: [keyboard, USB HID usage, macOS].
pub const DISCORD_OPTION_SPACE: [[u32; 3]; 2] = [[0, 226, 2], [0, 44, 2]];

/// The bundle identifier of Duckcall.app and the start of its agent's label.
const ID: &str = "io.github.lansidev.duckcall";

fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}

/// Where the app keeps the settings duckcall changes.
pub fn store(app: App) -> PathBuf {
    home().join("Library/Application Support").join(match app {
        App::Handy => "com.pais.handy/settings_store.json",
        App::Fluxer => "fluxer/desktop-app-store.sqlite3",
        App::Discord => "discord/Local Storage/leveldb",
    })
}

pub fn handy_log() -> PathBuf {
    std::env::var_os("HANDY_LOG").map_or_else(|| home().join("Library/Logs/com.pais.handy/handy.log"), PathBuf::from)
}

// --- output volume -------------------------------------------------------------------------------
// ponytail: ducks the whole system output, not only the call app, and every change spawns
// osascript (~70 ms). Per-app volume needs a Core Audio process tap (audio-capture permission).

/// Lowers the output volume to `level` and returns what it was before.
/// `None` if the output has no volume control (HDMI): there is nothing to restore then.
pub fn duck(level: u8) -> Option<u8> {
    let output = Command::new("osascript")
        .args(["-e", "set v to output volume of (get volume settings)"])
        .args(["-e", &format!("if v is not missing value and v > {level} then set volume output volume {level}")])
        .args(["-e", "v"])
        .output()
        .ok()?;
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}

pub fn restore(before: Option<u8>) {
    if let Some(volume) = before {
        let _ = Command::new("osascript").args(["-e", &format!("set volume output volume {volume}")]).status();
    }
}

// --- apps ----------------------------------------------------------------------------------------

fn process(app: App) -> &'static str {
    match app {
        App::Handy => "handy",
        App::Fluxer => "Fluxer",
        App::Discord => "Discord",
    }
}

pub fn is_running(app: App) -> bool {
    Command::new("pgrep").args(["-x", process(app)]).stdout(Stdio::null()).status().is_ok_and(|status| status.success())
}

/// Asks the app to quit and waits until it is gone.
pub fn stop(app: App) -> Result<()> {
    Command::new("pkill").args(["-x", process(app)]).status()?;
    for _ in 0..150 {
        if !is_running(app) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(format!("{} did not quit", app.name()).into())
}

pub fn start(app: App) -> Result<()> {
    Command::new("open").args(["-g", "-a", app.name()]).status()?;
    Ok(())
}

// --- background service: an app in /Applications that registers its own agent --------------------

/// The icon of the app, built from `assets/logo.svg` by `assets/icns.sh`.
const ICON: &[u8] = include_bytes!("../assets/duckcall.icns");

/// The service is an app, Duckcall.app, that carries its launch agent inside and registers it
/// through `SMAppService`. macOS then files the service under the app, and System Settings lists
/// "Duckcall" with its icon. A plain LaunchAgent of an unsigned developer is listed by the name of
/// its executable and without an icon, wherever that executable lives.
fn app() -> PathBuf {
    let applications = std::env::var_os("DUCKCALL_APPLICATIONS").map_or_else(|| PathBuf::from("/Applications"), PathBuf::from);
    applications.join("Duckcall.app")
}

fn info_plist() -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleIdentifier</key>
	<string>{ID}</string>
	<key>CFBundleName</key>
	<string>Duckcall</string>
	<key>CFBundleDisplayName</key>
	<string>Duckcall</string>
	<key>CFBundleExecutable</key>
	<string>duckcall</string>
	<key>CFBundleIconFile</key>
	<string>duckcall</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleShortVersionString</key>
	<string>{version}</string>
	<key>CFBundleVersion</key>
	<string>{version}</string>
	<key>LSUIElement</key>
	<true/>
</dict>
</plist>
"#,
        version = env!("CARGO_PKG_VERSION")
    )
}

/// The agent's label. It changes whenever the app's code does. macOS ties a registration to the
/// exact code, because the app is not signed by a developer, and it remembers that for a label
/// after the registration is taken back: changed code under the old label fails to launch with
/// EX_CONFIG, under a new label it starts at once.
fn label(code: &[u8], level: u8) -> String {
    let mut hasher = DefaultHasher::new();
    code.hash(&mut hasher);
    level.hash(&mut hasher);
    format!("{ID}.{:08x}", hasher.finish() as u32)
}

/// The agent as launchd reads it from inside the app; `BundleProgram` is relative to the app.
fn agent(label: &str, level: u8) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{label}</string>
	<key>BundleProgram</key>
	<string>Contents/MacOS/duckcall</string>
	<key>ProgramArguments</key>
	<array>
		<string>duckcall</string>
		<string>run</string>
	</array>
	<key>EnvironmentVariables</key>
	<dict>
		<key>DUCK_VOLUME</key>
		<string>{level}</string>
	</dict>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<true/>
</dict>
</plist>
"#
    )
}

/// The file name of the agent an app carries, `<label>.plist`, if it carries one.
fn agent_file(app: &Path) -> Option<String> {
    let files = fs::read_dir(app.join("Contents/Library/LaunchAgents")).ok()?;
    files.filter_map(|file| file.ok()?.file_name().into_string().ok()).find(|name| name.ends_with(".plist"))
}

/// Builds the app around a copy of this executable.
fn app_build(app: &Path, level: u8) -> Result<()> {
    let code = fs::read(std::env::current_exe()?)?; // before the old app goes: `install` may be run from it
    if app.exists() {
        fs::remove_dir_all(app)?;
    }
    let contents = app.join("Contents");
    for directory in ["MacOS", "Resources", "Library/LaunchAgents"] {
        fs::create_dir_all(contents.join(directory)).map_err(|error| {
            format!("cannot write {}: {error}. DUCKCALL_APPLICATIONS=~/Applications installs it for you alone", app.display())
        })?;
    }
    fs::write(contents.join("Info.plist"), info_plist())?;
    fs::write(contents.join("Resources/duckcall.icns"), ICON)?;
    let label = label(&code, level);
    fs::write(contents.join("Library/LaunchAgents").join(format!("{label}.plist")), agent(&label, level))?;
    let exe = contents.join("MacOS/duckcall");
    fs::write(&exe, code)?;
    fs::set_permissions(&exe, fs::Permissions::from_mode(0o755))?;
    // The plists and the icon belong to the app's signature, so sign it as a whole.
    let signed = Command::new("codesign")
        .args(["--force", "--sign", "-"])
        .arg(app)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?
        .success();
    if !signed {
        return Err("codesign could not sign Duckcall.app".into());
    }
    Ok(())
}

/// Runs the app with one command, the way macOS starts apps, and fails with what it wrote to
/// stderr. `open` is also the seam for the tests: replacing it keeps them from registering a
/// real service.
fn app_run(app: &Path, command: &str) -> Result<()> {
    let errors = std::env::temp_dir().join(format!("duckcall-{command}-{}.txt", std::process::id()));
    let started = Command::new("open")
        .args(["-n", "-W", "-g", "-a"])
        .arg(app)
        .arg("--stderr")
        .arg(&errors)
        .args(["--args", command])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?
        .success();
    let message = fs::read_to_string(&errors).unwrap_or_default();
    fs::remove_file(&errors).ok();
    if !started {
        return Err(format!("could not start {}", app.display()).into());
    }
    match message.trim() {
        "" => Ok(()),
        message => Err(message.into()),
    }
}

fn launchctl(args: &[&str]) -> bool {
    Command::new("launchctl")
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Takes the registration of the installed app back, before the app changes or goes.
fn service_unregister(app: &Path) {
    let Some(agent) = agent_file(app) else { return };
    let _ = app_run(app, "unregister"); // usually fails because nothing is registered
    // Whatever is still loaded, also from a registration macOS has lost track of.
    let service = format!("gui/{}/{}", unsafe { libc::getuid() }, agent.trim_end_matches(".plist"));
    launchctl(&["bootout", &service]);
    for _ in 0..50 {
        if !launchctl(&["print", &service]) {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Installs Duckcall.app and has it register its agent: `duckcall run` at login, kept alive.
pub fn service_install(level: u8) -> Result<()> {
    let app = app();
    service_unregister(&app);
    app_build(&app, level)?;
    app_run(&app, "register")
}

pub fn service_remove() -> Result<()> {
    let app = app();
    service_unregister(&app);
    if app.exists() {
        fs::remove_dir_all(&app)?;
    }
    Ok(())
}

/// The app this executable sits in, which is how it runs when Duckcall.app is opened.
fn own_app() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let app = exe.parent()?.parent()?.parent()?;
    (app.extension()? == "app").then(|| app.to_path_buf())
}

pub fn in_app() -> bool {
    own_app().is_some()
}

// --- SMAppService, through the Objective-C runtime -----------------------------------------------

type Object = *mut c_void;

#[link(name = "objc")]
unsafe extern "C" {
    fn objc_getClass(name: *const c_char) -> Object;
    fn sel_registerName(name: *const c_char) -> Object;
    fn objc_msgSend();
}

// Loads the frameworks whose classes are looked up by name below.
#[link(name = "Foundation", kind = "framework")]
unsafe extern "C" {}

#[link(name = "ServiceManagement", kind = "framework")]
unsafe extern "C" {}

/// Registers the agent inside the app this executable belongs to, or takes that back.
/// It has to run from the app's own executable: that app is where macOS looks for the agent.
pub fn app_service(register: bool) -> Result<()> {
    let agent = own_app().and_then(|app| agent_file(&app)).ok_or("this only works inside Duckcall.app; use `duckcall install`")?;
    let agent = CString::new(agent)?;
    // objc_msgSend has no signature of its own; each call needs the one of the method it reaches.
    let send = objc_msgSend as *const ();
    unsafe {
        let with_text: extern "C" fn(Object, Object, *const c_char) -> Object = std::mem::transmute(send);
        let with_object: extern "C" fn(Object, Object, Object) -> Object = std::mem::transmute(send);
        let with_error: extern "C" fn(Object, Object, *mut Object) -> i8 = std::mem::transmute(send);
        let plain: extern "C" fn(Object, Object) -> Object = std::mem::transmute(send);
        let selector = |name: &CStr| sel_registerName(name.as_ptr());

        let class = objc_getClass(c"SMAppService".as_ptr());
        if class.is_null() {
            return Err("the background service needs macOS 13 or later".into());
        }
        let name = with_text(objc_getClass(c"NSString".as_ptr()), selector(c"stringWithUTF8String:"), agent.as_ptr());
        let service = with_object(class, selector(c"agentServiceWithPlistName:"), name);
        let action = if register { c"registerAndReturnError:" } else { c"unregisterAndReturnError:" };
        let mut error: Object = std::ptr::null_mut();
        if with_error(service, selector(action), &mut error) != 0 {
            return Ok(());
        }
        if error.is_null() {
            return Err("macOS refused without giving a reason".into());
        }
        let description = plain(plain(error, selector(c"localizedDescription")), selector(c"UTF8String"));
        Err(CStr::from_ptr(description as *const c_char).to_string_lossy().into_owned().into())
    }
}
