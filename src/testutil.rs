use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

/// Minimal self-cleaning temp directory so tests need no extra dependency.
pub struct Dir(PathBuf);

impl Dir {
    pub fn new() -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let p = std::env::temp_dir().join(format!(
            "dorker-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&p).unwrap();
        Dir(p)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The stub emulator records its pid and AVD home per console port and idles until killed.
/// `<root>/crash-next` (holding a delay in seconds) makes the next start crash after it.
const EMULATOR: &str = r#"#!/bin/sh
root="$(cd "$(dirname "$0")/../.." && pwd)"
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
if mv "$root/crash-next" "$root/crashing-$port" 2>/dev/null; then
  sleep "$(cat "$root/crashing-$port")"
  echo "FATAL        | fake emulator crashed: boom" >&2
  exit 1
fi
echo $$ > "$root/pids/$port.pid"
echo "$ANDROID_AVD_HOME" > "$root/pids/$port.avdhome"
trap 'exit 0' TERM INT
while :; do sleep 1; done
"#;

/// The stub adb answers what readiness and teardown need. A device exists only while its
/// emulator stub is alive. `<root>/not-booting` keeps every device booting; an executable
/// `<root>/shell.sh` answers other `adb shell` commands (args: serial, command).
const ADB: &str = r#"#!/bin/sh
root="$(cd "$(dirname "$0")/../.." && pwd)"
serial=
if [ "$1" = "-s" ]; then serial=$2; shift 2; fi
port=${serial#emulator-}
if [ -n "$serial" ]; then
  [ -f "$root/pids/$port.pid" ] && kill -0 "$(cat "$root/pids/$port.pid")" 2>/dev/null || { echo "error: device '$serial' not found" >&2; exit 1; }
fi
echo "$serial $*" >> "$root/adb.log"
case "$1" in
  shell)
    shift
    case "$*" in
      "getprop sys.boot_completed") if [ -f "$root/not-booting" ]; then echo 0; else echo 1; fi ;;
      "getprop ro.build.version.sdk") echo 35 ;;
      "pm path android") echo "package:/system/framework/framework-res.apk" ;;
      sync) ;;
      *) if [ -x "$root/shell.sh" ]; then exec "$root/shell.sh" "$serial" "$*"; fi; echo "fake: $*" ;;
    esac ;;
  emu)
    if [ "$2" = "kill" ] && [ -f "$root/pids/$port.pid" ]; then kill "$(cat "$root/pids/$port.pid")" 2>/dev/null; fi ;;
  logcat) echo "fake logcat line" ;;
esac
"#;

/// A fake Android SDK, store and image in one temp directory, for in-process lifecycle tests.
pub struct FakeSdk {
    dir: Dir,
}

impl FakeSdk {
    pub fn new() -> Self {
        use std::os::unix::fs::PermissionsExt;
        let dir = Dir::new();
        let root = dir.path();
        for (path, body) in [
            ("sdk/emulator/emulator", EMULATOR),
            ("sdk/platform-tools/adb", ADB),
        ] {
            let p = root.join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, body).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let img = root.join(format!(
            "sdk/system-images/android-35/google_apis/{}",
            crate::sdk::host_abi()
        ));
        std::fs::create_dir_all(&img).unwrap();
        std::fs::write(img.join("system.img"), b"x").unwrap();
        std::fs::write(
            img.join("package.xml"),
            "<localPackage><revision><major>1</major></revision></localPackage>",
        )
        .unwrap();
        std::fs::create_dir_all(root.join("pids")).unwrap();
        FakeSdk { dir }
    }

    pub fn root(&self) -> &Path {
        self.dir.path()
    }

    pub fn sdk(&self) -> crate::sdk::Sdk {
        crate::sdk::Sdk {
            root: self.root().join("sdk"),
        }
    }

    pub fn store(&self) -> crate::store::Store {
        crate::store::Store::at(self.root().join("home")).unwrap()
    }

    /// Publish a built image (no snapshot) tagged `tag`; returns it.
    pub fn image(&self, tag: &str, memory_mb: u64) -> crate::images::ImageMeta {
        let store = self.store();
        let id = crate::store::new_id();
        let meta = crate::images::ImageMeta {
            id: id.clone(),
            fingerprint: id.clone(),
            tags: vec![crate::images::normalize_tag(tag).unwrap()],
            api: 35,
            flavour: "google_apis".into(),
            abi: crate::sdk::host_abi().into(),
            system_image: format!(
                "system-images;android-35;google_apis;{}",
                crate::sdk::host_abi()
            ),
            system_image_revision: "1.0.0".into(),
            emulator_version: "99.0.0.0".into(),
            memory_mb,
            cpus: 1,
            disk_mb: 2048,
            device: "pixel_7".into(),
            width: 1080,
            height: 2400,
            headless: true,
            gpu: "swiftshader_indirect".into(),
            snapshot: None,
            launch: None,
            created_at: crate::store::now_secs(),
            last_used: None,
        };
        let home = crate::images::avd_home(&store, &id);
        std::fs::create_dir_all(home.join("dorker.avd")).unwrap();
        std::fs::write(home.join("dorker.avd/config.ini"), "hw.ramSize=1024\n").unwrap();
        std::fs::write(home.join("dorker.ini"), "path=x\n").unwrap();
        crate::images::save_meta(&store, &meta).unwrap();
        meta
    }

    /// Emulator stubs that are still alive (zombies count as dead).
    pub fn live_emulators(&self) -> Vec<u32> {
        let mut live = Vec::new();
        for e in std::fs::read_dir(self.root().join("pids"))
            .unwrap()
            .flatten()
        {
            if e.path().extension().is_none_or(|x| x != "pid") {
                continue;
            }
            let Ok(pid) = std::fs::read_to_string(e.path()).map(|s| s.trim().parse::<u32>()) else {
                continue;
            };
            let Ok(pid) = pid else { continue };
            let state = std::process::Command::new("ps")
                .args(["-o", "stat=", "-p", &pid.to_string()])
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .unwrap_or_default();
            if !state.is_empty() && !state.starts_with('Z') {
                live.push(pid);
            }
        }
        live
    }
}

impl Drop for FakeSdk {
    fn drop(&mut self) {
        for pid in self.live_emulators() {
            let _ = std::process::Command::new("kill")
                .args(["-9", &pid.to_string()])
                .status();
        }
    }
}
