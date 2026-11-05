mod bench;
mod build;
mod commands;
mod doctor;
mod dorkerfile;
mod errors;
mod images;
mod log;
mod runtime;
mod sdk;
mod store;
mod testcmd;
#[cfg(test)]
mod testutil;
mod units;

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
    },
    /// List images
    Images {
        #[arg(long)]
        json: bool,
    },
    /// Remove an image
    Rmi { image: String },
    /// Install a system image, e.g. `dorker pull android:35`
    Pull { image: String },
    /// Create a starter Dorkerfile in the current directory
    Init,
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
    },
    /// Launch an instance from an image (e.g. android:35)
    Run {
        image: String,
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
        instance: String,
        /// Command to run, e.g. "pm list packages"
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Install an APK into an instance
    Install {
        instance: String,
        apk: std::path::PathBuf,
    },
    /// Capture a PNG screenshot
    Screenshot {
        instance: String,
        output: std::path::PathBuf,
    },
    /// Show Android logcat for an instance
    Logs {
        instance: String,
        /// Stream until interrupted
        #[arg(short, long)]
        follow: bool,
    },
    /// Restart a stopped instance (cold boot; keeps its data)
    Start {
        instance: String,
        /// Seconds to wait for Android to boot
        #[arg(long, default_value_t = 300)]
        timeout: u64,
    },
    /// Stop a running instance
    Stop {
        #[arg(required = true)]
        instances: Vec<String>,
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
            name,
            detach,
            rm,
            memory,
            cpus,
            adb_port,
            timeout,
            gpu,
        } => commands::run(commands::RunArgs {
            image,
            name,
            detach,
            rm,
            memory,
            cpus,
            adb_port,
            timeout,
            gpu,
        }),
        Command::Build {
            context,
            file,
            tag,
            no_cache,
            timeout,
        } => build::build(build::BuildArgs {
            file,
            tag,
            context,
            timeout,
            no_cache,
        }),
        Command::Pull { image } => commands::pull(&image),
        Command::Init => commands::init(),
        Command::Test {
            instance,
            runner,
            class,
            timeout,
        } => testcmd::run(&instance, &runner, class.as_deref(), timeout),
        Command::Bench {
            image,
            runs,
            apk,
            json,
            timeout,
            gpu,
            skip_idle,
        } => bench::run(bench::BenchArgs {
            image,
            runs: runs.max(1),
            apk,
            json,
            timeout,
            gpu,
            skip_idle,
        }),
        Command::Images { json } => commands::images(json),
        Command::Rmi { image } => commands::rmi(&image),
        Command::Ps { all, json } => commands::ps(all, json),
        Command::Exec { instance, command } => commands::exec(&instance, &command),
        Command::Install { instance, apk } => commands::install(&instance, &apk),
        Command::Screenshot { instance, output } => commands::screenshot(&instance, &output),
        Command::Logs { instance, follow } => commands::logs(&instance, follow),
        Command::Start { instance, timeout } => commands::start_existing(&instance, timeout),
        Command::Stop { instances } => commands::each(&instances, commands::stop),
        Command::Rm { instances, force } => commands::each(&instances, |i| commands::rm(i, force)),
    }
}
