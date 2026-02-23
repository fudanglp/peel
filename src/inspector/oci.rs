use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use indicatif::ProgressBar;
use serde::Deserialize;

use super::archive::{self, ArchiveResult};
use super::{FileEntry, ImageInfo, Inspector, LayerInfo};
use crate::probe::RuntimeKind;

// --- Docker CLI JSON output ---

#[derive(Deserialize)]
struct DockerInspect {
    #[serde(rename = "Architecture")]
    architecture: Option<String>,
    #[serde(rename = "Size", default)]
    size: u64,
    #[serde(rename = "RootFS")]
    rootfs: InspectRootFS,
}

#[derive(Deserialize)]
struct InspectRootFS {
    #[serde(rename = "Layers")]
    layers: Vec<String>,
}

#[derive(Deserialize)]
struct HistoryLine {
    #[serde(rename = "CreatedBy", default)]
    created_by: Option<String>,
    #[serde(rename = "Size", default)]
    size: String,
}

/// Reads layers via the container runtime CLI (`docker`/`podman`/`ctr`).
/// Cross-platform, no root needed, but slower (requires CLI calls).
pub struct OciInspector {
    cmd: String,
    kind: RuntimeKind,
    cached_files: HashMap<String, Vec<FileEntry>>,
    cache_populated: bool,
    progress: Option<ProgressBar>,
}

impl OciInspector {
    pub fn new(cmd: String, kind: RuntimeKind) -> Self {
        Self {
            cmd,
            kind,
            cached_files: HashMap::new(),
            cache_populated: false,
            progress: None,
        }
    }

    /// Attach a progress bar (clone of a Spinner's inner bar) for status updates.
    pub fn set_progress_bar(&mut self, bar: ProgressBar) {
        self.progress = Some(bar);
    }

    fn finish_step(&self, done_msg: impl Into<String>, next_msg: impl Into<String>) {
        if let Some(bar) = &self.progress {
            use crossterm::style::Stylize;
            bar.finish_and_clear();
            eprintln!("{} {}", "✔".green(), done_msg.into());
            bar.reset();
            bar.set_style(
                indicatif::ProgressStyle::default_spinner()
                    .template("{spinner:.dim} {msg}")
                    .unwrap(),
            );
            bar.set_message(next_msg.into());
            bar.enable_steady_tick(std::time::Duration::from_millis(80));
        }
    }

    fn start_parse_progress(&self, total: u64) {
        if let Some(bar) = &self.progress {
            bar.set_length(total);
            bar.set_position(0);
            bar.set_style(
                indicatif::ProgressStyle::with_template(
                    "{spinner:.dim} Parsing layers [{bar:20}] {pos}/{len} ({elapsed_precise:.>5})",
                )
                .unwrap()
                .with_key("elapsed_precise", |state: &indicatif::ProgressState, w: &mut dyn std::fmt::Write| {
                    let _ = write!(w, "{}s", state.elapsed().as_secs());
                })
                .progress_chars("━╸░"),
            );
        }
    }

    fn make_progress_callback(&self) -> Option<archive::OnLayerParsed> {
        self.progress.clone().map(|bar| {
            Box::new(move || bar.inc(1)) as archive::OnLayerParsed
        })
    }

    fn temp_path() -> PathBuf {
        std::env::temp_dir().join(format!("peel-save-{}.tar", std::process::id()))
    }

    /// Save/export the image to a temp file.
    fn save_to_file(&self, image: &str, total_size: Option<u64>) -> Result<PathBuf> {
        match self.kind {
            RuntimeKind::Containerd => self.save_via_export(image),
            RuntimeKind::Docker | RuntimeKind::Podman => self.save_via_pipe(image, total_size),
        }
    }

    /// ctr requires a file path argument — no stdout piping.
    fn save_via_export(&self, image: &str) -> Result<PathBuf> {
        let tmp = Self::temp_path();
        let tmp_str = tmp.to_string_lossy();

        let output = Command::new(&self.cmd)
            .args(["image", "export", &tmp_str, image])
            .output()
            .with_context(|| format!("Failed to run '{} image export'", self.cmd))?;
        if !output.status.success() {
            let _ = std::fs::remove_file(&tmp);
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("Failed to export '{}': {}", image, stderr.trim());
        }
        Ok(tmp)
    }

    /// docker/podman: pipe stdout to temp file with byte-level progress.
    fn save_via_pipe(&self, image: &str, total_size: Option<u64>) -> Result<PathBuf> {
        let tmp = Self::temp_path();

        let mut cmd = Command::new(&self.cmd);
        cmd.args(["save", image]);
        if matches!(self.kind, RuntimeKind::Podman) {
            cmd.arg("--format=docker-archive");
        }

        let mut child = cmd
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("Failed to run '{} save'", self.cmd))?;

        let mut stdout = child.stdout.take().context("Failed to capture stdout")?;
        let mut file = std::fs::File::create(&tmp)
            .with_context(|| format!("Failed to create {}", tmp.display()))?;

        if let (Some(bar), Some(total)) = (&self.progress, total_size.filter(|&s| s > 0)) {
            bar.set_length(total);
            bar.set_position(0);
            bar.set_style(
                indicatif::ProgressStyle::with_template(
                    "{spinner:.dim} {msg} [{bar:20}] {bytes}/{total_bytes} ({elapsed_precise:.>5})",
                )
                .unwrap()
                .with_key("elapsed_precise", |state: &indicatif::ProgressState, w: &mut dyn std::fmt::Write| {
                    let _ = write!(w, "{}s", state.elapsed().as_secs());
                })
                .progress_chars("━╸░"),
            );

            let mut buf = [0u8; 64 * 1024];
            loop {
                let n = stdout.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                file.write_all(&buf[..n])?;
                bar.inc(n as u64);
            }
        } else {
            std::io::copy(&mut stdout, &mut file)?;
        }

        drop(file);
        drop(stdout);
        let status = child.wait()?;
        if !status.success() {
            let _ = std::fs::remove_file(&tmp);
            let mut stderr_str = String::new();
            if let Some(mut stderr) = child.stderr.take() {
                let _ = stderr.read_to_string(&mut stderr_str);
            }
            bail!("Failed to save '{}': {}", image, stderr_str.trim());
        }

        Ok(tmp)
    }

    fn store_result(&mut self, result: ArchiveResult) -> ImageInfo {
        self.cached_files = result.files;
        self.cache_populated = true;
        result.info
    }

    // ---- Docker / Podman: fast metadata via CLI ----

    fn inspect_via_cli(&mut self, image: &str) -> Result<ImageInfo> {
        let (name, tag) = archive::parse_image_ref(image);

        // `docker image inspect`
        let inspect_out = Command::new(&self.cmd)
            .args(["image", "inspect", image, "--format", "{{json .}}"])
            .output()
            .with_context(|| {
                format!("Failed to run '{} image inspect'", self.cmd)
            })?;

        if !inspect_out.status.success() {
            let stderr = String::from_utf8_lossy(&inspect_out.stderr);
            bail!(
                "'{} image inspect {}' failed: {}",
                self.cmd,
                image,
                stderr.trim()
            );
        }

        let json = String::from_utf8_lossy(&inspect_out.stdout);
        let di: DockerInspect =
            serde_json::from_str(json.trim()).context("Failed to parse docker inspect JSON")?;
        let diff_ids = di.rootfs.layers;

        // `docker image history`
        let history_out = Command::new(&self.cmd)
            .args([
                "image", "history", image, "--no-trunc", "--format", "{{json .}}",
            ])
            .output()
            .with_context(|| {
                format!("Failed to run '{} image history'", self.cmd)
            })?;

        if !history_out.status.success() {
            let stderr = String::from_utf8_lossy(&history_out.stderr);
            bail!(
                "'{} image history {}' failed: {}",
                self.cmd,
                image,
                stderr.trim()
            );
        }

        let history_str = String::from_utf8_lossy(&history_out.stdout);
        let mut history_entries: Vec<HistoryLine> = Vec::new();
        for line in history_str.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let entry: HistoryLine = serde_json::from_str(line)
                .with_context(|| format!("Failed to parse history line: {line}"))?;
            history_entries.push(entry);
        }

        // docker history is newest-first; reverse to base-first
        history_entries.reverse();

        // Non-empty history entries correspond 1:1 to diff_ids
        let non_empty: Vec<(Option<String>, u64)> = history_entries
            .iter()
            .filter(|e| parse_docker_size(&e.size) > 0)
            .map(|e| (e.created_by.clone(), parse_docker_size(&e.size)))
            .collect();

        // Save image and parse all layer file listings via shared archive lib
        let size_str = format_bytes(di.size);
        self.finish_step(
            "Resolved image metadata",
            format!("Saving {} ...", image),
        );
        let tmp = self.save_to_file(image, Some(di.size))?;
        self.finish_step(
            format!("{} exported ({})", image, size_str),
            format!("Parsing {} layers ...", diff_ids.len()),
        );
        self.start_parse_progress(diff_ids.len() as u64);
        let mut on_layer = self.make_progress_callback();
        let result = archive::parse_archive(&tmp, &name, &tag, Some(&diff_ids), &mut on_layer);
        let _ = std::fs::remove_file(&tmp);
        let mut result = result?;

        // Override layer metadata with the richer CLI-sourced info
        let mut total_size = 0u64;
        for (i, layer) in result.info.layers.iter_mut().enumerate() {
            if let Some((created_by, size)) = non_empty.get(i) {
                layer.created_by = created_by.clone();
                layer.size = *size;
                total_size += size;
            }
        }
        result.info.total_size = total_size;
        result.info.architecture = di.architecture;

        Ok(self.store_result(result))
    }

    // ---- Containerd (ctr): metadata + files from OCI export ----

    fn inspect_via_export(&mut self, image: &str) -> Result<ImageInfo> {
        let (name, tag) = archive::parse_image_ref(image);

        self.finish_step(
            "Resolved image metadata",
            format!("Exporting {} ...", image),
        );
        let tmp = self.save_to_file(image, None)?;
        self.finish_step(
            format!("{} exported", image),
            "Parsing layers ...".to_string(),
        );

        let num_layers_guess = 10u64; // we don't know yet, progress will update
        self.start_parse_progress(num_layers_guess);
        let mut on_layer = self.make_progress_callback();
        let result = archive::parse_archive(&tmp, &name, &tag, None, &mut on_layer);
        let _ = std::fs::remove_file(&tmp);

        Ok(self.store_result(result?))
    }
}

impl Inspector for OciInspector {
    fn inspect(&mut self, image: &str) -> Result<ImageInfo> {
        match self.kind {
            RuntimeKind::Containerd => self.inspect_via_export(image),
            RuntimeKind::Docker | RuntimeKind::Podman => self.inspect_via_cli(image),
        }
    }

    fn list_files(&mut self, layer: &LayerInfo) -> Result<Vec<FileEntry>> {
        if !self.cache_populated {
            bail!("inspect() must be called before list_files()");
        }

        self.cached_files
            .remove(&layer.digest)
            .with_context(|| format!("Layer {} not found in save output", layer.digest))
    }
}

/// Parse Docker's human-readable size strings (e.g. "77.84MB", "0B") into bytes.
fn parse_docker_size(s: &str) -> u64 {
    let s = s.trim();
    if s.is_empty() || s == "0B" {
        return 0;
    }
    if let Ok(n) = s.parse::<u64>() {
        return n;
    }
    let unit_start = s.find(|c: char| c.is_alphabetic()).unwrap_or(s.len());
    let num: f64 = s[..unit_start].parse().unwrap_or(0.0);
    let unit = &s[unit_start..];
    let multiplier = match unit {
        "B" => 1.0,
        "kB" | "KB" => 1_000.0,
        "MB" => 1_000_000.0,
        "GB" => 1_000_000_000.0,
        "TB" => 1_000_000_000_000.0,
        _ => 1.0,
    };
    (num * multiplier) as u64
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
