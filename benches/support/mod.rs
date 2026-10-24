//! Shared helpers for the benchmark binaries.
#![allow(dead_code)]

use std::process::Command;

fn cmd(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).output().ok()?;
    let s = String::from_utf8(out.stdout).ok()?;
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_string())
}

/// CPU model string, best effort.
pub fn cpu() -> String {
    if cfg!(target_os = "macos") {
        return cmd("sysctl", &["-n", "machdep.cpu.brand_string"])
            .unwrap_or_else(|| "unknown".into());
    }
    std::fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("model name"))
                .and_then(|l| l.split(':').nth(1))
                .map(|s| s.trim().to_string())
        })
        .unwrap_or_else(|| "unknown".into())
}

/// OS name and version, best effort.
pub fn os() -> String {
    if cfg!(target_os = "macos") {
        let v = cmd("sw_vers", &["-productVersion"]).unwrap_or_default();
        return format!("macOS {v}");
    }
    cmd("uname", &["-sr"]).unwrap_or_else(|| std::env::consts::OS.into())
}

/// Total RAM in GiB, best effort.
pub fn ram_gib() -> String {
    if cfg!(target_os = "macos") {
        if let Some(b) = cmd("sysctl", &["-n", "hw.memsize"]).and_then(|s| s.parse::<u64>().ok()) {
            return format!("{}", b >> 30);
        }
    }
    std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("MemTotal"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|k| k.parse::<u64>().ok())
                .map(|kb| format!("{}", kb >> 20))
        })
        .unwrap_or_else(|| "unknown".into())
}

/// Current load averages (1/5/15 min) from `uptime`, best effort. Recorded because
/// these benchmarks are run on a shared machine.
pub fn load() -> String {
    match cmd("uptime", &[]) {
        Some(u) => match u.find("load average") {
            Some(i) => u[i..].to_string(),
            None => u,
        },
        None => "unknown".into(),
    }
}

/// Prints a machine/build metadata block.
pub fn print_metadata() {
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(0);
    let profile = if cfg!(debug_assertions) {
        "debug (NOT representative)"
    } else {
        "optimized (bench profile: opt-level=3, lto=thin, codegen-units=1)"
    };
    println!(
        "machine: cpu={} | logical_cores={cores} | ram_gib={} | os={} | arch={}",
        cpu(),
        ram_gib(),
        os(),
        std::env::consts::ARCH
    );
    println!(
        "build:   {} | profile={profile}",
        env!("TITAN_RUSTC_VERSION")
    );
}
