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
if [ "$1" = "-version" ]; then
  echo "Android emulator version 99.0.0.0 (build_id 1) (CL:N/A)"
  exit 0
fi
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
            // A build reads the image's revision from its package.xml.
            std::fs::write(
                img.join("package.xml"),
                "<localPackage><revision><major>1</major></revision></localPackage>",
            )
            .unwrap();
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
            .env_remove("FAKE_BLANK_FILE")
            .env_remove("FAKE_LIST_FILE");
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
if [ "$2" = "sdk" ] && [ "$3" = "list" ]; then
  # `android sdk list --all <pattern>`: print a canned repository listing if the test gave one.
  [ -n "$FAKE_LIST_FILE" ] && [ -f "$FAKE_LIST_FILE" ] && { echo "Available packages:"; cat "$FAKE_LIST_FILE"; exit 0; }
  exit 2
fi
[ "$2" = "sdk" ] && [ "$3" = "install" ] || { echo "unexpected args: $*" >&2; exit 2; }
pkg=$4
echo "$*" > "$FAKE_PIDDIR/android-args.txt"
if [ -n "$FAKE_PULL_NOOP" ]; then
  echo "Package $(echo "$pkg" | tr ';' '/') not found."
  exit 0
fi
dir="$root/$(echo "$pkg" | tr ';' '/')"
mkdir -p "$dir" && echo x > "$dir/system.img"
printf '<localPackage><revision><major>1</major></revision></localPackage>' > "$dir/package.xml"
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
        err.contains("dorker versions"),
        "points at the command that lists what exists: {err}"
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

/// A repository listing in the format `android sdk list --all` prints, for the host ABI and the
/// other one, with the API 37 naming that broke `dorker pull` (android-37.0, ps16k flavours).
fn repo_listing(f: &Fake) -> PathBuf {
    let other = if host_abi() == "arm64-v8a" {
        "x86_64"
    } else {
        "arm64-v8a"
    };
    let mut text = String::new();
    for (path, desc) in [
        ("android-37.0/google_apis", "Google APIs"),
        ("android-37.0/google_apis_playstore", "Google Play"),
        ("android-37.0/google_atd", "Google APIs ATD"),
        (
            "android-37.1/google_apis_ps16k",
            "16 KB Page Size Google APIs",
        ),
        ("android-37.2-beta1/google_apis_ps16k", "16 KB beta"),
        ("android-CANARY/google_apis_ps16k", "canary"),
        ("android-38.1/google_apis_ps16k", "16 KB only"),
    ] {
        for abi in [host_abi(), other] {
            text.push_str(&format!(
                "  system-images/{path}/{abi}      8.0.0      {desc} System Image\n"
            ));
        }
    }
    let file = f.root.join("repo-listing.txt");
    std::fs::write(&file, text).unwrap();
    file
}

#[test]
fn pull_installs_the_published_package_name_not_a_guessed_one() {
    let _guard = serial();
    let f = Fake::new("pull-discover", false);
    write_exec(&f.sdk.join("cmdline-tools/latest/bin/android"), ANDROID_CLI);
    let listing = repo_listing(&f);

    // android:37 used to look for `android-37`, which is not published (it is `android-37.0`).
    let out = f
        .cmd(&["pull", "android:37"])
        .env("FAKE_LIST_FILE", &listing)
        .output()
        .unwrap();
    assert_eq!(code(&out), 0, "{}", text(&out.stderr));
    let abi = host_abi();
    assert!(
        text(&out.stderr).contains(&format!(
            "android:37 is published as system-images/android-37.0/google_apis/{abi}"
        )),
        "{}",
        text(&out.stderr)
    );
    let args = std::fs::read_to_string(f.piddir.join("android-args.txt")).unwrap();
    assert!(
        args.contains(&format!(
            "sdk install system-images;android-37.0;google_apis;{abi}"
        )),
        "{args}"
    );

    // ...and the installed image is then what `run android:37` uses.
    let run = f.run(&["run", "-d", "--json", "android:37"]);
    assert_eq!(code(&run), 0, "{}", text(&run.stderr));
    assert_eq!(
        code(&f.run(&["pull", "android:37"])),
        0,
        "a second pull is a no-op"
    );
}

#[test]
fn pull_picks_a_16kb_flavour_when_that_is_all_a_release_publishes() {
    let _guard = serial();
    let f = Fake::new("pull-ps16k", false);
    write_exec(&f.sdk.join("cmdline-tools/latest/bin/android"), ANDROID_CLI);
    let listing = repo_listing(&f);
    let out = f
        .cmd(&["pull", "android:37.1"])
        .env("FAKE_LIST_FILE", &listing)
        .output()
        .unwrap();
    assert_eq!(code(&out), 0, "{}", text(&out.stderr));
    let args = std::fs::read_to_string(f.piddir.join("android-args.txt")).unwrap();
    assert!(
        args.contains(&format!(
            "system-images;android-37.1;google_apis_ps16k;{}",
            host_abi()
        )),
        "{args}"
    );
    // A release with no .0 image: the only stable one is used (38.1 here).
    let out = f
        .cmd(&["pull", "android:38"])
        .env("FAKE_LIST_FILE", &listing)
        .output()
        .unwrap();
    assert_eq!(code(&out), 0, "{}", text(&out.stderr));
    assert!(
        text(&out.stderr)
            .contains("android:38 is published as system-images/android-38.1/google_apis_ps16k"),
        "{}",
        text(&out.stderr)
    );
    // Preview releases are never chosen: 37.2 is only published as a beta.
    let out = f
        .cmd(&["pull", "android:37.2"])
        .env("FAKE_LIST_FILE", &listing)
        .output()
        .unwrap();
    assert_eq!(code(&out), 3, "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("publishes no stable system image for android:37.2"),
        "{}",
        text(&out.stderr)
    );
}

#[test]
fn pull_lists_what_exists_when_nothing_matches() {
    let _guard = serial();
    let f = Fake::new("pull-unavailable", false);
    write_exec(&f.sdk.join("cmdline-tools/latest/bin/android"), ANDROID_CLI);
    let listing = repo_listing(&f);

    let out = f
        .cmd(&["pull", "android:37-nonexistent"])
        .env("FAKE_LIST_FILE", &listing)
        .output()
        .unwrap();
    assert_eq!(code(&out), 3, "{}", text(&out.stderr));
    let err = text(&out.stderr);
    assert!(
        err.contains("no system image matches android:37-nonexistent"),
        "{err}"
    );
    assert!(
        err.contains("android:37.0-google_apis "),
        "lists the real references: {err}"
    );
    assert!(err.contains("android:37.1-google_apis_ps16k"), "{err}");
    assert!(
        err.contains("Try: dorker pull android:37.0-google_apis"),
        "suggests the best one: {err}"
    );
    assert!(
        !err.contains("beta") && !err.contains("CANARY"),
        "previews are not offered: {err}"
    );
    assert!(
        !std::fs::exists(f.piddir.join("android-args.txt")).unwrap(),
        "nothing was installed"
    );

    // An API the repository does not publish at all.
    let out = f
        .cmd(&["pull", "android:99"])
        .env("FAKE_LIST_FILE", &listing)
        .output()
        .unwrap();
    assert_eq!(code(&out), 3);
    assert!(
        text(&out.stderr).contains("publishes no stable system image for android:99"),
        "{}",
        text(&out.stderr)
    );
}

#[test]
fn versions_lists_published_and_installed_versions() {
    let _guard = serial();
    let f = Fake::new("versions", false);
    write_exec(&f.sdk.join("cmdline-tools/latest/bin/android"), ANDROID_CLI);
    let listing = repo_listing(&f);
    let run = |args: &[&str]| {
        f.cmd(args)
            .env("FAKE_LIST_FILE", &listing)
            .output()
            .unwrap()
    };

    // Nothing installed yet: every release is "available", and the table is on stdout only.
    let out = run(&["versions"]);
    assert_eq!(code(&out), 0, "{}", text(&out.stderr));
    let table = text(&out.stdout);
    assert!(table.starts_with("USE "), "{table}");
    assert!(
        table.contains("android:38.1")
            && table.contains("android:37.1")
            && table.contains("android:37 "),
        "{table}"
    );
    assert!(
        !table.contains("beta") && !table.contains("CANARY") && !table.contains("37.2"),
        "previews are hidden: {table}"
    );
    assert!(!table.contains("installed "), "{table}");
    assert!(
        text(&out.stderr).contains("Reading Google's SDK repository"),
        "progress goes to stderr"
    );
    assert!(!table.contains("Reading Google"), "{table}");
    let row37 = table
        .lines()
        .find(|l| l.starts_with("android:37 "))
        .unwrap();
    assert!(row37.contains("google_apis,"), "{row37}");

    // After pulling, that release is marked installed, with the flavour starred.
    assert_eq!(code(&run(&["pull", "android:37"])), 0);
    let out = run(&["versions", "37"]);
    let table = text(&out.stdout);
    assert!(
        table.lines().any(|l| l.starts_with("android:37 ")
            && l.contains("installed")
            && l.contains("google_apis*")),
        "{table}"
    );
    assert!(
        table
            .lines()
            .any(|l| l.starts_with("android:37.1") && l.contains("available")),
        "{table}"
    );
    assert!(
        !table.contains("android:38"),
        "an API filter narrows the table: {table}"
    );

    // JSON: machine-readable, never styled, with the command to run.
    let out = f
        .cmd(&["versions", "--json"])
        .env("FAKE_LIST_FILE", &listing)
        .env("CLICOLOR_FORCE", "1")
        .output()
        .unwrap();
    assert!(!text(&out.stdout).contains('\x1b'));
    assert!(
        !text(&out.stderr).contains("Reading Google"),
        "no progress chatter in JSON mode"
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["repository_read"], true);
    assert_eq!(v["abi"], host_abi());
    let r37 = v["versions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["use"] == "android:37")
        .unwrap();
    assert_eq!(r37["installed"], true);
    assert_eq!(r37["pull"], "dorker pull android:37");
    assert!(
        r37["flavours"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["name"] == "google_apis" && x["installed"] == true)
    );
}

#[test]
fn versions_still_works_when_the_repository_cannot_be_read() {
    let _guard = serial();
    let f = Fake::new("versions-offline", true); // the fake SDK has android-35 installed
    // No FAKE_LIST_FILE: the stub's `sdk list` fails, like an offline machine.
    write_exec(&f.sdk.join("cmdline-tools/latest/bin/android"), ANDROID_CLI);
    let out = f.run(&["versions"]);
    assert_eq!(code(&out), 0, "{}", text(&out.stderr));
    let table = text(&out.stdout);
    assert!(
        table
            .lines()
            .any(|l| l.starts_with("android:35") && l.contains("installed")),
        "{table}"
    );
    assert!(table.contains("only installed images are shown"), "{table}");
    let json: serde_json::Value =
        serde_json::from_slice(&f.run(&["versions", "--json"]).stdout).unwrap();
    assert_eq!(json["repository_read"], false);
}

#[test]
fn pull_errors_point_at_dorker_versions() {
    let _guard = serial();
    let f = Fake::new("pull-hint-versions", false);
    write_exec(&f.sdk.join("cmdline-tools/latest/bin/android"), ANDROID_CLI);
    let listing = repo_listing(&f);
    let out = f
        .cmd(&["pull", "android:37-nonexistent"])
        .env("FAKE_LIST_FILE", &listing)
        .output()
        .unwrap();
    assert!(
        text(&out.stderr).contains("dorker versions"),
        "{}",
        text(&out.stderr)
    );
    let out = f
        .cmd(&["pull", "android:99"])
        .env("FAKE_LIST_FILE", &listing)
        .output()
        .unwrap();
    assert!(
        text(&out.stderr).contains("dorker versions"),
        "{}",
        text(&out.stderr)
    );
}

// ---------- automatic pulling of a missing system image ----------

#[test]
fn run_installs_a_missing_image_only_when_told_to() {
    let _guard = serial();
    let f = Fake::new("autopull-run", false);
    write_exec(&f.sdk.join("cmdline-tools/latest/bin/android"), ANDROID_CLI);
    let listing = repo_listing(&f);
    let with = |args: &[&str], env: &[(&str, &str)]| {
        let mut c = f.cmd(args);
        c.env("FAKE_LIST_FILE", &listing);
        for (k, v) in env {
            c.env(k, v);
        }
        c.output().unwrap()
    };

    // Not a terminal (CI, scripts, agents): fail as before, and say how to opt in. Nothing is
    // downloaded or licence-accepted behind the user's back.
    let out = with(&["run", "-d", "android:37"], &[]);
    assert_eq!(code(&out), 3, "{}", text(&out.stderr));
    let err = text(&out.stderr);
    assert!(err.contains("dorker pull android:37"), "{err}");
    assert!(
        err.contains("--pull") && err.contains("DORKER_PULL=1"),
        "{err}"
    );
    assert!(
        !f.piddir.join("android-args.txt").exists(),
        "nothing was installed"
    );

    // --pull: install it, then carry on and start the instance.
    let out = with(
        &[
            "run",
            "-d",
            "--pull",
            "--json",
            "--name",
            "p1",
            "android:37",
        ],
        &[],
    );
    assert_eq!(code(&out), 0, "{}", text(&out.stderr));
    let inst: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(inst["status"], "READY");
    let args = std::fs::read_to_string(f.piddir.join("android-args.txt")).unwrap();
    assert!(
        args.contains(&format!(
            "sdk install system-images;android-37.0;google_apis;{}",
            host_abi()
        )),
        "{args}"
    );
    assert!(
        text(&out.stderr).contains("accepts the Android SDK licences on your behalf"),
        "the licence is still announced"
    );

    // Once installed, no flag is needed.
    let out = with(&["run", "-d", "--name", "p2", "android:37"], &[]);
    assert_eq!(code(&out), 0, "{}", text(&out.stderr));
}

#[test]
fn the_environment_variable_turns_on_pulling_for_scripts() {
    let _guard = serial();
    let f = Fake::new("autopull-env", false);
    write_exec(&f.sdk.join("cmdline-tools/latest/bin/android"), ANDROID_CLI);
    let listing = repo_listing(&f);
    let out = f
        .cmd(&["run", "-d", "--name", "e1", "android:37"])
        .env("FAKE_LIST_FILE", &listing)
        .env("DORKER_PULL", "1")
        .output()
        .unwrap();
    assert_eq!(code(&out), 0, "{}", text(&out.stderr));
    // DORKER_PULL=0 is off.
    let f2 = Fake::new("autopull-env0", false);
    write_exec(
        &f2.sdk.join("cmdline-tools/latest/bin/android"),
        ANDROID_CLI,
    );
    let listing2 = repo_listing(&f2);
    let out = f2
        .cmd(&["run", "-d", "android:37"])
        .env("FAKE_LIST_FILE", &listing2)
        .env("DORKER_PULL", "0")
        .output()
        .unwrap();
    assert_eq!(code(&out), 3);
}

#[test]
fn build_pulls_the_dorkerfiles_missing_base_image_with_the_flag() {
    let _guard = serial();
    let f = Fake::new("autopull-build", false);
    write_exec(&f.sdk.join("cmdline-tools/latest/bin/android"), ANDROID_CLI);
    let listing = repo_listing(&f);
    let ctx = f.root.join("ctx");
    std::fs::create_dir_all(&ctx).unwrap();
    std::fs::write(ctx.join("Dorkerfile"), "FROM android:37\n").unwrap();
    let ctx_s = ctx.to_str().unwrap();

    // Without the flag the build stops before doing anything, with the hint.
    let out = f
        .cmd(&["build", "-t", "t:1", ctx_s])
        .env("FAKE_LIST_FILE", &listing)
        .output()
        .unwrap();
    assert_eq!(code(&out), 3, "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("--pull"),
        "{}",
        text(&out.stderr)
    );
    assert!(!f.piddir.join("android-args.txt").exists());

    // With it: the base image is installed, then the whole build runs against the fake emulator.
    let out = f
        .cmd(&["build", "-t", "t:1", "--pull", ctx_s])
        .env("FAKE_LIST_FILE", &listing)
        .output()
        .unwrap();
    assert_eq!(code(&out), 0, "{}", text(&out.stderr));
    let args = std::fs::read_to_string(f.piddir.join("android-args.txt")).unwrap();
    assert!(
        args.contains("system-images;android-37.0;google_apis;"),
        "{args}"
    );
    let images = text(&f.run(&["images"]).stdout);
    assert!(images.contains("t "), "{images}");
    assert!(f.live_emulators().is_empty(), "the build instance is gone");
}

#[test]
fn a_pull_that_cannot_succeed_still_reports_what_exists() {
    let _guard = serial();
    let f = Fake::new("autopull-unavailable", false);
    write_exec(&f.sdk.join("cmdline-tools/latest/bin/android"), ANDROID_CLI);
    let listing = repo_listing(&f);
    let out = f
        .cmd(&["run", "-d", "--pull", "android:99"])
        .env("FAKE_LIST_FILE", &listing)
        .output()
        .unwrap();
    assert_eq!(code(&out), 3, "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("dorker versions"),
        "{}",
        text(&out.stderr)
    );
}

// ---------- `dorker run` with no image: build and run the Dorkerfile here ----------

#[test]
fn run_without_an_image_builds_and_runs_the_dorkerfile_in_this_folder() {
    let _guard = serial();
    let f = Fake::new("run-dorkerfile", true);
    let ctx = f.root.join("proj");
    std::fs::create_dir_all(&ctx).unwrap();
    std::fs::write(ctx.join("Dorkerfile"), "FROM android:35\n").unwrap();
    let in_ctx = |args: &[&str]| f.cmd(args).current_dir(&ctx).output().unwrap();

    // First run: builds the image, then starts an instance of it.
    let out = in_ctx(&["run", "-d", "--json", "--name", "one"]);
    assert_eq!(code(&out), 0, "{}", text(&out.stderr));
    let inst: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("stdout is only the JSON");
    assert_eq!(inst["status"], "READY");
    assert!(
        text(&out.stderr).contains("Built image"),
        "{}",
        text(&out.stderr)
    );
    assert!(
        text(&out.stderr).contains("Running image"),
        "{}",
        text(&out.stderr)
    );
    assert!(
        ctx.join("Dorkerfile.lock").is_file(),
        "it is a normal build, so the lockfile is written"
    );
    let image_id = inst["image"].as_str().unwrap().to_string();
    assert_eq!(image_id.len(), 12, "{inst}");

    // Second run: nothing changed, so the build is a cache hit and the same image is used.
    let out = in_ctx(&["run", "-d", "--json", "--name", "two"]);
    assert_eq!(code(&out), 0, "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("Using cached image"),
        "{}",
        text(&out.stderr)
    );
    let second: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(second["image"], image_id.as_str());
    assert_eq!(
        f.live_emulators().len(),
        2,
        "two isolated instances of one image"
    );

    // Edit the Dorkerfile: `run` now starts the *current* configuration (a new image).
    std::fs::write(ctx.join("Dorkerfile"), "FROM android:35\nMEMORY 3GB\n").unwrap();
    let out = in_ctx(&["run", "-d", "--json", "--name", "three"]);
    assert_eq!(code(&out), 0, "{}", text(&out.stderr));
    let third: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_ne!(
        third["image"],
        image_id.as_str(),
        "a changed Dorkerfile means a new image"
    );
    assert_eq!(third["memory_mb"], 3072, "the new setting is what runs");
    assert_eq!(second["memory_mb"], 2048);

    // -f points at a Dorkerfile elsewhere; relative paths inside it resolve next to that file.
    let other = f.root.join("other");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(other.join("Custom.dorker"), "FROM android:35\nCPUS 1\n").unwrap();
    let out = f
        .cmd(&[
            "run",
            "-d",
            "--json",
            "--name",
            "four",
            "-f",
            other.join("Custom.dorker").to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(code(&out), 0, "{}", text(&out.stderr));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()["cpus"],
        1
    );
}

#[test]
fn run_without_an_image_explains_missing_or_broken_dorkerfiles() {
    let _guard = serial();
    let f = Fake::new("run-dorkerfile-errors", true);
    let empty = f.root.join("empty");
    std::fs::create_dir_all(&empty).unwrap();

    let out = f.cmd(&["run", "-d"]).current_dir(&empty).output().unwrap();
    assert_eq!(code(&out), 2, "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("no image given and no Dorkerfile"),
        "{}",
        text(&out.stderr)
    );
    assert!(
        text(&out.stderr).contains("dorker run android:35"),
        "{}",
        text(&out.stderr)
    );

    // A broken Dorkerfile reports its position, exactly like `dorker build`.
    std::fs::write(empty.join("Dorkerfile"), "FROM android:35\nMEM 2GB\n").unwrap();
    let out = f.cmd(&["run", "-d"]).current_dir(&empty).output().unwrap();
    assert_eq!(code(&out), 2);
    assert!(
        text(&out.stderr).starts_with("Dorkerfile:2:1"),
        "{}",
        text(&out.stderr)
    );
    assert!(f.live_emulators().is_empty(), "nothing was started");

    // An image and -f together are ambiguous.
    let out = f
        .cmd(&["run", "-d", "android:35", "-f", "Dorkerfile"])
        .current_dir(&empty)
        .output()
        .unwrap();
    assert_eq!(code(&out), 2, "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("only applies when no image is given"),
        "{}",
        text(&out.stderr)
    );

    // A build failure inside `run` keeps build's exit code (here: missing system image).
    let bare = Fake::new("run-dorkerfile-noimage", false);
    std::fs::create_dir_all(bare.root.join("p")).unwrap();
    std::fs::write(bare.root.join("p/Dorkerfile"), "FROM android:35\n").unwrap();
    let out = bare
        .cmd(&["run", "-d"])
        .current_dir(bare.root.join("p"))
        .output()
        .unwrap();
    assert_eq!(code(&out), 3, "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("--pull"),
        "{}",
        text(&out.stderr)
    );
}

// ---------- dorker prune ----------

/// Build an image from `FROM android:35` with the given extra lines; returns its id.
fn build_image(f: &Fake, name: &str, extra: &str, tag: Option<&str>) -> String {
    let ctx = f.root.join(format!("ctx-{name}"));
    std::fs::create_dir_all(&ctx).unwrap();
    std::fs::write(ctx.join("Dorkerfile"), format!("FROM android:35\n{extra}")).unwrap();
    let mut args = vec!["build"];
    if let Some(t) = tag {
        args.extend(["-t", t]);
    }
    args.push(ctx.to_str().unwrap());
    let out = f.run(&args);
    assert_eq!(code(&out), 0, "{}", text(&out.stderr));
    text(&out.stdout).trim().to_string()
}

fn image_ids(f: &Fake) -> Vec<String> {
    let v: serde_json::Value =
        serde_json::from_slice(&f.run(&["images", "--json"]).stdout).unwrap();
    let mut ids: Vec<String> = v
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap().to_string())
        .collect();
    ids.sort();
    ids
}

/// Make an image look like it was last used `secs_ago` seconds ago.
fn age_image(f: &Fake, id: &str, secs_ago: u64) {
    let path = f.home.join("images").join(id).join("image.json");
    let mut v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    v["last_used"] = serde_json::json!(now - secs_ago);
    v["created_at"] = serde_json::json!(now - secs_ago - 10);
    std::fs::write(path, serde_json::to_vec_pretty(&v).unwrap()).unwrap();
}

#[test]
fn prune_removes_only_what_is_unused_and_never_deletes_without_confirmation() {
    let _guard = serial();
    let f = Fake::new("prune", true);
    let keep_tagged = build_image(&f, "a", "MEMORY 2GB\n", Some("keep:1"));
    let unused_b = build_image(&f, "b", "MEMORY 3GB\n", None);
    let in_use = build_image(&f, "c", "MEMORY 4GB\n", None);
    let unused_d = build_image(&f, "d", "MEMORY 5GB\n", None);
    assert_eq!(image_ids(&f).len(), 4);
    assert_eq!(code(&f.run(&["run", "-d", "--name", "u", &in_use])), 0);

    // Not a terminal and no --yes: refuse, and delete nothing.
    let out = f.run(&["prune"]);
    assert_eq!(code(&out), 2, "{}", text(&out.stderr));
    assert!(text(&out.stderr).contains("--yes"), "{}", text(&out.stderr));
    assert_eq!(image_ids(&f).len(), 4, "nothing was removed");

    // --dry-run lists exactly the untagged, unused images and removes nothing.
    let out = f.run(&["prune", "--dry-run"]);
    assert_eq!(code(&out), 0, "{}", text(&out.stderr));
    let plan = text(&out.stdout);
    assert!(
        plan.contains(&unused_b) && plan.contains(&unused_d),
        "{plan}"
    );
    assert!(
        !plan.contains(&keep_tagged) && !plan.contains(&in_use),
        "tagged and in-use images are not offered: {plan}"
    );
    assert!(
        plan.contains("Kept: 1 used by instances, 1 tagged"),
        "{plan}"
    );
    assert!(
        plan.contains("--instances"),
        "mentions what is not covered: {plan}"
    );
    assert_eq!(image_ids(&f).len(), 4);

    // --yes removes them. The instance that was made from an image keeps running.
    let out = f.run(&["prune", "--yes"]);
    assert_eq!(code(&out), 0, "{}", text(&out.stderr));
    assert!(
        text(&out.stdout).contains("Removed 2 image(s)"),
        "{}",
        text(&out.stdout)
    );
    let mut left = vec![keep_tagged.clone(), in_use.clone()];
    left.sort();
    assert_eq!(image_ids(&f), left);
    assert_eq!(f.live_emulators().len(), 1);
    assert!(
        text(&f.run(&["prune", "--yes"]).stdout).starts_with("Nothing to prune."),
        "nothing is left to remove by default"
    );

    // --all also removes the tagged image, but still never one an instance came from.
    let out = f.run(&["prune", "--all", "--yes"]);
    assert_eq!(code(&out), 0, "{}", text(&out.stderr));
    assert_eq!(image_ids(&f), std::slice::from_ref(&in_use));
}

#[test]
fn prune_older_than_instances_json_and_incomplete_builds() {
    let _guard = serial();
    let f = Fake::new("prune-more", true);
    let old = build_image(&f, "old", "MEMORY 3GB\n", None);
    let fresh = build_image(&f, "fresh", "MEMORY 4GB\n", None);
    age_image(&f, &old, 10 * 86_400);

    // --older-than keeps the recently used image.
    let out = f.run(&["prune", "--older-than", "7d", "--yes"]);
    assert_eq!(code(&out), 0, "{}", text(&out.stderr));
    assert_eq!(image_ids(&f), std::slice::from_ref(&fresh));
    assert_eq!(
        code(&f.run(&["prune", "--older-than", "soon"])),
        2,
        "a bad duration is a configuration error"
    );

    // JSON: machine-readable plan; refuses to delete unless told to.
    let _extra = build_image(&f, "extra", "MEMORY 5GB\n", None);
    assert_eq!(code(&f.run(&["prune", "--json"])), 2);
    let out = f.run(&["prune", "--json", "--dry-run"]);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout is only JSON");
    assert_eq!(v["dry_run"], true);
    assert_eq!(v["images"].as_array().unwrap().len(), 2);
    assert!(v["reclaimable_bytes"].as_u64().unwrap() > 0);
    assert_eq!(image_ids(&f).len(), 2, "a dry run removes nothing");

    // Instances: stopped ones go with --instances, running ones stay.
    assert_eq!(code(&f.run(&["run", "-d", "--name", "stays", &fresh])), 0);
    assert_eq!(code(&f.run(&["run", "-d", "--name", "goes", &fresh])), 0);
    assert_eq!(code(&f.run(&["stop", "goes"])), 0);
    wait_until("the stopped emulator to exit", || {
        f.live_emulators().len() == 1
    });
    let out = f.run(&["prune", "--instances", "--yes"]);
    assert_eq!(code(&out), 0, "{}", text(&out.stderr));
    let names: Vec<String> = ps_json(&f)
        .iter()
        .map(|i| i["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, ["stays"], "only the stopped instance was removed");
    assert_eq!(f.live_emulators().len(), 1);

    // Leftovers of an interrupted build are cleaned once they are not brand new.
    let partial = f.home.join("images").join(".deadbeef.partial");
    std::fs::create_dir_all(partial.join("avd")).unwrap();
    assert!(
        text(&f.run(&["prune", "--dry-run"]).stdout).contains("Nothing to prune")
            || !text(&f.run(&["prune", "--dry-run"]).stdout).contains("Incomplete"),
        "a fresh partial may be a build in progress"
    );
    let file = std::fs::File::open(&partial).unwrap();
    file.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(3600))
        .unwrap();
    let out = f.run(&["prune", "--yes"]);
    assert_eq!(code(&out), 0, "{}", text(&out.stderr));
    assert!(!partial.exists(), "the stale partial build was removed");
    assert_eq!(
        image_ids(&f),
        std::slice::from_ref(&fresh),
        "the default prune also removed the unused image; the one an instance came from stays"
    );
}

#[test]
fn ls_shows_images_instances_and_storage() {
    let _guard = serial();
    let f = Fake::new("ls", true);
    let tagged = build_image(&f, "a", "MEMORY 2GB\n", Some("app:latest"));
    let unused = build_image(&f, "b", "MEMORY 3GB\n", None);
    assert_eq!(code(&f.run(&["run", "-d", "--name", "web", &tagged])), 0);
    assert_eq!(code(&f.run(&["run", "-d", "--name", "web2", &tagged])), 0);

    let out = f.run(&["ls"]);
    assert_eq!(code(&out), 0, "{}", text(&out.stderr));
    let table = text(&out.stdout);
    assert!(table.starts_with("IMAGES\n"), "{table}");
    let tagged_row = table.lines().find(|l| l.starts_with(&tagged)).unwrap();
    assert!(
        tagged_row.contains("app:latest") && tagged_row.contains("2 instances"),
        "{tagged_row}"
    );
    let unused_row = table.lines().find(|l| l.starts_with(&unused)).unwrap();
    assert!(
        unused_row.contains("<none>") && unused_row.trim_end().ends_with('-'),
        "{unused_row}"
    );
    assert!(
        table.contains("INSTANCES") && table.contains("web2") && table.contains("READY"),
        "{table}"
    );
    assert!(
        table.contains("STORAGE")
            && table.contains("1 unused")
            && table.contains("reclaimable with `dorker prune`"),
        "{table}"
    );
    assert!(
        table.contains("Android SDK system images (not managed by dorker): 1,"),
        "{table}"
    );

    // JSON: usage per image, storage totals, and the same reclaimable count prune would act on.
    let v: serde_json::Value =
        serde_json::from_slice(&f.run(&["ls", "--json"]).stdout).expect("stdout is only JSON");
    let img = v["images"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == tagged.as_str())
        .unwrap();
    assert_eq!(img["used_by_instances"], 2);
    assert_eq!(v["instances"].as_array().unwrap().len(), 2);
    assert_eq!(v["storage"]["reclaimable_images"], 1);
    let prune: serde_json::Value =
        serde_json::from_slice(&f.run(&["prune", "--json", "--dry-run"]).stdout).unwrap();
    assert_eq!(
        v["storage"]["reclaimable_bytes"], prune["reclaimable_bytes"],
        "ls and prune agree on what is reclaimable"
    );

    // `df` is an alias, and a stopped instance is reported as such.
    assert_eq!(code(&f.run(&["stop", "web2"])), 0);
    let out = f.run(&["df"]);
    assert!(
        text(&out.stdout)
            .lines()
            .any(|l| l.starts_with("web2-") || (l.contains("web2") && l.contains("STOPPED"))),
        "{}",
        text(&out.stdout)
    );
    // A killed emulator is shown as CRASHED, not READY (same reconciliation as `ps`).
    let pid = f.live_emulators()[0];
    let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
    wait_until("the killed emulator to be gone", || {
        f.live_emulators().is_empty()
    });
    assert!(
        text(&f.run(&["ls"]).stdout)
            .lines()
            .any(|l| l.contains("web ") && l.contains("CRASHED")),
        "{}",
        text(&f.run(&["ls"]).stdout)
    );
}
