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
    pub gpu: Option<String>,
    pub skip_idle: bool,
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

fn os_label() -> String {
    if let Some(v) = sh("sw_vers", &["-productVersion"]) {
        return format!("macos {v}");
    }
    std::fs::read_to_string("/etc/os-release")
        .ok()
        .and_then(|t| {
            t.lines().find_map(|l| {
                l.strip_prefix("PRETTY_NAME=")
                    .map(|v| v.trim_matches('"').to_string())
            })
        })
        .unwrap_or_else(|| std::env::consts::OS.to_string())
}

fn cpu_model() -> Option<String> {
    sh("sysctl", &["-n", "machdep.cpu.brand_string"]).or_else(|| {
        std::fs::read_to_string("/proc/cpuinfo")
            .ok()?
            .lines()
            .find_map(|l| {
                l.strip_prefix("model name")
                    .and_then(|r| r.split_once(':'))
                    .map(|(_, v)| v.trim().to_string())
            })
    })
}

/// utime + stime (clock ticks) of a process, from `/proc/<pid>/stat`.
fn proc_cpu_ticks(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The command name (field 2) may contain spaces; fields resume after the last ')'.
    let rest = stat.rsplit_once(')')?.1;
    let f: Vec<&str> = rest.split_whitespace().collect();
    // After ')': state is f[0] (field 3), so utime/stime (fields 14/15) are f[11]/f[12].
    Some(f.get(11)?.parse::<u64>().ok()? + f.get(12)?.parse::<u64>().ok()?)
}

/// CPU% of one core used by a process over `secs` (interval average).
fn cpu_percent(pid: u32, secs: u64) -> Option<f64> {
    if cfg!(target_os = "macos") {
        // Two samples `secs` apart; the second is the interval average.
        sh(
            "top",
            &[
                "-l",
                "2",
                "-s",
                &secs.to_string(),
                "-pid",
                &pid.to_string(),
                "-stats",
                "cpu",
            ],
        )
        .and_then(|o| o.lines().rev().find_map(|l| l.trim().parse::<f64>().ok()))
    } else {
        // `ps %cpu` is a lifetime average (it includes the boot burst), so sample
        // /proc CPU time over an interval instead.
        let ticks = sh("getconf", &["CLK_TCK"])
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(100.0);
        let a = proc_cpu_ticks(pid)?;
        std::thread::sleep(Duration::from_secs(secs));
        let b = proc_cpu_ticks(pid)?;
        Some(b.saturating_sub(a) as f64 / ticks / secs as f64 * 100.0)
    }
}

/// Below this (% of one core over 5 s) the guest counts as idle.
const QUIET_PERCENT: f64 = 25.0;
const QUIET_TIMEOUT: Duration = Duration::from_secs(90);

/// Host memory attributable to the emulator process, in MB, and the metric used.
/// macOS `ps` RSS excludes compressed pages and swings by 2-3x for the same
/// workload, so use `footprint` (physical footprint). Linux uses VmRSS.
pub fn memory_mb(pid: u32) -> (Option<f64>, &'static str) {
    if cfg!(target_os = "macos") {
        let mb = sh("footprint", &["-p", &pid.to_string()]).and_then(|o| parse_footprint_mb(&o));
        if mb.is_some() {
            return (mb, "footprint");
        }
    }
    let kb = std::fs::read_to_string(format!("/proc/{pid}/status"))
        .ok()
        .and_then(|t| {
            t.lines().find_map(|l| {
                l.strip_prefix("VmRSS:")
                    .map(|v| v.trim().trim_end_matches("kB").trim().to_string())
            })
        })
        .and_then(|v| v.parse::<f64>().ok())
        .or_else(|| {
            sh("ps", &["-o", "rss=", "-p", &pid.to_string()]).and_then(|s| s.trim().parse().ok())
        });
    (kb.map(|k| k / 1024.0), "rss")
}

/// Parse `Footprint: 2737 MB` from `footprint -p` output.
pub fn parse_footprint_mb(out: &str) -> Option<f64> {
    let rest = out.lines().find_map(|l| l.split("Footprint:").nth(1))?;
    let mut it = rest.split_whitespace();
    let n: f64 = it.next()?.parse().ok()?;
    match it.next()? {
        "KB" => Some(n / 1024.0),
        "MB" => Some(n),
        "GB" => Some(n * 1024.0),
        _ => None,
    }
}

pub struct Idle {
    pub cpu: Option<f64>,
    pub mem_mb: Option<f64>,
    pub mem_metric: &'static str,
    /// Seconds spent waiting for the guest to go quiet before measuring.
    pub settled_after_secs: f64,
    pub quiet: bool,
}

/// Wait for the emulator to go quiet (first boot and post-restore background work
/// can use whole cores for a minute), then measure CPU over 10 s and RSS. A fixed
/// short settle time reported that background work as "idle CPU", which it is not.
fn idle_usage(pid: u32) -> Idle {
    let t0 = Instant::now();
    let mut quiet = false;
    while t0.elapsed() < QUIET_TIMEOUT {
        match cpu_percent(pid, 5) {
            Some(c) if c < QUIET_PERCENT => {
                quiet = true;
                break;
            }
            Some(_) => {}
            None => break,
        }
    }
    let settled_after_secs = t0.elapsed().as_secs_f64();
    let cpu = cpu_percent(pid, 10);
    let (mem_mb, mem_metric) = memory_mb(pid);
    Idle {
        cpu,
        mem_mb,
        mem_metric,
        settled_after_secs,
        quiet,
    }
}

fn idle_json(i: &Option<Idle>) -> serde_json::Value {
    match i {
        None => serde_json::Value::Null,
        Some(i) => json!({
            "cpu_percent": i.cpu.map(|c| (c * 10.0).round() / 10.0),
            "host_memory_mb": i.mem_mb.map(|m| m.round()),
            "memory_metric": i.mem_metric,
            "settled_after_secs": (i.settled_after_secs * 10.0).round() / 10.0,
            "reached_quiet": i.quiet,
        }),
    }
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
        gpu: a.gpu.clone(),
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

    // Idle usage is measured on the last cold-booted and the last restored instance,
    // because a restored guest holds its whole RAM image and behaves differently.
    let (mut idle_cold, mut idle_restore): (Option<Idle>, Option<Idle>) = (None, None);
    for i in 0..a.runs {
        let mut s = start(&base(format!("bench-cold-{i}")), true)?;
        cold.push(s.ready_secs);
        if i + 1 == a.runs && !a.skip_idle {
            if let Some(pid) = s.inst.pid {
                idle_cold = Some(idle_usage(pid));
            }
        }
        cleanup.push(tear_down(&mut s)?);
    }

    // The last measured run stays up for install / screenshot / idle numbers.
    let (mut install, mut shot) = (None, None);
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
            if let Some(pid) = s.inst.pid.filter(|_| !a.skip_idle) {
                idle_restore = Some(idle_usage(pid));
            }
        }
        cleanup.push(tear_down(&mut s)?);
    }

    let machine = json!({
        "os": os_label(),
        "arch": std::env::consts::ARCH,
        "cpu": cpu_model(),
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
        "idle_cold_boot": idle_json(&idle_cold),
        "idle_after_restore": idle_json(&idle_restore),
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
        for (label, idle) in [
            ("idle, cold boot", &idle_cold),
            ("idle, after restore", &idle_restore),
        ] {
            let Some(i) = idle else { continue };
            let note = if i.quiet {
                format!("quiet after {:.0}s", i.settled_after_secs)
            } else {
                format!(
                    "NOT quiet after {:.0}s; includes background work",
                    i.settled_after_secs
                )
            };
            println!(
                "  {label:<20} cpu {}  mem {} ({})  [{note}]",
                f(i.cpu, "%"),
                f(i.mem_mb.map(f64::round), " MB"),
                i.mem_metric
            );
        }
        println!("  stop + remove      {} (median)", f(median(&cleanup), "s"));
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proc_stat_parsing_handles_spaces_in_names() {
        let own = proc_cpu_ticks(std::process::id());
        if cfg!(target_os = "linux") {
            assert!(own.is_some());
        }
        // Parsing logic on a synthetic line with a ')' and spaces inside the command.
        let stat = "123 (qemu (x) y) S 1 123 123 0 -1 4194560 100 0 0 0 40 2 0 0 20 0 5 0 99 1 1";
        let rest = stat.rsplit_once(')').unwrap().1;
        let f: Vec<&str> = rest.split_whitespace().collect();
        assert_eq!(
            f[11].parse::<u64>().unwrap() + f[12].parse::<u64>().unwrap(),
            42
        );
    }

    #[test]
    fn footprint_parsing() {
        let out = "qemu-system-aarch64-headless [66205]: 64-bit    Footprint: 2737 MB (16384 bytes per page)\n";
        assert_eq!(parse_footprint_mb(out), Some(2737.0));
        assert_eq!(
            parse_footprint_mb("x Footprint: 512 KB (4096 bytes per page)"),
            Some(0.5)
        );
        assert_eq!(
            parse_footprint_mb("x Footprint: 2 GB (4096 bytes per page)"),
            Some(2048.0)
        );
        assert_eq!(parse_footprint_mb("no such line"), None);
    }

    #[test]
    fn medians() {
        assert_eq!(median(&[]), None);
        assert_eq!(median(&[3.0]), Some(3.0));
        assert_eq!(median(&[5.0, 1.0, 3.0]), Some(3.0));
        assert_eq!(median(&[1.0, 2.0, 3.0, 4.0]), Some(2.5));
    }
}
