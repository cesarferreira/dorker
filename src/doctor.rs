use crate::sdk::{Sdk, host_abi};
use crate::store::Store;
use anyhow::Result;
use std::process::Command;

#[derive(Clone, Copy, PartialEq)]
enum Level {
    Ok,
    Warn,
    Fail,
}

struct Check {
    name: &'static str,
    level: Level,
    detail: String,
    fix: Option<String>,
}

fn out(cmd: &mut Command) -> Option<String> {
    let o = cmd.output().ok()?;
    o.status
        .success()
        .then(|| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("").to_string()
}

fn collect() -> Vec<Check> {
    let mut v = Vec::new();
    let mut push = |name, level, detail: String, fix: Option<&str>| {
        v.push(Check {
            name,
            level,
            detail,
            fix: fix.map(String::from),
        })
    };

    // Platform
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    let ver = if os == "macos" {
        out(Command::new("sw_vers").arg("-productVersion"))
    } else {
        None
    };
    let supported = (os == "macos" && arch == "aarch64") || os == "linux";
    push(
        "platform",
        if supported { Level::Ok } else { Level::Warn },
        format!("{os} {} {arch}", ver.unwrap_or_default())
            .trim()
            .to_string(),
        (!supported)
            .then_some("dorker targets macOS Apple Silicon first; other hosts are untested"),
    );

    // SDK
    let sdk = Sdk::locate();
    match &sdk {
        Ok(s) => push("android sdk", Level::Ok, s.root.display().to_string(), None),
        Err(e) => push(
            "android sdk",
            Level::Fail,
            first_line(&e.to_string()),
            Some("brew install --cask android-commandlinetools  # then set ANDROID_HOME"),
        ),
    }

    if let Ok(s) = &sdk {
        // emulator
        if s.emulator().is_file() {
            let v = out(Command::new(s.emulator()).arg("-version")).unwrap_or_default();
            let line = v
                .lines()
                .find(|l| l.contains("emulator version"))
                .unwrap_or("")
                .to_string();
            push("emulator", Level::Ok, line, None);
        } else {
            push(
                "emulator",
                Level::Fail,
                format!("not found at {}", s.emulator().display()),
                Some("sdkmanager --install emulator"),
            );
        }
        // adb
        if s.adb().is_file() {
            let v = out(Command::new(s.adb()).arg("version")).unwrap_or_default();
            push("adb", Level::Ok, first_line(&v), None);
        } else {
            push(
                "adb",
                Level::Fail,
                format!("not found at {}", s.adb().display()),
                Some("sdkmanager --install platform-tools"),
            );
        }
        // system images
        let images = s.system_images();
        let host: Vec<_> = images.iter().filter(|i| i.abi == host_abi()).collect();
        if host.is_empty() {
            push(
                "system images",
                Level::Fail,
                format!("no {} images installed", host_abi()),
                Some(&format!(
                    "sdkmanager \"system-images;android-35;google_apis;{}\"",
                    host_abi()
                )),
            );
        } else {
            let list: Vec<String> = host
                .iter()
                .map(|i| format!("android:{}-{}", i.api, i.flavour))
                .collect();
            push("system images", Level::Ok, list.join(", "), None);
        }
    }

    // Hardware acceleration
    if os == "macos" {
        let hv = out(Command::new("sysctl").args(["-n", "kern.hv_support"]));
        match hv.as_deref() {
            Some("1") => push(
                "acceleration",
                Level::Ok,
                "Hypervisor.framework available".into(),
                None,
            ),
            _ => push(
                "acceleration",
                Level::Fail,
                "Hypervisor.framework unavailable".into(),
                Some("Run on bare-metal macOS; nested virtualisation is not supported"),
            ),
        }
    } else if std::path::Path::new("/dev/kvm").exists() {
        push("acceleration", Level::Ok, "/dev/kvm present".into(), None);
    } else {
        push(
            "acceleration",
            Level::Fail,
            "/dev/kvm missing".into(),
            Some("Enable KVM and add your user to the kvm group"),
        );
    }

    // Disk space (df -k <dir>: available KiB is column 4)
    let home = crate::sdk::dirs_home().unwrap_or_else(|| "/".into());
    if let Some(df) = out(Command::new("df").arg("-k").arg(&home)) {
        let avail_kb: u64 = df
            .lines()
            .nth(1)
            .and_then(|l| l.split_whitespace().nth(3))
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let gb = avail_kb / 1024 / 1024;
        push(
            "disk space",
            if gb >= 10 { Level::Ok } else { Level::Warn },
            format!("{gb} GB free"),
            (gb < 10).then_some("each instance needs roughly 4-8 GB; free up space"),
        );
    }

    // Store
    match Store::open() {
        Ok(_) => push(
            "state dir",
            Level::Ok,
            std::env::var("DORKER_HOME").unwrap_or_else(|_| "~/.dorker".into()),
            None,
        ),
        Err(e) => push(
            "state dir",
            Level::Fail,
            e.to_string(),
            Some("check permissions on ~/.dorker"),
        ),
    }
    v
}

pub fn run(json: bool) -> Result<i32> {
    let checks = collect();
    let failed = checks.iter().any(|c| c.level == Level::Fail);
    if json {
        let rows: Vec<_> = checks
            .iter()
            .map(|c| {
                serde_json::json!({
                    "check": c.name,
                    "status": match c.level { Level::Ok => "ok", Level::Warn => "warn", Level::Fail => "fail" },
                    "detail": c.detail, "fix": c.fix,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({ "ok": !failed, "checks": rows }))?
        );
    } else {
        for c in &checks {
            let mark = match c.level {
                Level::Ok => "ok  ",
                Level::Warn => "warn",
                Level::Fail => "FAIL",
            };
            println!("[{mark}] {:<14} {}", c.name, c.detail);
            if let (Some(fix), true) = (&c.fix, c.level != Level::Ok) {
                println!("       fix: {fix}");
            }
        }
        println!(
            "\n{}",
            if failed {
                "Some checks failed."
            } else {
                "All required checks passed."
            }
        );
    }
    Ok(if failed { 2 } else { 0 })
}
