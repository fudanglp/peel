use std::fs;
use std::io::{self, Write};
use std::path::Path;

use anyhow::{Context, Result};
use crossterm::style::{self, Stylize};

use crate::config;
use crate::inspector::{self, Inspector};
use crate::probe::{RuntimeInfo, StorageDriver};

pub fn run(image: &str, use_oci: bool, json: Option<&str>, runtime: Option<String>) -> Result<()> {
    config::init_from_cli(json.is_some(), runtime)?;
    let cfg = config::get();

    // If the image looks like a tar file, use the archive inspector directly
    let mut inspector: Box<dyn Inspector> = if looks_like_archive(image) {
        Box::new(inspector::docker_archive::DockerArchiveInspector::new(
            image.into(),
        ))
    } else if use_oci {
        // Use OCI/runtime API path
        let cmd = cfg
            .probe
            .default
            .map(|i| cfg.probe.runtimes[i].binary_path.display().to_string())
            .unwrap_or_else(|| "docker".to_string());
        Box::new(inspector::oci::OciInspector::new(cmd))
    } else {
        // Direct storage access — may need sudo
        if let Some(idx) = cfg.probe.default {
            let rt = &cfg.probe.runtimes[idx];
            if !rt.can_read {
                maybe_escalate(rt)?;
            }
            match rt.storage_driver {
                #[cfg(target_os = "linux")]
                StorageDriver::Overlay2 | StorageDriver::Fuse | StorageDriver::Vfs => {
                    Box::new(inspector::overlay2::Overlay2Inspector::new(
                        rt.storage_root.clone(),
                    ))
                }
                _ => {
                    // Unsupported storage driver for direct access, fall back to OCI
                    Box::new(inspector::oci::OciInspector::new(
                        rt.binary_path.display().to_string(),
                    ))
                }
            }
        } else {
            anyhow::bail!("No container runtime detected. Install Docker or Podman, or use a tar archive.");
        }
    };

    print_runtime_summary(cfg);

    let mut info = inspector.inspect(image)?;

    // Populate file lists for each layer
    for layer in &mut info.layers {
        layer.files = inspector.list_files(layer)?;
    }

    if let Some(dest) = json {
        let output = serde_json::to_string_pretty(&info)?;
        if dest == "-" {
            println!("{output}");
        } else {
            fs::write(dest, &output)
                .with_context(|| format!("Failed to write JSON to {dest}"))?;
            eprintln!("Wrote {dest}");
        }
    } else {
        println!("{}", info.name);
        if let Some(arch) = &info.architecture {
            println!("  arch: {arch}");
        }
        println!("  total size: {} bytes", info.total_size);
        println!();
        for layer in &info.layers {
            println!("{}", layer.digest);
            if let Some(cmd) = &layer.created_by {
                println!("  {cmd}");
            }
            println!("  size: {} bytes", layer.size);
            println!();
        }
    }

    Ok(())
}

fn print_runtime_summary(cfg: &config::AppConfig) {
    let mut stderr = io::stderr();

    if cfg.probe.runtimes.is_empty() {
        let _ = writeln!(stderr, "{} No container runtimes detected", "!".yellow().bold());
        return;
    }

    let detected: Vec<String> = cfg
        .probe
        .runtimes
        .iter()
        .map(|rt| rt.kind.to_string())
        .collect();
    let _ = writeln!(
        stderr,
        "{} {}",
        "Runtimes".dim(),
        detected.join(", ")
    );

    if let Some(idx) = cfg.probe.default {
        let rt = &cfg.probe.runtimes[idx];
        let _ = writeln!(
            stderr,
            "{} {} (storage: {}, driver: {})",
            "Selected".dim(),
            style::style(&rt.kind).green().bold(),
            style::style(rt.storage_root.display()).dim(),
            style::style(&rt.storage_driver).dim(),
        );
    }

    let _ = writeln!(stderr);
}

fn looks_like_archive(image: &str) -> bool {
    let p = Path::new(image);
    matches!(
        p.extension().and_then(|e| e.to_str()),
        Some("tar" | "gz" | "tgz")
    ) || image.ends_with(".tar.gz")
}

/// Re-execute the current process under sudo.
fn escalate_with_sudo() -> Result<()> {
    let exe = std::env::current_exe()?;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let status = std::process::Command::new("sudo")
        .arg(exe)
        .args(&args)
        .status()?;
    std::process::exit(status.code().unwrap_or(1));
}

/// Prompt the user to re-run with sudo if direct storage access requires root.
fn maybe_escalate(rt: &RuntimeInfo) -> Result<()> {
    let mut stderr = io::stderr();
    write!(
        stderr,
        "{} Direct layer access reads from {} which is owned by root.\n",
        "!".yellow().bold(),
        style::style(rt.storage_root.display()).bold()
    )?;
    write!(
        stderr,
        "  peel needs to re-run with {} to read layers directly.\n\n",
        "sudo".bold()
    )?;
    write!(
        stderr,
        "  Alternatively, run with {} to read layers through the {} API\n",
        "--use-oci".green().bold(),
        rt.kind
    )?;
    write!(stderr, "  (no root needed, but slower).\n\n")?;
    write!(stderr, "Re-run with sudo? {} ", "[Y/n]".dim())?;
    stderr.flush()?;

    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    let answer = answer.trim().to_lowercase();

    if answer.is_empty() || answer == "y" || answer == "yes" {
        writeln!(stderr, "{}", "─".repeat(40).dim())?;
        escalate_with_sudo()?;
    }

    anyhow::bail!(
        "Cannot read storage without root. Re-run with sudo or use --use-oci."
    );
}
