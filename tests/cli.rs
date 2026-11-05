//! Integration tests that drive the real binary. The first group needs no
//! emulator. The `#[ignore]` group needs an Android SDK with an installed system
//! image and boots real emulators: run it with `cargo test -- --ignored`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Env {
    home: PathBuf,
}

impl Env {
    fn new(name: &str) -> Self {
        let home = std::env::temp_dir().join(format!("dorker-it-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        Env { home }
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_dorker"));
        c.args(args).env("DORKER_HOME", &self.home);
        c
    }

    fn run(&self, args: &[&str]) -> Output {
        self.cmd(args).output().unwrap()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        // Best effort: stop anything a failed test left running.
        if let Ok(out) = Command::new(env!("CARGO_BIN_EXE_dorker"))
            .args(["ps", "-a", "--json"])
            .env("DORKER_HOME", &self.home)
            .output()
        {
            if let Ok(rows) = serde_json::from_slice::<Vec<serde_json::Value>>(&out.stdout) {
                for r in rows {
                    let _ = Command::new(env!("CARGO_BIN_EXE_dorker"))
                        .args(["rm", "-f", r["id"].as_str().unwrap_or("")])
                        .env("DORKER_HOME", &self.home)
                        .output();
                }
            }
        }
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap_or(-1)
}

fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, body).unwrap();
    p
}

#[test]
fn empty_state_lists_nothing() {
    let e = Env::new("empty");
    let ps = e.run(&["ps", "-a"]);
    assert_eq!(code(&ps), 0);
    assert_eq!(text(&ps.stdout).lines().count(), 1, "header only");
    assert_eq!(code(&e.run(&["images"])), 0);
    assert_eq!(text(&e.run(&["ps", "--json"]).stdout).trim(), "[]");
}

#[test]
fn unknown_instance_and_image_are_errors() {
    let e = Env::new("unknown");
    let o = e.run(&["stop", "nope"]);
    assert_eq!(code(&o), 1);
    assert!(text(&o.stderr).contains("no such instance"));
    assert_eq!(code(&e.run(&["rmi", "nope"])), 1);
}

#[test]
fn dorkerfile_errors_use_exit_code_2_and_show_position() {
    let e = Env::new("parse");
    let ctx = e.home.join("ctx");
    std::fs::create_dir_all(&ctx).unwrap();
    write(&ctx, "Dorkerfile", "FROM android:35\n\nMEM 2GB\n");
    let o = e.run(&["build", ctx.to_str().unwrap()]);
    assert_eq!(code(&o), 2);
    let err = text(&o.stderr);
    assert!(
        err.starts_with("Dorkerfile:3:1\nerror: unsupported directive \"MEM\""),
        "{err}"
    );
    assert!(err.contains("Did you mean \"MEMORY\"?"));
    assert_eq!(
        err.matches("error:").count(),
        1,
        "no duplicated prefix: {err}"
    );
}

#[test]
fn missing_sdk_is_exit_code_3() {
    let e = Env::new("nosdk");
    let o = e
        .cmd(&["run", "-d", "android:35"])
        .env("ANDROID_HOME", "/nonexistent")
        .env("ANDROID_SDK_ROOT", "/nonexistent")
        .env("HOME", "/nonexistent")
        .output()
        .unwrap();
    assert_eq!(code(&o), 3, "{}", text(&o.stderr));
    assert!(text(&o.stderr).contains("Android SDK not found"));
}

#[test]
fn invalid_arguments_are_exit_code_2() {
    let e = Env::new("args");
    // These fail validation before anything is started, with or without an SDK.
    let o = e.run(&["run", "-d", "android:35", "--memory", "banana"]);
    assert!(matches!(code(&o), 2 | 3), "{}", text(&o.stderr));
    let o = e.run(&["test", "x", "--runner", "bad; reboot"]);
    assert_eq!(code(&o), 2);
}

#[test]
fn insufficient_disk_space_is_a_resource_conflict_before_anything_is_created() {
    let e = Env::new("disk");
    // An absurd requirement makes the preflight fail on any machine, with no SDK needed.
    let o = e
        .cmd(&["run", "-d", "android:35"])
        .env("DORKER_MIN_FREE_MB", "999999999999")
        .output()
        .unwrap();
    assert_eq!(code(&o), 8, "{}", text(&o.stderr));
    let err = text(&o.stderr);
    assert!(err.contains("not enough free disk space"), "{err}");
    assert!(
        err.contains("DORKER_MIN_FREE_MB"),
        "tells the user how to adjust it: {err}"
    );
    assert_eq!(
        std::fs::read_dir(e.home.join("instances")).unwrap().count(),
        0,
        "nothing created"
    );
}

#[test]
fn init_creates_a_parseable_dorkerfile_and_never_overwrites() {
    let e = Env::new("init");
    let dir = e.home.join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let o = e.cmd(&["init"]).current_dir(&dir).output().unwrap();
    assert_eq!(code(&o), 0);
    let body = std::fs::read_to_string(dir.join("Dorkerfile")).unwrap();
    assert!(body.starts_with("FROM android:35"));
    let again = e.cmd(&["init"]).current_dir(&dir).output().unwrap();
    assert_eq!(code(&again), 8, "refuses to overwrite");
    assert_eq!(
        std::fs::read_to_string(dir.join("Dorkerfile")).unwrap(),
        body
    );
}

// ---------- emulator end-to-end (ignored by default) ----------

fn example_apks() -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/instrumentation");
    if !dir.join("test.apk").exists() {
        let st = Command::new(dir.join("make-apk.sh"))
            .status()
            .expect("make-apk.sh");
        assert!(
            st.success(),
            "building example APKs needs the Android build-tools and a JDK"
        );
    }
    dir
}

fn adb_ok(e: &Env, args: &[&str]) -> String {
    let o = e.run(args);
    assert_eq!(code(&o), 0, "{args:?} failed: {}", text(&o.stderr));
    text(&o.stdout)
}

#[test]
#[ignore = "boots real emulators; run with --ignored"]
fn end_to_end_build_run_exec_screenshot_test_cleanup() {
    let e = Env::new("e2e");
    let ctx = example_apks();
    let ctx = ctx.to_str().unwrap();

    // Build, then an identical rebuild must hit the cache.
    let b = e.run(&["build", "-t", "e2e:test", ctx]);
    assert_eq!(code(&b), 0, "{}", text(&b.stderr));
    let id = text(&b.stdout).trim().to_string();
    let again = e.run(&["build", "-t", "e2e:again", ctx]);
    assert!(
        text(&again.stderr).contains("Using cached image"),
        "{}",
        text(&again.stderr)
    );
    assert_eq!(text(&again.stdout).trim(), id);
    assert!(Path::new(ctx).join("Dorkerfile.lock").is_file());

    // Two instances at once, restored from the snapshot, with distinct ports.
    adb_ok(&e, &["run", "-d", "--name", "a", "e2e:test"]);
    adb_ok(&e, &["run", "-d", "--name", "b", "e2e:test"]);
    let ps: Vec<serde_json::Value> =
        serde_json::from_slice(&e.run(&["ps", "--json"]).stdout).unwrap();
    assert_eq!(ps.len(), 2);
    assert_ne!(ps[0]["serial"], ps[1]["serial"]);
    assert!(ps.iter().all(|r| r["status"] == "READY"));

    // exec reaches the right instance and propagates exit codes.
    assert_eq!(
        adb_ok(&e, &["exec", "a", "getprop ro.build.version.sdk"]).trim(),
        "35"
    );
    assert!(adb_ok(&e, &["exec", "a", "pm list packages"]).contains("com.dorker.sample"));
    assert_eq!(code(&e.run(&["exec", "a", "sh -c 'exit 7'"])), 7);

    // Writable state is isolated per instance.
    adb_ok(&e, &["exec", "a", "touch /data/local/tmp/only_in_a"]);
    assert!(!adb_ok(&e, &["exec", "b", "ls /data/local/tmp"]).contains("only_in_a"));

    // Screenshot is a real PNG.
    let png = e.home.join("shot.png");
    adb_ok(&e, &["screenshot", "a", png.to_str().unwrap()]);
    assert!(
        std::fs::read(&png)
            .unwrap()
            .starts_with(b"\x89PNG\r\n\x1a\n")
    );

    // Instrumentation: failures exit 7, a passing selection exits 0.
    let runner = "com.dorker.sample.test/com.dorker.sample.SampleRunner";
    let all = e.run(&["test", "a", "--runner", runner]);
    assert_eq!(code(&all), 7);
    assert!(text(&all.stdout).contains("2 passed, 1 failed"));
    let ok = e.run(&[
        "test",
        "a",
        "--runner",
        runner,
        "--class",
        "com.dorker.sample.SampleTests#passing",
    ]);
    assert_eq!(code(&ok), 0, "{}", text(&ok.stdout));

    // A cold boot of the image has the same provisioned state as a snapshot restore.
    adb_ok(
        &e,
        &["run", "-d", "--name", "cold", "--memory", "3GB", "e2e:test"],
    );
    assert!(adb_ok(&e, &["exec", "cold", "pm list packages"]).contains("com.dorker.sample"));

    // A stopped instance can be restarted and keeps the data it wrote.
    adb_ok(&e, &["exec", "cold", "echo kept > /data/local/tmp/marker"]);
    adb_ok(&e, &["stop", "cold"]);
    assert_eq!(
        code(&e.run(&["start", "a"])),
        8,
        "starting a running instance is a conflict"
    );
    adb_ok(&e, &["start", "cold"]);
    assert_eq!(
        adb_ok(&e, &["exec", "cold", "cat /data/local/tmp/marker"]).trim(),
        "kept"
    );

    // Name collisions are a conflict.
    assert_eq!(code(&e.run(&["run", "-d", "--name", "a", "e2e:test"])), 8);

    // Stop/remove clean up; the image is untouched.
    assert_eq!(code(&e.run(&["rm", "a"])), 1, "running instance needs -f");
    adb_ok(&e, &["rm", "-f", "a", "b", "cold"]);
    assert_eq!(text(&e.run(&["ps", "-a", "--json"]).stdout).trim(), "[]");
    assert_eq!(
        std::fs::read_dir(e.home.join("instances")).unwrap().count(),
        0
    );
    assert!(text(&e.run(&["images"]).stdout).contains("e2e"));
}

#[test]
#[ignore = "boots real emulators; run with --ignored"]
fn failed_build_leaves_nothing_behind() {
    let e = Env::new("badbuild");
    let ctx = e.home.join("ctx");
    std::fs::create_dir_all(&ctx).unwrap();
    write(
        &ctx,
        "Dorkerfile",
        "FROM android:35\nINSTALL ./bad.apk\nSNAPSHOT ready\n",
    );
    write(&ctx, "bad.apk", "not an apk");
    let o = e.run(&["build", "-t", "bad", ctx.to_str().unwrap()]);
    assert_eq!(code(&o), 4, "{}", text(&o.stderr));
    assert!(text(&o.stderr).contains("INSTALL_PARSE_FAILED"));
    assert_eq!(text(&e.run(&["ps", "-a", "--json"]).stdout).trim(), "[]");
    assert_eq!(
        std::fs::read_dir(e.home.join("instances")).unwrap().count(),
        0
    );
    let images: Vec<_> = std::fs::read_dir(e.home.join("images")).unwrap().collect();
    assert!(images.is_empty(), "no partial image published");
    assert!(!ctx.join("Dorkerfile.lock").exists());
}
