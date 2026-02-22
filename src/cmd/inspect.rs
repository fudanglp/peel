use std::fs;
use std::io::{self, Write};
use std::path::Path;

use anyhow::{Context, Result};
use crossterm::style::{self, Stylize};

use crate::config;
use crate::inspector::{self, Inspector};
use crate::probe::{RuntimeInfo, StorageDriver};
use crate::progress::Spinner;

pub fn run(image: &str, use_oci: bool, json: Option<&str>, runtime: Option<String>, web: bool, no_sudo: bool) -> Result<()> {
    config::init_from_cli(json.is_some(), runtime)?;
    let cfg = config::get();

    print_runtime_summary(cfg);

    let spinner = Spinner::new("Resolving image metadata...");

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
        let mut oci = inspector::oci::OciInspector::new(cmd);
        oci.set_progress_bar(spinner.clone_bar());
        Box::new(oci)
    } else {
        // Direct storage access — may need sudo
        if let Some(idx) = cfg.probe.default {
            let rt = &cfg.probe.runtimes[idx];
            if !rt.can_read {
                maybe_escalate(rt, no_sudo)?;
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
                    let mut oci = inspector::oci::OciInspector::new(
                        rt.binary_path.display().to_string(),
                    );
                    oci.set_progress_bar(spinner.clone_bar());
                    Box::new(oci)
                }
            }
        } else {
            anyhow::bail!("No container runtime detected. Install Docker or Podman, or use a tar archive.");
        }
    };

    let mut info = inspector.inspect(image)?;

    let num_layers = info.layers.len();
    for (i, layer) in info.layers.iter_mut().enumerate() {
        spinner.set_message(format!("Reading layer {}/{} ...", i + 1, num_layers));
        layer.files = inspector.list_files(layer)?;
    }
    spinner.finish(format!("Inspected {} layers", num_layers));

    if web {
        let json_str = serde_json::to_string_pretty(&info)?;
        let safe_name = info
            .name
            .replace(|c: char| !c.is_alphanumeric() && c != '-', "_");
        let salt: u16 = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| (d.as_millis() % 10000) as u16)
            .unwrap_or(0);
        let tmp = std::env::temp_dir();
        let json_path = tmp.join(format!("peel-{safe_name}-{salt}.json"));
        let html_path = tmp.join(format!("peel-{safe_name}-{salt}.html"));

        fs::write(&json_path, &json_str)
            .with_context(|| format!("Failed to write JSON to {}", json_path.display()))?;
        eprintln!(
            "{} Wrote {} ({})",
            "✔".green(),
            style::style(json_path.display()).cyan(),
            format_bytes(json_str.len() as u64)
        );

        let html = super::report::build_report(&json_str);
        fs::write(&html_path, &html)
            .with_context(|| format!("Failed to write HTML to {}", html_path.display()))?;
        eprintln!(
            "{} Wrote {} ({})",
            "✔".green(),
            style::style(html_path.display()).cyan(),
            format_bytes(html.len() as u64)
        );

        return super::report::serve(&html);
    }

    if let Some(dest) = json {
        let output = serde_json::to_string_pretty(&info)?;
        if dest == "-" {
            println!("{output}");
        } else {
            fs::write(dest, &output)
                .with_context(|| format!("Failed to write JSON to {dest}"))?;
            eprintln!("{} Wrote {dest}", "✔".green());
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

fn format_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB"];
    let mut size = bytes as f64;
    for unit in UNITS {
        if size < 1024.0 {
            return if size.fract() < 0.05 {
                format!("{:.0} {unit}", size)
            } else {
                format!("{:.1} {unit}", size)
            };
        }
        size /= 1024.0;
    }
    format!("{:.1} TB", size)
}

fn looks_like_archive(image: &str) -> bool {
    let p = Path::new(image);
    matches!(
        p.extension().and_then(|e| e.to_str()),
        Some("tar" | "gz" | "tgz")
    ) || image.ends_with(".tar.gz")
}

/// Re-execute the current process under sudo, setting PEEL_ESCALATED to prevent loops.
fn escalate_with_sudo() -> Result<()> {
    let exe = std::env::current_exe()?;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let status = std::process::Command::new("sudo")
        .arg(exe)
        .args(&args)
        .env("PEEL_ESCALATED", "1")
        .status()?;
    std::process::exit(status.code().unwrap_or(1));
}

/// Auto-escalate to sudo unless --no-sudo is set.
fn maybe_escalate(rt: &RuntimeInfo, no_sudo: bool) -> Result<()> {
    let already_escalated = std::env::var("PEEL_ESCALATED").is_ok();

    if already_escalated {
        anyhow::bail!(
            "Already escalated but still cannot read {}. Check permissions.",
            rt.storage_root.display()
        );
    }

    let mut stderr = io::stderr();
    let bar: &str = &"─".repeat(56);
    writeln!(stderr)?;
    writeln!(stderr, "  {}",  bar.dim())?;
    writeln!(
        stderr,
        "  {} Reading layers directly via {} — much faster,",
        "▶".green().bold(),
        style::style("overlay2").bold()
    )?;
    writeln!(
        stderr,
        "  but {} needs root to access {}",
        "sudo".bold(),
        style::style(rt.storage_root.display()).dim()
    )?;
    writeln!(stderr)?;
    writeln!(stderr, "  Re-running as root...")?;
    writeln!(stderr)?;
    writeln!(
        stderr,
        "  {}",
        "Can't sudo? Use --no-sudo to fall back to the OCI API.".dim()
    )?;
    writeln!(stderr, "  {}", bar.dim())?;
    writeln!(stderr)?;

    if no_sudo {
        anyhow::bail!(
            "Cannot read storage without root. Remove --no-sudo or use --use-oci."
        );
    }

    escalate_with_sudo()?;

    unreachable!()
}
