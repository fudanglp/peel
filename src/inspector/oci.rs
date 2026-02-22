use std::collections::HashMap;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use indicatif::ProgressBar;
use serde::Deserialize;

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

// --- Docker archive (docker save) ---

#[derive(Deserialize)]
struct DockerManifestEntry {
    #[serde(rename = "Layers")]
    layers: Vec<String>,
}

// --- OCI archive (ctr image export) ---

#[derive(Deserialize)]
struct OciIndex {
    manifests: Vec<OciDescriptor>,
}

#[derive(Deserialize)]
struct OciDescriptor {
    digest: String,
    #[serde(default)]
    size: u64,
}

#[derive(Deserialize)]
struct OciManifest {
    config: OciDescriptor,
    layers: Vec<OciDescriptor>,
}

#[derive(Deserialize)]
struct OciConfig {
    architecture: Option<String>,
    rootfs: OciRootFS,
    #[serde(default)]
    history: Vec<OciHistoryEntry>,
}

#[derive(Deserialize)]
struct OciRootFS {
    diff_ids: Vec<String>,
}

#[derive(Deserialize)]
struct OciHistoryEntry {
    created_by: Option<String>,
    #[serde(default)]
    empty_layer: bool,
}

/// Reads layers via the container runtime CLI (`docker`/`podman`/`ctr`).
/// Cross-platform, no root needed, but slower (requires CLI calls).
pub struct OciInspector {
    cmd: String,
    kind: RuntimeKind,
    image_name: Option<String>,
    diff_ids: Vec<String>,
    cached_files: HashMap<String, Vec<FileEntry>>,
    cache_populated: bool,
    progress: Option<ProgressBar>,
}

impl OciInspector {
    pub fn new(cmd: String, kind: RuntimeKind) -> Self {
        Self {
            cmd,
            kind,
            image_name: None,
            diff_ids: Vec::new(),
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

    fn inc_parse_progress(&self) {
        if let Some(bar) = &self.progress {
            bar.inc(1);
        }
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

    // ---- Docker / Podman: fast metadata via CLI ----

    fn inspect_via_cli(&mut self, image: &str) -> Result<ImageInfo> {
        let (name, tag) = parse_image_ref(image);

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

        let mut layers = Vec::with_capacity(diff_ids.len());
        let mut total_size = 0u64;

        for (i, digest) in diff_ids.iter().enumerate() {
            let (created_by, size) = non_empty
                .get(i)
                .map(|(cmd, sz)| (cmd.clone(), *sz))
                .unwrap_or((None, 0));
            total_size += size;
            layers.push(LayerInfo {
                digest: digest.clone(),
                created_by,
                size,
                files: Vec::new(),
            });
        }

        self.image_name = Some(image.to_string());
        self.diff_ids = diff_ids.clone();

        // Save image and parse all layer file listings up front
        let size_str = format_bytes(di.size);
        self.finish_step(
            "Resolved image metadata",
            format!("Saving {} ...", image),
        );
        let tmp = self.save_to_file(image, Some(di.size))?;
        self.finish_step(
            format!("{} exported ({})", image, size_str),
            format!("Parsing {} layers ...", layers.len()),
        );
        self.start_parse_progress(layers.len() as u64);
        let parse_result = self.parse_docker_archive(&tmp);
        let _ = std::fs::remove_file(&tmp);
        parse_result?;

        Ok(ImageInfo {
            name,
            tag: Some(tag),
            architecture: di.architecture,
            total_size,
            layers,
        })
    }

    fn parse_docker_archive(&mut self, path: &Path) -> Result<()> {
        let file = std::fs::File::open(path)
            .with_context(|| format!("Failed to open {}", path.display()))?;
        let mut archive = tar::Archive::new(file);

        let mut layer_files: HashMap<String, Vec<FileEntry>> = HashMap::new();
        let mut manifest: Option<Vec<DockerManifestEntry>> = None;

        for entry_result in archive.entries().context("Failed to read tar entries")? {
            let mut entry = entry_result.context("Failed to read tar entry")?;
            let entry_path = entry.path()?.to_string_lossy().to_string();

            if entry_path == "manifest.json" {
                let mut content = String::new();
                entry.read_to_string(&mut content)?;
                manifest = Some(
                    serde_json::from_str(&content)
                        .context("Failed to parse manifest.json from docker save output")?,
                );
            } else if entry_path.ends_with("/layer.tar") {
                self.inc_parse_progress();
                let files = Self::parse_layer_entry(&mut entry)
                    .with_context(|| format!("Failed to parse layer {entry_path}"))?;
                layer_files.insert(entry_path, files);
            }
        }

        let manifest = manifest.context("manifest.json not found in docker save output")?;
        let me = manifest
            .into_iter()
            .next()
            .context("Empty manifest in docker save output")?;

        // Modern Docker (v25+) uses OCI-layout archives where layers are stored as
        // blobs/sha256/<hash> instead of <id>/layer.tar.  If any manifest layer
        // paths weren't found in the first pass, do a second pass targeting them.
        let missing: Vec<String> = me
            .layers
            .iter()
            .filter(|p| !layer_files.contains_key(p.as_str()))
            .cloned()
            .collect();

        if !missing.is_empty() {
            let file = std::fs::File::open(path)
                .with_context(|| format!("Failed to open {}", path.display()))?;
            let mut archive = tar::Archive::new(file);

            for entry_result in archive.entries().context("Failed to read tar entries")? {
                let mut entry = entry_result.context("Failed to read tar entry")?;
                let entry_path = entry.path()?.to_string_lossy().to_string();

                if missing.iter().any(|m| m == &entry_path) {
                    self.inc_parse_progress();
                    let files = Self::parse_layer_entry(&mut entry)
                        .with_context(|| format!("Failed to parse layer {entry_path}"))?;
                    layer_files.insert(entry_path, files);
                }
            }
        }

        for (i, tar_path) in me.layers.iter().enumerate() {
            if let Some(diff_id) = self.diff_ids.get(i) {
                let files = layer_files.remove(tar_path).unwrap_or_default();
                self.cached_files.insert(diff_id.clone(), files);
            }
        }

        self.cache_populated = true;
        Ok(())
    }

    // ---- Containerd (ctr): metadata + files from OCI export ----

    fn inspect_via_export(&mut self, image: &str) -> Result<ImageInfo> {
        let (name, tag) = parse_image_ref(image);

        self.finish_step(
            "Resolved image metadata",
            format!("Exporting {} ...", image),
        );
        let tmp = self.save_to_file(image, None)?;
        self.finish_step(
            format!("{} exported", image),
            "Parsing layers ...".to_string(),
        );
        let result = self.parse_oci_archive(&tmp, &name, &tag);
        let _ = std::fs::remove_file(&tmp);

        result
    }

    fn parse_oci_archive(&mut self, path: &Path, name: &str, tag: &str) -> Result<ImageInfo> {
        // Pass 1: read index.json and small blobs (manifest, config).
        // Skip large blobs (layers) to keep memory bounded.
        let file = std::fs::File::open(path)?;
        let mut archive = tar::Archive::new(file);

        let mut index_data: Option<Vec<u8>> = None;
        let mut small_blobs: HashMap<String, Vec<u8>> = HashMap::new();

        for entry_result in archive.entries()? {
            let mut entry = entry_result?;
            let entry_path = entry.path()?.to_string_lossy().to_string();

            if entry_path == "index.json" {
                let mut data = Vec::new();
                entry.read_to_end(&mut data)?;
                index_data = Some(data);
            } else if let Some(hash) = entry_path.strip_prefix("blobs/sha256/") {
                if entry.size() < 1_000_000 {
                    let mut data = Vec::new();
                    entry.read_to_end(&mut data)?;
                    small_blobs.insert(format!("sha256:{hash}"), data);
                }
            }
        }

        // Resolve index → manifest → config
        let index: OciIndex = serde_json::from_slice(
            index_data
                .as_ref()
                .context("index.json not found in OCI archive")?,
        )
        .context("Failed to parse index.json")?;

        let manifest_desc = index.manifests.first().context("No manifests in index.json")?;
        let manifest: OciManifest = serde_json::from_slice(
            small_blobs
                .get(&manifest_desc.digest)
                .with_context(|| format!("Manifest blob {} not found", manifest_desc.digest))?,
        )
        .context("Failed to parse OCI manifest")?;

        let config: OciConfig = serde_json::from_slice(
            small_blobs
                .get(&manifest.config.digest)
                .with_context(|| format!("Config blob {} not found", manifest.config.digest))?,
        )
        .context("Failed to parse OCI image config")?;

        let diff_ids = config.rootfs.diff_ids;

        // Build compressed-digest → diff_id mapping
        let mut digest_to_diffid: HashMap<&str, &str> = HashMap::new();
        for (i, layer_desc) in manifest.layers.iter().enumerate() {
            if let Some(diff_id) = diff_ids.get(i) {
                digest_to_diffid.insert(&layer_desc.digest, diff_id);
            }
        }

        // Pass 2: read layer blobs (large entries skipped in pass 1)
        self.start_parse_progress(diff_ids.len() as u64);
        let file = std::fs::File::open(path)?;
        let mut archive = tar::Archive::new(file);

        for entry_result in archive.entries()? {
            let mut entry = entry_result?;
            let entry_path = entry.path()?.to_string_lossy().to_string();

            if let Some(hash) = entry_path.strip_prefix("blobs/sha256/") {
                let digest_str = format!("sha256:{hash}");
                if let Some(diff_id) = digest_to_diffid.get(digest_str.as_str()) {
                    if !self.cached_files.contains_key(*diff_id) {
                        self.inc_parse_progress();
                        let files = Self::parse_layer_entry(&mut entry)
                            .with_context(|| format!("Failed to parse layer {digest_str}"))?;
                        self.cached_files.insert((*diff_id).to_string(), files);
                    }
                }
            }
        }

        // Also parse any tiny layers that ended up in small_blobs
        for (digest, data) in &small_blobs {
            if let Some(diff_id) = digest_to_diffid.get(digest.as_str()) {
                if !self.cached_files.contains_key(*diff_id) {
                    self.inc_parse_progress();
                    let files = Self::parse_layer_bytes(data)
                        .with_context(|| format!("Failed to parse layer {digest}"))?;
                    self.cached_files.insert((*diff_id).to_string(), files);
                }
            }
        }

        self.cache_populated = true;

        // Match non-empty history entries to diff_ids (same as overlay2)
        let mut created_by_list: Vec<Option<String>> = Vec::new();
        for entry in &config.history {
            if !entry.empty_layer {
                created_by_list.push(entry.created_by.clone());
            }
        }

        let mut layers = Vec::with_capacity(diff_ids.len());
        let mut total_size = 0u64;

        for (i, digest) in diff_ids.iter().enumerate() {
            let size = manifest.layers.get(i).map(|d| d.size).unwrap_or(0);
            total_size += size;
            layers.push(LayerInfo {
                digest: digest.clone(),
                created_by: created_by_list.get(i).cloned().flatten(),
                size,
                files: Vec::new(),
            });
        }

        self.image_name = Some(name.to_string());
        self.diff_ids = diff_ids;

        Ok(ImageInfo {
            name: name.to_string(),
            tag: Some(tag.to_string()),
            architecture: config.architecture,
            total_size,
            layers,
        })
    }

    // ---- Shared layer parsing ----

    /// Read a layer tar entry and enumerate its files (auto-detects gzip).
    fn parse_layer_entry<R: Read>(entry: &mut R) -> Result<Vec<FileEntry>> {
        let mut data = Vec::new();
        entry.read_to_end(&mut data)?;
        Self::parse_layer_bytes(&data)
    }

    fn parse_layer_bytes(data: &[u8]) -> Result<Vec<FileEntry>> {
        let is_gzip = data.len() >= 2 && data[0] == 0x1f && data[1] == 0x8b;
        let cursor = Cursor::new(data);

        if is_gzip {
            Self::parse_inner_tar(flate2::read::GzDecoder::new(cursor))
        } else {
            Self::parse_inner_tar(cursor)
        }
    }

    fn parse_inner_tar<R: Read>(reader: R) -> Result<Vec<FileEntry>> {
        let mut archive = tar::Archive::new(reader);
        let mut files = Vec::new();

        for entry_result in archive.entries()? {
            let entry = match entry_result {
                Ok(e) => e,
                Err(_) => continue,
            };

            if entry.header().entry_type().is_dir() {
                continue;
            }

            let path = match entry.path() {
                Ok(p) => p.to_path_buf(),
                Err(_) => continue,
            };

            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();

            let is_whiteout = name.starts_with(".wh.");
            let size = if is_whiteout { 0 } else { entry.size() };

            files.push(FileEntry {
                path,
                size,
                is_whiteout,
            });
        }

        files.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(files)
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

/// Parse `name:tag` handling registry port syntax (`registry:5000/foo:bar`).
fn parse_image_ref(image: &str) -> (String, String) {
    if let Some((n, t)) = image.rsplit_once(':') {
        if t.contains('/') {
            (image.to_string(), "latest".to_string())
        } else {
            (n.to_string(), t.to_string())
        }
    } else {
        (image.to_string(), "latest".to_string())
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
