//! Lifecycle and failure-path tests against a *fake* Android SDK: stub `emulator` and `adb`
//! shell scripts. They need no emulator or hardware virtualisation, so they run in CI and
//! cover the failure cases the PRD asks for (crash during startup, startup timeout, port
//! conflicts, concurrent creation, missing image) deterministically.
//!
//! The stubs mimic only what dorker uses: the emulator stub records its pid per console port
//! and idles; the adb stub answers `getprop`, `pm path`, `exec-out screencap`, `install`,
//! `logcat` and `emu kill`, and echoes other shell commands back.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const EMULATOR: &str = r#"#!/bin/sh
port=
while [ $# -gt 0 ]; do
  [ "$1" = "-port" ] && port=$2
  shift
done
echo "fake emulator starting on port $port" >&2
if [ -n "$FAKE_EMU_CRASH" ]; then
  echo "FATAL        | fake emulator crashed: boom" >&2
  exit 1
fi
echo $$ > "$FAKE_PIDDIR/$port.pid"
trap 'exit 0' TERM INT
while :; do sleep 1; done
"#;

const ADB: &str = r#"#!/bin/sh
serial=
if [ "$1" = "-s" ]; then serial=$2; shift 2; fi
port=${serial#emulator-}
# A device exists only while its emulator stub is alive.
if [ -n "$serial" ]; then
  [ -f "$FAKE_PIDDIR/$port.pid" ] && kill -0 "$(cat "$FAKE_PIDDIR/$port.pid")" 2>/dev/null || { echo "error: device '$serial' not found" >&2; exit 1; }
fi
case "$1" in
  shell)
    shift
    case "$*" in
      "getprop sys.boot_completed") if [ -n "$FAKE_NOT_BOOTING" ]; then echo 0; else echo 1; fi ;;
      "getprop ro.build.version.sdk") echo 35 ;;
      "pm path android") echo "package:/system/framework/framework-res.apk" ;;
      sync) ;;
      "sh -c 'exit 7'") exit 7 ;;
      *) echo "fake: $*" ;;
    esac ;;
  emu)
    if [ "$2" = "kill" ] && [ -f "$FAKE_PIDDIR/$port.pid" ]; then kill "$(cat "$FAKE_PIDDIR/$port.pid")" 2>/dev/null; fi ;;
  exec-out)
    if [ "$3" = "-p" ]; then
      printf '\211PNG\r\n\032\nfake-image-data'
    else
      # Raw frame: width 2, height 2, format 1, then four RGBA pixels.
      printf '\002\000\000\000\002\000\000\000\001\000\000\000'
      blank=
      [ -n "$FAKE_BLANK" ] && blank=1
      [ -n "$FAKE_BLANK_FILE" ] && [ -f "$FAKE_BLANK_FILE" ] && blank=1
      if [ -n "$blank" ]; then
        printf '\000\000\000\377\000\000\000\377\000\000\000\377\000\000\000\377'
      else
        printf '\377\000\000\377\000\377\000\377\000\000\377\377\377\377\377\377'
      fi
    fi ;;
  install) echo "Success" ;;
  logcat) echo "fake logcat line" ;;
  version) echo "Android Debug Bridge version 1.0.41" ;;
esac
"#;

/// The tests bind and probe real TCP ports, so run them one at a time.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

struct Fake {
    root: PathBuf,
    sdk: PathBuf,
    home: PathBuf,
    piddir: PathBuf,
}

fn write_exec(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

impl Fake {
    fn new(name: &str, with_image: bool) -> Self {
        let root = std::env::temp_dir().join(format!("dorker-fake-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let sdk = root.join("sdk");
        let home = root.join("home");
        let piddir = root.join("pids");
        std::fs::create_dir_all(&piddir).unwrap();
        write_exec(&sdk.join("emulator/emulator"), EMULATOR);
        write_exec(&sdk.join("platform-tools/adb"), ADB);
        if with_image {
            let abi = if cfg!(target_arch = "aarch64") {
                "arm64-v8a"
            } else {
                "x86_64"
            };
            let img = sdk.join(format!("system-images/android-35/google_apis/{abi}"));
            std::fs::create_dir_all(&img).unwrap();
            std::fs::write(img.join("system.img"), b"x").unwrap();
        }
        Fake {
            root,
            sdk,
            home,
            piddir,
        }
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_dorker"));
        c.args(args)
            .env("DORKER_HOME", &self.home)
            .env("ANDROID_HOME", &self.sdk)
            .env("ANDROID_SDK_ROOT", &self.sdk)
            .env("FAKE_PIDDIR", &self.piddir)
            .env("DORKER_MIN_FREE_MB", "1")
            .env_remove("FAKE_EMU_CRASH")
            .env_remove("FAKE_NOT_BOOTING")
            .env_remove("FAKE_BLANK")
            .env_remove("FAKE_BLANK_FILE");
        c
    }

    fn run(&self, args: &[&str]) -> Output {
        self.cmd(args).output().unwrap()
    }

    /// Pids recorded by emulator stubs that are still alive.
    fn live_emulators(&self) -> Vec<u32> {
        let mut live = Vec::new();
        for e in std::fs::read_dir(&self.piddir).unwrap().flatten() {
            if let Ok(s) = std::fs::read_to_string(e.path()) {
                if let Ok(pid) = s.trim().parse::<u32>() {
                    let alive = Command::new("kill")
                        .args(["-0", &pid.to_string()])
                        .stderr(std::process::Stdio::null())
                        .status()
                        .map(|s| s.success())
                        .unwrap_or(false);
                    // A zombie (killed but not reaped) is dead for our purposes.
                    let zombie = Command::new("ps")
                        .args(["-o", "stat=", "-p", &pid.to_string()])
                        .output()
                        .map(|o| String::from_utf8_lossy(&o.stdout).trim().starts_with('Z'))
                        .unwrap_or(false);
                    if alive && !zombie {
                        live.push(pid);
                    }
                }
            }
        }
        live
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        for pid in self.live_emulators() {
            let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap_or(-1)
}

fn ps_json(f: &Fake) -> Vec<serde_json::Value> {
    serde_json::from_slice(&f.run(&["ps", "-a", "--json"]).stdout).unwrap()
}

fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    for _ in 0..100 {
        if cond() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    panic!("timed out waiting for: {what}");
}

#[test]
fn full_lifecycle_against_the_fake_sdk() {
    let _guard = serial();
    let f = Fake::new("lifecycle", true);
    let run = f.run(&["run", "-d", "--name", "one", "--json", "android:35"]);
    assert_eq!(code(&run), 0, "{}", text(&run.stderr));
    let inst: serde_json::Value = serde_json::from_slice(&run.stdout).unwrap();
    assert_eq!(inst["status"], "READY");
    assert!(
        inst["serial"].as_str().unwrap().starts_with("emulator-55"),
        "{inst}"
    );
    assert_eq!(f.live_emulators().len(), 1);

    let ps = ps_json(&f);
    assert_eq!(ps.len(), 1);
    assert_eq!(ps[0]["status"], "READY");

    // exec: output and exit-status propagation; --json captures both streams.
    let ex = f.run(&["exec", "one", "pm list packages"]);
    assert_eq!(text(&ex.stdout).trim(), "fake: pm list packages");
    assert_eq!(code(&f.run(&["exec", "one", "sh -c 'exit 7'"])), 7);
    let ex = f.run(&["exec", "--json", "one", "ls /sdcard"]);
    let v: serde_json::Value = serde_json::from_slice(&ex.stdout).unwrap();
    assert_eq!(v["exit_code"], 0);
    assert_eq!(v["stdout"].as_str().unwrap().trim(), "fake: ls /sdcard");

    // screenshot to a file and to stdout; logs; install.
    let png = f.root.join("shot.png");
    assert_eq!(
        code(&f.run(&["screenshot", "one", png.to_str().unwrap()])),
        0
    );
    assert!(
        std::fs::read(&png)
            .unwrap()
            .starts_with(b"\x89PNG\r\n\x1a\n")
    );
    let piped = f.run(&["screenshot", "one", "-"]);
    assert!(piped.stdout.starts_with(b"\x89PNG\r\n\x1a\n"));
    assert!(text(&f.run(&["logs", "one", "--tail", "5"]).stdout).contains("fake logcat line"));
    let apk = f.root.join("a.apk");
    std::fs::write(&apk, b"x").unwrap();
    assert_eq!(code(&f.run(&["install", "one", apk.to_str().unwrap()])), 0);

    // stop keeps the record; the emulator process is gone.
    assert_eq!(code(&f.run(&["stop", "one"])), 0);
    wait_until("emulator to exit", || f.live_emulators().is_empty());
    assert_eq!(ps_json(&f)[0]["status"], "STOPPED");
    assert_ne!(
        code(&f.run(&["exec", "one", "ls"])),
        0,
        "exec needs a READY instance"
    );

    // start again, then rm -f removes everything.
    assert_eq!(code(&f.run(&["start", "one"])), 0);
    assert_eq!(f.live_emulators().len(), 1);
    assert_eq!(code(&f.run(&["rm", "one"])), 1, "running instance needs -f");
    assert_eq!(code(&f.run(&["rm", "-f", "one"])), 0);
    wait_until("emulator to exit", || f.live_emulators().is_empty());
    assert!(ps_json(&f).is_empty());
    assert_eq!(
        std::fs::read_dir(f.home.join("instances")).unwrap().count(),
        0
    );
}

#[test]
fn emulator_crash_during_startup_is_a_clear_startup_failure() {
    let _guard = serial();
    let f = Fake::new("crash", true);
    let out = f
        .cmd(&["run", "-d", "--name", "boom", "android:35"])
        .env("FAKE_EMU_CRASH", "1")
        .output()
        .unwrap();
    assert_eq!(code(&out), 5, "{}", text(&out.stderr));
    let err = text(&out.stderr);
    assert!(err.contains("emulator exited during startup"), "{err}");
    assert!(
        err.contains("fake emulator crashed: boom"),
        "the emulator's own log is shown: {err}"
    );
    // The failed instance is kept for diagnosis, marked FAILED, with no process left.
    let ps = ps_json(&f);
    assert_eq!(ps.len(), 1);
    assert_eq!(ps[0]["status"], "FAILED");
    assert!(f.live_emulators().is_empty());
    assert_eq!(code(&f.run(&["rm", "boom"])), 0);
    assert!(ps_json(&f).is_empty());

    // With --rm the failed instance is removed automatically.
    let out = f
        .cmd(&["run", "-d", "--rm", "--name", "gone", "android:35"])
        .env("FAKE_EMU_CRASH", "1")
        .output()
        .unwrap();
    assert_eq!(code(&out), 5);
    assert!(ps_json(&f).is_empty());
}

#[test]
fn startup_timeout_kills_the_emulator_and_reports_it() {
    let _guard = serial();
    let f = Fake::new("timeout", true);
    let out = f
        .cmd(&[
            "run",
            "-d",
            "--name",
            "slow",
            "--timeout",
            "3",
            "android:35",
        ])
        .env("FAKE_NOT_BOOTING", "1")
        .output()
        .unwrap();
    assert_eq!(code(&out), 5, "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("timed out after 3s"),
        "{}",
        text(&out.stderr)
    );
    wait_until("the timed-out emulator to be killed", || {
        f.live_emulators().is_empty()
    });
    assert_eq!(ps_json(&f)[0]["status"], "FAILED");
}

#[test]
fn unavailable_port_and_name_collisions_are_conflicts() {
    let _guard = serial();
    let f = Fake::new("conflicts", true);
    // Hold a console port; asking for it must fail as a resource conflict before spawning anything.
    let held = std::net::TcpListener::bind(("127.0.0.1", 5600)).unwrap();
    let out = f.run(&["run", "-d", "--adb-port", "5600", "android:35"]);
    assert_eq!(code(&out), 8, "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("already in use"),
        "{}",
        text(&out.stderr)
    );
    assert!(f.live_emulators().is_empty());
    drop(held);

    // Without --adb-port a busy port is simply skipped.
    let busy = std::net::TcpListener::bind(("127.0.0.1", 5554)).unwrap();
    let ok = f.run(&["run", "-d", "--json", "--name", "skip", "android:35"]);
    assert_eq!(code(&ok), 0, "{}", text(&ok.stderr));
    let inst: serde_json::Value = serde_json::from_slice(&ok.stdout).unwrap();
    assert_ne!(inst["serial"], "emulator-5554", "a busy port is skipped");
    drop(busy);

    // Name collision.
    let dup = f.run(&["run", "-d", "--name", "skip", "android:35"]);
    assert_eq!(code(&dup), 8);
    assert!(text(&dup.stderr).contains("already in use"));
}

#[test]
fn concurrent_creation_gets_distinct_ports_and_one_winner_per_name() {
    let _guard = serial();
    let f = Fake::new("concurrent", true);
    // Four different names at once: all succeed with distinct console ports.
    let children: Vec<_> = (0..4)
        .map(|i| {
            f.cmd(&[
                "run",
                "-d",
                "--json",
                "--name",
                &format!("c{i}"),
                "android:35",
            ])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap()
        })
        .collect();
    let mut serials = Vec::new();
    for c in children {
        let o = c.wait_with_output().unwrap();
        assert_eq!(code(&o), 0, "{}", text(&o.stderr));
        let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
        serials.push(v["serial"].as_str().unwrap().to_string());
    }
    serials.sort();
    serials.dedup();
    assert_eq!(serials.len(), 4, "distinct ports: {serials:?}");
    assert_eq!(f.live_emulators().len(), 4);

    // The same name at once: exactly one wins.
    let children: Vec<_> = (0..3)
        .map(|_| {
            f.cmd(&["run", "-d", "--name", "same", "android:35"])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    let codes: Vec<i32> = children
        .into_iter()
        .map(|c| code(&c.wait_with_output().unwrap()))
        .collect();
    assert_eq!(
        codes.iter().filter(|c| **c == 0).count(),
        1,
        "exactly one winner: {codes:?}"
    );
    assert_eq!(
        codes.iter().filter(|c| **c == 8).count(),
        2,
        "the others conflict: {codes:?}"
    );
    assert_eq!(
        ps_json(&f).iter().filter(|i| i["name"] == "same").count(),
        1
    );

    // Stopping several instances at once works and leaves no emulators.
    let ids: Vec<String> = ps_json(&f)
        .iter()
        .map(|i| i["id"].as_str().unwrap().to_string())
        .collect();
    let mut args = vec!["rm", "-f"];
    args.extend(ids.iter().map(String::as_str));
    assert_eq!(code(&f.run(&args)), 0);
    wait_until("all emulators to exit", || f.live_emulators().is_empty());
    assert!(ps_json(&f).is_empty());
}

#[test]
fn crashed_emulator_is_detected_and_a_missing_image_points_to_pull() {
    let _guard = serial();
    let f = Fake::new("recover", true);
    assert_eq!(
        code(&f.run(&["run", "-d", "--name", "victim", "android:35"])),
        0
    );
    let pid = f.live_emulators()[0];
    // Kill the emulator behind dorker's back: the next command must see CRASHED, not READY.
    let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
    wait_until("the killed emulator to be gone", || {
        f.live_emulators().is_empty()
    });
    let ps = ps_json(&f);
    assert_eq!(ps[0]["status"], "CRASHED", "{ps:?}");
    assert_ne!(code(&f.run(&["exec", "victim", "ls"])), 0);
    assert_eq!(code(&f.run(&["rm", "victim"])), 0);

    // No matching system image: a dependency error that suggests `dorker pull`.
    let bare = Fake::new("noimage", false);
    let out = bare.run(&["run", "-d", "android:35"]);
    assert_eq!(code(&out), 3, "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("dorker pull android:35"),
        "{}",
        text(&out.stderr)
    );
}

#[test]
fn screenshot_waits_for_a_non_blank_frame_but_never_blocks_forever() {
    let _guard = serial();
    let f = Fake::new("blank", true);
    assert_eq!(code(&f.run(&["run", "-d", "--name", "s", "android:35"])), 0);
    let started = std::time::Instant::now();
    let ok = f.run(&["screenshot", "s", "-"]);
    assert!(ok.stdout.starts_with(b"\x89PNG\r\n\x1a\n"));
    assert!(
        !text(&ok.stderr).contains("still blank"),
        "a real frame is captured at once: {}",
        text(&ok.stderr)
    );
    assert!(
        started.elapsed().as_secs() < 5,
        "no waiting when the screen already has content"
    );

    // A screen that stays blank: waits the full --wait, warns, and still returns the image.
    let started = std::time::Instant::now();
    let out = f
        .cmd(&["screenshot", "s", "-", "--wait", "2"])
        .env("FAKE_BLANK", "1")
        .output()
        .unwrap();
    assert_eq!(code(&out), 0, "{}", text(&out.stderr));
    assert!(
        out.stdout.starts_with(b"\x89PNG\r\n\x1a\n"),
        "the image is still returned"
    );
    assert!(
        text(&out.stderr).contains("still blank after 2s"),
        "{}",
        text(&out.stderr)
    );
    assert!(
        started.elapsed().as_secs_f64() >= 2.0,
        "waited for the deadline"
    );

    // --wait 0 disables the check entirely.
    let started = std::time::Instant::now();
    let out = f
        .cmd(&["screenshot", "s", "-", "--wait", "0"])
        .env("FAKE_BLANK", "1")
        .output()
        .unwrap();
    assert!(out.stdout.starts_with(b"\x89PNG\r\n\x1a\n"));
    assert!(!text(&out.stderr).contains("still blank"));
    assert!(started.elapsed().as_secs_f64() < 2.0);

    // A screen that fills in after a moment: returns as soon as it does, with no warning.
    let flag = f.root.join("blank.flag");
    std::fs::write(&flag, b"").unwrap();
    let flag2 = flag.clone();
    let t = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(1500));
        let _ = std::fs::remove_file(flag2);
    });
    let started = std::time::Instant::now();
    let out = f
        .cmd(&["screenshot", "s", "-", "--wait", "8"])
        .env("FAKE_BLANK_FILE", &flag)
        .output()
        .unwrap();
    t.join().unwrap();
    assert!(out.stdout.starts_with(b"\x89PNG\r\n\x1a\n"));
    assert!(
        !text(&out.stderr).contains("still blank"),
        "{}",
        text(&out.stderr)
    );
    let secs = started.elapsed().as_secs_f64();
    assert!(
        (1.0..6.0).contains(&secs),
        "returned once the frame filled in: {secs}s"
    );
}

// ---------- dorker pull against stub package tools ----------

const ANDROID_CLI: &str = r#"#!/bin/sh
# Stub of `android --sdk=ROOT sdk install PACKAGE`. Real tools exit 0 even for unknown packages.
root=${1#--sdk=}
[ "$2" = "sdk" ] && [ "$3" = "install" ] || { echo "unexpected args: $*" >&2; exit 2; }
pkg=$4
echo "$*" > "$FAKE_PIDDIR/android-args.txt"
if [ -n "$FAKE_PULL_NOOP" ]; then
  echo "Package $(echo "$pkg" | tr ';' '/') not found."
  exit 0
fi
dir="$root/$(echo "$pkg" | tr ';' '/')"
mkdir -p "$dir" && echo x > "$dir/system.img"
"#;

const SDKMANAGER: &str = r#"#!/bin/sh
# Stub of the legacy `sdkmanager --sdk_root=ROOT --install PACKAGE`.
root=${1#--sdk_root=}
[ "$2" = "--install" ] || { echo "unexpected args: $*" >&2; exit 2; }
pkg=$3
echo "$*" > "$FAKE_PIDDIR/sdkmanager-args.txt"
dir="$root/$(echo "$pkg" | tr ';' '/')"
mkdir -p "$dir" && echo x > "$dir/system.img"
"#;

fn host_abi() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "arm64-v8a"
    } else {
        "x86_64"
    }
}

#[test]
fn pull_installs_with_the_android_cli_and_verifies_the_result() {
    let _guard = serial();
    let f = Fake::new("pull-android", false);
    write_exec(&f.sdk.join("cmdline-tools/latest/bin/android"), ANDROID_CLI);
    write_exec(
        &f.sdk.join("cmdline-tools/latest/bin/sdkmanager"),
        SDKMANAGER,
    );

    let out = f.run(&["pull", "android:35"]);
    assert_eq!(code(&out), 0, "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("android sdk install"),
        "names the tool it used: {}",
        text(&out.stderr)
    );
    // Google's CLI accepts the SDK licence without asking, so dorker must say so first.
    assert!(
        text(&out.stderr).contains("accepts the Android SDK licences on your behalf"),
        "{}",
        text(&out.stderr)
    );
    // The new CLI was used, with the SDK root and the SDK package id; the deprecated tool was not.
    let args = std::fs::read_to_string(f.piddir.join("android-args.txt")).unwrap();
    assert!(
        args.contains(&format!(
            "sdk install system-images;android-35;google_apis;{}",
            host_abi()
        )),
        "{args}"
    );
    assert!(
        args.contains(&format!("--sdk={}", f.sdk.display())),
        "{args}"
    );
    assert!(
        !f.piddir.join("sdkmanager-args.txt").exists(),
        "sdkmanager must not be used when `android` exists"
    );

    // The image is now visible, so a second pull needs no tool at all.
    let again = f.run(&["pull", "android:35"]);
    assert_eq!(code(&again), 0);
    assert!(
        text(&again.stdout).contains("already installed"),
        "{}",
        text(&again.stdout)
    );
}

#[test]
fn pull_does_not_trust_a_tool_that_exits_zero_without_installing() {
    let _guard = serial();
    let f = Fake::new("pull-noop", false);
    write_exec(&f.sdk.join("cmdline-tools/latest/bin/android"), ANDROID_CLI);
    let out = f
        .cmd(&["pull", "android:99"])
        .env("FAKE_PULL_NOOP", "1")
        .output()
        .unwrap();
    assert_eq!(code(&out), 3, "{}", text(&out.stderr));
    let err = text(&out.stderr);
    assert!(err.contains("is not installed after"), "{err}");
    assert!(
        err.contains("android sdk list --all"),
        "points at the replacement tool: {err}"
    );
    assert!(
        !err.contains("sdkmanager"),
        "no advice to use the deprecated tool: {err}"
    );
}

#[test]
fn pull_falls_back_to_sdkmanager_on_older_command_line_tools() {
    let _guard = serial();
    let f = Fake::new("pull-legacy", false);
    write_exec(
        &f.sdk.join("cmdline-tools/latest/bin/sdkmanager"),
        SDKMANAGER,
    );
    let out = f.run(&["pull", "android:35"]);
    assert_eq!(code(&out), 0, "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("sdkmanager"),
        "{}",
        text(&out.stderr)
    );
    let args = std::fs::read_to_string(f.piddir.join("sdkmanager-args.txt")).unwrap();
    assert!(
        args.contains("--install system-images;android-35;google_apis;"),
        "{args}"
    );
}

#[test]
fn pull_without_any_package_tool_explains_how_to_get_one() {
    let _guard = serial();
    let f = Fake::new("pull-none", false);
    let out = f.run(&["pull", "android:35"]);
    assert_eq!(code(&out), 3, "{}", text(&out.stderr));
    let err = text(&out.stderr);
    assert!(err.contains("android-commandlinetools"), "{err}");
    assert!(err.contains("`android` and `sdkmanager`"), "{err}");
}

#[test]
fn missing_image_errors_recommend_dorker_pull_and_the_new_cli() {
    let _guard = serial();
    let bare = Fake::new("hint", false);
    let out = bare.run(&["run", "-d", "android:35"]);
    assert_eq!(code(&out), 3);
    let err = text(&out.stderr);
    assert!(err.contains("dorker pull android:35"), "{err}");
    assert!(err.contains("android sdk install"), "{err}");
    assert!(!err.contains("sdkmanager"), "{err}");
}

#[test]
fn pull_stays_quiet_about_the_licence_once_it_has_been_accepted() {
    let _guard = serial();
    let f = Fake::new("pull-licensed", false);
    write_exec(&f.sdk.join("cmdline-tools/latest/bin/android"), ANDROID_CLI);
    std::fs::create_dir_all(f.sdk.join("licenses")).unwrap();
    std::fs::write(
        f.sdk.join("licenses/android-sdk-license"),
        b"accepted-earlier",
    )
    .unwrap();
    let out = f.run(&["pull", "android:35"]);
    assert_eq!(code(&out), 0, "{}", text(&out.stderr));
    assert!(
        !text(&out.stderr).contains("on your behalf"),
        "{}",
        text(&out.stderr)
    );
}

#[test]
fn legacy_sdkmanager_is_not_given_the_licence_notice() {
    let _guard = serial();
    let f = Fake::new("pull-legacy-lic", false);
    write_exec(
        &f.sdk.join("cmdline-tools/latest/bin/sdkmanager"),
        SDKMANAGER,
    );
    let out = f.run(&["pull", "android:35"]);
    assert_eq!(code(&out), 0, "{}", text(&out.stderr));
    assert!(
        !text(&out.stderr).contains("on your behalf"),
        "the legacy tool prompts for licences itself"
    );
}
