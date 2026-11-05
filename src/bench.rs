//! `dorker bench`: measured startup, provisioning and resource numbers.

use crate::commands::{RunArgs, Started, start, stop_instance};
use crate::runtime;
use anyhow::{Context, Result};
use serde_json::json;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

pub struct BenchArgs {
    pub image: String,
    pub runs: u32,
    pub apk: Option<PathBuf>,
    pub json: bool,
    pub timeout: u64,
}

fn sh(cmd: &str, args: &[&str]) -> Option<String> {
    let o = Command::new(cmd).args(args).output().ok()?;
    o.status
        .success()
        .then(|| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

pub fn median(v: &[f64]) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let m = s.len() / 2;
    Some(if s.len() % 2 == 1 {
        s[m]
    } else {
        (s[m - 1] + s[m]) / 2.0
    })
}

/// Idle CPU% (interval average) and RSS (MB) of a process.
fn idle_usage(pid: u32) -> (Option<f64>, Option<f64>) {
    let rss = sh("ps", &["-o", "rss=", "-p", &pid.to_string()])
        .and_then(|s| s.trim().parse::<f64>().ok())
        .map(|kb| kb / 1024.0);
    let cpu = if cfg!(target_os = "macos") {
        // Two samples, 3 s apart; the second is the interval average.
        sh(
            "top",
            &[
                "-l",
                "2",
                "-s",
                "3",
                "-pid",
                &pid.to_string(),
                "-stats",
                "cpu",
            ],
        )
        .and_then(|o| o.lines().rev().find_map(|l| l.trim().parse::<f64>().ok()))
    } else {
        sh("ps", &["-o", "%cpu=", "-p", &pid.to_string()]).and_then(|s| s.trim().parse().ok())
    };
    (cpu, rss)
}

pub fn run(a: BenchArgs) -> Result<i32> {
    let base = |name: String| RunArgs {
        image: a.image.clone(),
        name: Some(name),
        detach: true,
        rm: false,
        memory: None,
        cpus: None,
        adb_port: None,
        timeout: a.timeout,
    };
    let mut cold = Vec::new();
    let mut restore = Vec::new();
    let mut cleanup = Vec::new();
    let mut snapshot_unavailable = false;

    let tear_down = |s: &mut Started| -> Result<f64> {
        let t = Instant::now();
        stop_instance(&s.sdk, &s.store, &mut s.inst)?;
        s.store.remove(&s.inst)?;
        Ok(t.elapsed().as_secs_f64())
    };

    for i in 0..a.runs {
        let mut s = start(&base(format!("bench-cold-{i}")), true)?;
        cold.push(s.ready_secs);
        cleanup.push(tear_down(&mut s)?);
    }

    // The last measured run stays up for install / screenshot / idle numbers.
    let (mut install, mut shot, mut idle_cpu, mut idle_rss) = (None, None, None, None);
    for i in 0..a.runs {
        let mut s = start(&base(format!("bench-run-{i}")), false)?;
        if s.restored {
            restore.push(s.ready_secs);
        } else {
            snapshot_unavailable = true;
        }
        if i + 1 == a.runs {
            if let Some(apk) = &a.apk {
                let t = Instant::now();
                let ok = runtime::adb(&s.sdk, &s.inst.serial())
                    .args(["install", "-r"])
                    .arg(apk)
                    .output()
                    .context("adb install")?;
                if ok.status.success() {
                    install = Some(t.elapsed().as_secs_f64());
                }
            }
            let t = Instant::now();
            let png = runtime::adb(&s.sdk, &s.inst.serial())
                .args(["exec-out", "screencap", "-p"])
                .output()?;
            if png.status.success() && png.stdout.len() > 8 {
                shot = Some(t.elapsed().as_secs_f64());
            }
            std::thread::sleep(Duration::from_secs(10)); // let the guest settle
            if let Some(pid) = s.inst.pid {
                (idle_cpu, idle_rss) = idle_usage(pid);
            }
        }
        cleanup.push(tear_down(&mut s)?);
    }

    let machine = json!({
        "os": format!("{} {}", std::env::consts::OS, sh("sw_vers", &["-productVersion"]).unwrap_or_default()).trim(),
        "arch": std::env::consts::ARCH,
        "cpu": sh("sysctl", &["-n", "machdep.cpu.brand_string"]),
        "cores": std::thread::available_parallelism().map(|n| n.get()).ok(),
        "memory_gb": sh("sysctl", &["-n", "hw.memsize"]).and_then(|s| s.parse::<f64>().ok()).map(|b| (b / 1e9).round()),
        "emulator": crate::sdk::Sdk::locate().ok().and_then(|s| s.emulator_version().ok()),
    });
    let r2 = |x: f64| (x * 100.0).round() / 100.0;
    let result = json!({
        "machine": machine,
        "image": a.image,
        "runs": a.runs,
        "cold_boot_secs": { "samples": cold, "median": median(&cold).map(r2) },
        "snapshot_restore_secs": { "samples": restore, "median": median(&restore).map(r2), "available": !snapshot_unavailable && !restore.is_empty() },
        "apk_install_secs": install.map(r2),
        "screenshot_secs": shot.map(r2),
        "idle_cpu_percent": idle_cpu,
        "idle_host_rss_mb": idle_rss.map(|m| m.round()),
        "cleanup_secs": { "median": median(&cleanup).map(r2) },
    });
    if a.json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        let f =
            |v: Option<f64>, unit: &str| v.map_or("n/a".to_string(), |x| format!("{x:.2}{unit}"));
        println!("dorker bench: {} ({} runs)", a.image, a.runs);
        println!(
            "  machine            {} / {}",
            result["machine"]["os"].as_str().unwrap_or(""),
            result["machine"]["cpu"].as_str().unwrap_or("")
        );
        println!(
            "  emulator           {}",
            result["machine"]["emulator"].as_str().unwrap_or("?")
        );
        println!(
            "  cold boot          {} (median)  samples {:?}",
            f(median(&cold), "s"),
            cold.iter().map(|x| r2(*x)).collect::<Vec<_>>()
        );
        if result["snapshot_restore_secs"]["available"] == true {
            println!(
                "  snapshot restore   {} (median)  samples {:?}",
                f(median(&restore), "s"),
                restore.iter().map(|x| r2(*x)).collect::<Vec<_>>()
            );
        } else {
            println!("  snapshot restore   n/a (image has no usable snapshot)");
        }
        println!("  apk install        {}", f(install, "s"));
        println!("  screenshot         {}", f(shot, "s"));
        println!("  idle cpu           {}", f(idle_cpu, "%"));
        println!("  idle host rss      {}", f(idle_rss, " MB"));
        println!("  stop + remove      {} (median)", f(median(&cleanup), "s"));
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn medians() {
        assert_eq!(median(&[]), None);
        assert_eq!(median(&[3.0]), Some(3.0));
        assert_eq!(median(&[5.0, 1.0, 3.0]), Some(3.0));
        assert_eq!(median(&[1.0, 2.0, 3.0, 4.0]), Some(2.5));
    }
}
