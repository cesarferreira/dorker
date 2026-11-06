mod bench;
mod build;
mod commands;
mod commit;
mod doctor;
mod dorkerfile;
mod errors;
mod images;
mod log;
mod ls;
mod mcp;
mod prune;
mod runtime;
mod sdk;
mod store;
mod style;
mod testcmd;
#[cfg(test)]
mod testutil;
mod units;
mod versions;
mod xapk;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "dorker",
    version,
    about = "Docker for headless Android emulator environments"
)]
struct Cli {
    /// Print what dorker decides and runs (emulator command line, boot mode, timings) to stderr.
    /// Also enabled by DORKER_VERBOSE=1
    #[arg(short, long, global = true)]
    verbose: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Check system requirements
    Doctor {
        /// Machine-readable output
        #[arg(long)]
        json: bool,
    },
    /// Build an image from a Dorkerfile
    Build {
        /// Build context directory
        #[arg(default_value = ".")]
        context: std::path::PathBuf,
        /// Dorkerfile path (default: <context>/Dorkerfile)
        #[arg(short, long)]
        file: Option<std::path::PathBuf>,
        /// Name and optionally tag the image, e.g. myapp:latest
        #[arg(short, long)]
        tag: Option<String>,
        /// Rebuild even if an identical image is cached
        #[arg(long)]
        no_cache: bool,
        /// Seconds to wait for Android to boot
        #[arg(long, default_value_t = 300)]
        timeout: u64,
        /// Install a missing system image without asking (also DORKER_PULL=1). Google's
        /// `android` CLI accepts the SDK licences for you when it installs.
        #[arg(long)]
        pull: bool,
        /// Let the Dorkerfile's PROVISION scripts run on this machine without asking (also
        /// DORKER_ALLOW_PROVISION=1)
        #[arg(long)]
        allow_provision: bool,
    },
    /// List images
    Images {
        #[arg(long)]
        json: bool,
    },
    /// Remove an image
    Rmi { image: String },
    /// Show images, instances and the disk space they use
    #[command(alias = "df")]
    Ls {
        #[arg(long)]
        json: bool,
    },
    /// Free disk: remove unused images (and optionally stopped instances)
    Prune {
        /// Also remove tagged images that no instance uses (default: untagged ones only)
        #[arg(short, long)]
        all: bool,
        /// Also remove instances that are not running (stopped, failed, crashed)
        #[arg(long)]
        instances: bool,
        /// Only things last used (images) or created (instances) longer ago, e.g. 7d, 12h
        #[arg(long)]
        older_than: Option<String>,
        /// Show what would be removed and exit
        #[arg(long)]
        dry_run: bool,
        /// Do not ask for confirmation
        #[arg(short, long)]
        yes: bool,
        #[arg(long)]
        json: bool,
    },
    /// List the Android versions you can use (installed, and available to pull)
    Versions {
        /// Only this API level, e.g. 37
        api: Option<u32>,
        /// Include Wear, TV, tablet and other non-phone flavours
        #[arg(long)]
        all: bool,
        #[arg(long)]
        json: bool,
    },
    /// Install a system image, e.g. `dorker pull android:35`
    Pull {
        /// Image to install; omit to install the `FROM` image of ./Dorkerfile
        image: Option<String>,
        /// Dorkerfile to read when no image is given (default: ./Dorkerfile)
        #[arg(short, long)]
        file: Option<std::path::PathBuf>,
    },
    /// Create a starter Dorkerfile in the current directory
    Init,
    /// Run a Model Context Protocol server on stdio, so AI agents can drive dorker
    Mcp,
    /// Run instrumentation tests in an instance
    Test {
        instance: String,
        /// Instrumentation runner, e.g. com.example.test/androidx.test.runner.AndroidJUnitRunner
        #[arg(long)]
        runner: String,
        /// Limit to a class or class#method (comma separated)
        #[arg(long)]
        class: Option<String>,

        /// Seconds before the run is aborted
        #[arg(long, default_value_t = 600)]
        timeout: u64,
        /// Print the result as JSON on stdout
        #[arg(long)]
        json: bool,
    },
    /// Measure boot, restore, install and resource numbers for an image
    Bench {
        /// android:<api> or a built image
        image: String,
        #[arg(long, default_value_t = 3)]
        runs: u32,
        /// APK to time the install of
        #[arg(long)]
        apk: Option<std::path::PathBuf>,
        #[arg(long)]
        json: bool,
        #[arg(long, default_value_t = 300)]
        timeout: u64,
        /// GPU mode for base `android:<api>` images
        #[arg(long)]
        gpu: Option<String>,
        /// Skip the (slow) idle CPU and memory measurement
        #[arg(long)]
        skip_idle: bool,
        /// Package to launch and time (default: the image's `CMD launch` package)
        #[arg(long)]
        launch: Option<String>,
        /// Also time the plain official emulator (cold, and with its own quickboot
        /// snapshot) as a baseline. Base `android:<api>` images only.
        #[arg(long)]
        baseline: bool,
        /// Install a missing system image without asking (also DORKER_PULL=1). Google's
        /// `android` CLI accepts the SDK licences for you when it installs.
        #[arg(long)]
        pull: bool,
    },
    /// Launch an instance from an image (e.g. android:35)
    Run {
        /// Image to run (e.g. android:35 or myapp:latest). Omit it to build and run the
        /// Dorkerfile in this folder.
        image: Option<String>,
        /// Dorkerfile to run when no image is given (default: ./Dorkerfile)
        #[arg(short, long)]
        file: Option<std::path::PathBuf>,
        /// Instance name (default: generated)
        #[arg(long)]
        name: Option<String>,
        /// Return once Android is ready instead of staying attached
        #[arg(short, long)]
        detach: bool,
        /// Remove the instance when it stops
        #[arg(long)]
        rm: bool,
        /// Guest RAM, e.g. 2GB (overriding an image's value disables its snapshot)
        #[arg(long)]
        memory: Option<String>,
        /// Virtual CPU count (overriding an image's value disables its snapshot)
        #[arg(long)]
        cpus: Option<u32>,
        /// Emulator console port (even, 5554..=5680); ADB uses port+1
        #[arg(long)]
        adb_port: Option<u16>,
        /// Seconds to wait for Android to boot
        #[arg(long, default_value_t = 300)]
        timeout: u64,
        /// Emulator GPU mode for base images: swiftshader_indirect (default), host,
        /// angle_indirect, guest, off, auto. Built images fix this in their Dorkerfile.
        #[arg(long)]
        gpu: Option<String>,
        /// Print the started instance as JSON on stdout (requires --detach)
        #[arg(long)]
        json: bool,
        /// Install a missing system image without asking (also DORKER_PULL=1). Google's
        /// `android` CLI accepts the SDK licences for you when it installs.
        #[arg(long)]
        pull: bool,
        /// Stream the device's log (logcat) while attached
        #[arg(long)]
        logs: bool,
        /// Let the Dorkerfile's PROVISION scripts run on this machine without asking (also
        /// DORKER_ALLOW_PROVISION=1)
        #[arg(long)]
        allow_provision: bool,
    },
    /// List instances
    Ps {
        /// Show stopped instances too
        #[arg(short, long)]
        all: bool,
        #[arg(long)]
        json: bool,
    },
    /// Run an Android shell command in an instance
    Exec {
        /// Print {exit_code, stdout, stderr} as JSON (put before the instance)
        #[arg(long)]
        json: bool,
        instance: String,
        /// Command to run, e.g. "pm list packages"
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Install an APK into an instance
    Install {
        instance: String,
        apk: std::path::PathBuf,
        /// Print the result as JSON on stdout
        #[arg(long)]
        json: bool,
    },
    /// Capture a PNG screenshot (use `-` as the output to write the PNG to stdout)
    Screenshot {
        instance: String,
        output: std::path::PathBuf,
        /// Print {path, bytes} as JSON on stdout
        #[arg(long)]
        json: bool,
        /// Seconds to wait for a non-blank frame (e.g. just after a cold boot); 0 disables
        #[arg(long, default_value_t = 10)]
        wait: u64,
    },
    /// Show Android logcat for an instance
    Logs {
        instance: String,
        /// Stream until interrupted
        #[arg(short, long)]
        follow: bool,
        /// Print only the last N lines and exit
        #[arg(long)]
        tail: Option<u32>,
    },
    /// Restart a stopped instance (cold boot; keeps its data)
    Start {
        instance: String,
        /// Seconds to wait for Android to boot
        #[arg(long, default_value_t = 300)]
        timeout: u64,
        /// Print the instance as JSON on stdout
        #[arg(long)]
        json: bool,
    },
    /// Save a running instance as a new image (keeps what you set up on the device)
    Commit {
        /// Instance name or id (must be READY; it is stopped by the commit)
        instance: String,
        /// Name for the new image, e.g. `myapp:signed-in`
        #[arg(short, long)]
        tag: Option<String>,
        /// Print the result as JSON on stdout
        #[arg(long)]
        json: bool,
    },
    /// Stop a running instance
    Stop {
        #[arg(required_unless_present_any = ["all", "orphans"])]
        instances: Vec<String>,
        /// Stop every running instance
        #[arg(long, conflicts_with = "instances")]
        all: bool,
        /// Also stop emulators dorker started that have no instance record (they may belong
        /// to another DORKER_HOME); asks first unless --yes
        #[arg(long, conflicts_with = "instances")]
        orphans: bool,
        /// Do not ask before stopping orphaned emulators
        #[arg(short, long)]
        yes: bool,
    },
    /// Remove an instance and its data
    Rm {
        #[arg(required = true)]
        instances: Vec<String>,
        /// Stop the instance first if it is running
        #[arg(short, long)]
        force: bool,
    },
}

fn main() {
    let cli = Cli::parse();
    let env_on = std::env::var_os("DORKER_VERBOSE").is_some_and(|v| !v.is_empty() && v != "0");
    log::set_verbose(cli.verbose || env_on);
    log::vlog!(
        "dorker {} on {} {}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    match run(cli) {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            // Parse errors already carry `file:line:col` and an `error:` line.
            let is_parse = e
                .downcast_ref::<errors::Coded>()
                .is_some_and(|c| c.inner.downcast_ref::<dorkerfile::ParseError>().is_some());
            if is_parse {
                eprintln!("{e:#}");
            } else {
                eprintln!("error: {e:#}");
            }
            std::process::exit(errors::code_of(&e));
        }
    }
}

fn run(cli: Cli) -> Result<i32> {
    match cli.command {
        Command::Doctor { json } => doctor::run(json),
        Command::Run {
            image,
            file,
            name,
            detach,
            rm,
            memory,
            cpus,
            adb_port,
            timeout,
            gpu,
            json,
            pull,
            logs,
            allow_provision,
        } => {
            let image =
                commands::run_target(image, file.as_deref(), timeout, pull, allow_provision)?;
            commands::run(commands::RunArgs {
                image,
                name,
                detach,
                rm,
                memory,
                cpus,
                adb_port,
                timeout,
                gpu,
                json,
                pull,
                logs,
            })
        }
        Command::Build {
            context,
            file,
            tag,
            no_cache,
            timeout,
            pull,
            allow_provision,
        } => build::build(build::BuildArgs {
            file,
            tag,
            context,
            timeout,
            no_cache,
            pull,
            allow_provision,
        }),
        Command::Versions { api, all, json } => versions::run(api, all, json),
        Command::Pull { image, file } => commands::pull(image.as_deref(), file.as_deref()),
        Command::Init => commands::init(),
        Command::Mcp => mcp::serve(),
        Command::Test {
            instance,
            runner,
            class,
            timeout,
            json,
        } => testcmd::run(&instance, &runner, class.as_deref(), timeout, json),
        Command::Bench {
            image,
            runs,
            apk,
            json,
            timeout,
            gpu,
            skip_idle,
            launch,
            baseline,
            pull,
        } => bench::run(bench::BenchArgs {
            image,
            runs: runs.max(1),
            apk,
            json,
            timeout,
            gpu,
            skip_idle,
            launch,
            baseline,
            pull,
        }),
        Command::Images { json } => commands::images(json),
        Command::Rmi { image } => commands::rmi(&image),
        Command::Ls { json } => ls::run(json),
        Command::Prune {
            all,
            instances,
            older_than,
            dry_run,
            yes,
            json,
        } => prune::run(prune::PruneArgs {
            all,
            instances,
            older_than,
            dry_run,
            yes,
            json,
        }),
        Command::Ps { all, json } => commands::ps(all, json),
        Command::Exec {
            json,
            instance,
            command,
        } => commands::exec(&instance, &command, json),
        Command::Install {
            instance,
            apk,
            json,
        } => commands::install(&instance, &apk, json),
        Command::Screenshot {
            instance,
            output,
            json,
            wait,
        } => commands::screenshot(&instance, &output, json, wait),
        Command::Logs {
            instance,
            follow,
            tail,
        } => commands::logs(&instance, follow, tail),
        Command::Start {
            instance,
            timeout,
            json,
        } => commands::start_existing(&instance, timeout, json),
        Command::Commit {
            instance,
            tag,
            json,
        } => commit::run(&instance, tag.as_deref(), json),
        Command::Stop {
            instances,
            all,
            orphans,
            yes,
        } => {
            if all || orphans {
                commands::stop_all(all, orphans, yes)
            } else {
                commands::each(&instances, commands::stop)
            }
        }
        Command::Rm { instances, force } => commands::each(&instances, |i| commands::rm(i, force)),
    }
}
