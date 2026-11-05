mod commands;
mod doctor;
mod runtime;
mod sdk;
mod store;
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
        /// Guest RAM, e.g. 2GB or 2048MB
        #[arg(long, default_value = "2GB")]
        memory: String,
        /// Virtual CPU count
        #[arg(long, default_value_t = 2)]
        cpus: u32,
        /// Emulator console port (even, 5554..=5680); ADB uses port+1
        #[arg(long)]
        adb_port: Option<u16>,
        /// Seconds to wait for Android to boot
        #[arg(long, default_value_t = 300)]
        timeout: u64,
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
    /// Stop a running instance
    Stop { instance: String },
    /// Remove an instance and its data
    Rm {
        instance: String,
        /// Stop the instance first if it is running
        #[arg(short, long)]
        force: bool,
    },
}

fn main() {
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("error: {e:#}");
            std::process::exit(1);
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
        } => commands::run(commands::RunArgs {
            image,
            name,
            detach,
            rm,
            memory,
            cpus,
            adb_port,
            timeout,
        }),
        Command::Ps { all, json } => commands::ps(all, json),
        Command::Exec { instance, command } => commands::exec(&instance, &command),
        Command::Install { instance, apk } => commands::install(&instance, &apk),
        Command::Screenshot { instance, output } => commands::screenshot(&instance, &output),
        Command::Logs { instance, follow } => commands::logs(&instance, follow),
        Command::Stop { instance } => commands::stop(&instance),
        Command::Rm { instance, force } => commands::rm(&instance, force),
    }
}
