use std::collections::HashMap;
use std::io::{Cursor, Read};
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

use super::{FileEntry, ImageInfo, LayerInfo};

/// Parsed result from a tar archive: image metadata + per-layer file listings.
pub struct ArchiveResult {
    pub info: ImageInfo,
    /// Files keyed by diff_id (layer digest).
    pub files: HashMap<String, Vec<FileEntry>>,
}

/// Optional callback invoked after each layer is parsed.
pub type OnLayerParsed = Box<dyn FnMut()>;

// ---- Docker-format archive structs (manifest.json) ----

#[derive(Deserialize)]
struct DockerManifestEntry {
    #[serde(rename = "Config")]
    config: String,
    #[serde(rename = "Layers")]
    layers: Vec<String>,
    #[serde(rename = "RepoTags", default)]
    repo_tags: Vec<String>,
}

// ---- OCI-layout archive structs (index.json) ----

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

// ---- Shared config struct (used by both formats) ----

#[derive(Deserialize)]
struct ImageConfig {
    architecture: Option<String>,
    rootfs: Rootfs,
    #[serde(default)]
    history: Vec<HistoryEntry>,
}

#[derive(Deserialize)]
struct Rootfs {
    diff_ids: Vec<String>,
}

#[derive(Deserialize)]
struct HistoryEntry {
    created_by: Option<String>,
    #[serde(default)]
    empty_layer: bool,
}

/// Parse a tar archive file, auto-detecting Docker vs OCI format.
///
/// `name` and `tag` are used for the returned `ImageInfo` (caller decides how
/// to derive them — from the CLI ref, from the filename, etc.).
///
/// If `diff_ids_hint` is provided (from a prior `docker inspect` call), those
/// are used instead of reading the config from inside the archive.
///
/// `on_layer` is called once per layer parsed (for progress reporting).
pub fn parse_archive(
    path: &Path,
    name: &str,
    tag: &str,
    diff_ids_hint: Option<&[String]>,
    on_layer: &mut Option<OnLayerParsed>,
) -> Result<ArchiveResult> {
    // Peek at the archive to detect format
    let format = detect_format(path)?;

    match format {
        ArchiveFormat::Docker => parse_docker_format(path, name, tag, diff_ids_hint, on_layer),
        ArchiveFormat::Oci => parse_oci_format(path, name, tag, on_layer),
    }
}

#[derive(Debug)]
enum ArchiveFormat {
    Docker,
    Oci,
}

fn detect_format(path: &Path) -> Result<ArchiveFormat> {
    let file = std::fs::File::open(path)
        .with_context(|| format!("Failed to open {}", path.display()))?;
    let mut archive = tar::Archive::new(file);

    for entry_result in archive.entries().context("Failed to read tar entries")? {
        let entry = entry_result.context("Failed to read tar entry")?;
        let entry_path = entry.path()?.to_string_lossy().to_string();

        if entry_path == "manifest.json" {
            return Ok(ArchiveFormat::Docker);
        }
        if entry_path == "index.json" {
            return Ok(ArchiveFormat::Oci);
        }
    }

    anyhow::bail!("Unrecognized archive format: no manifest.json or index.json found")
}

// ---- Docker-format parsing ----

fn parse_docker_format(
    path: &Path,
    name: &str,
    tag: &str,
    diff_ids_hint: Option<&[String]>,
    on_layer: &mut Option<OnLayerParsed>,
) -> Result<ArchiveResult> {
    let file = std::fs::File::open(path)
        .with_context(|| format!("Failed to open {}", path.display()))?;
    let mut archive = tar::Archive::new(file);

    let mut layer_files: HashMap<String, Vec<FileEntry>> = HashMap::new();
    let mut manifest_data: Option<Vec<DockerManifestEntry>> = None;
    let mut configs: HashMap<String, Vec<u8>> = HashMap::new();

    for entry_result in archive.entries().context("Failed to read tar entries")? {
        let mut entry = entry_result.context("Failed to read tar entry")?;
        let entry_path = entry.path()?.to_string_lossy().to_string();

        if entry_path == "manifest.json" {
            let mut content = String::new();
            entry.read_to_string(&mut content)?;
            manifest_data = Some(
                serde_json::from_str(&content)
                    .context("Failed to parse manifest.json")?,
            );
        } else if entry_path.ends_with(".json") && entry_path != "manifest.json" {
            // Could be the image config (e.g. "abc123.json")
            let mut data = Vec::new();
            entry.read_to_end(&mut data)?;
            configs.insert(entry_path, data);
        } else if entry_path.ends_with("/layer.tar") {
            if let Some(cb) = on_layer {
                cb();
            }
            let files = parse_layer_entry(&mut entry)
                .with_context(|| format!("Failed to parse layer {entry_path}"))?;
            layer_files.insert(entry_path, files);
        }
    }

    let manifest_entries = manifest_data.context("manifest.json not found in archive")?;
    let me = manifest_entries
        .into_iter()
        .next()
        .context("Empty manifest in archive")?;

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
                if let Some(cb) = on_layer {
                    cb();
                }
                let files = parse_layer_entry(&mut entry)
                    .with_context(|| format!("Failed to parse layer {entry_path}"))?;
                layer_files.insert(entry_path, files);
            }
        }
    }

    // Resolve diff_ids: prefer hint from CLI, fall back to config in archive
    let (architecture, diff_ids, created_by_list) = if let Some(hint) = diff_ids_hint {
        // Caller already knows the diff_ids (from `docker inspect`), no config needed
        (None, hint.to_vec(), Vec::new())
    } else {
        // Read the image config from inside the archive
        let config_data = configs
            .get(&me.config)
            .with_context(|| format!("Config {} not found in archive", me.config))?;
        let config: ImageConfig =
            serde_json::from_slice(config_data).context("Failed to parse image config")?;

        let mut cbl: Vec<Option<String>> = Vec::new();
        for entry in &config.history {
            if !entry.empty_layer {
                cbl.push(entry.created_by.clone());
            }
        }

        (config.architecture, config.rootfs.diff_ids, cbl)
    };

    // Derive name/tag from RepoTags if caller didn't provide meaningful ones
    let (final_name, final_tag) = if name.is_empty() {
        if let Some(repo_tag) = me.repo_tags.first() {
            parse_image_ref(repo_tag)
        } else {
            (name.to_string(), tag.to_string())
        }
    } else {
        (name.to_string(), tag.to_string())
    };

    // Build layer info + file map keyed by diff_id
    let mut files_by_diff_id: HashMap<String, Vec<FileEntry>> = HashMap::new();
    let mut layers = Vec::with_capacity(diff_ids.len());
    let mut total_size = 0u64;

    for (i, diff_id) in diff_ids.iter().enumerate() {
        let layer_file_list = me
            .layers
            .get(i)
            .and_then(|tar_path| layer_files.remove(tar_path))
            .unwrap_or_default();

        let size: u64 = layer_file_list.iter().map(|f| f.size).sum();
        total_size += size;

        layers.push(LayerInfo {
            digest: diff_id.clone(),
            created_by: created_by_list.get(i).cloned().flatten(),
            size,
            files: Vec::new(),
        });

        files_by_diff_id.insert(diff_id.clone(), layer_file_list);
    }

    Ok(ArchiveResult {
        info: ImageInfo {
            name: final_name,
            tag: Some(final_tag),
            architecture,
            total_size,
            layers,
        },
        files: files_by_diff_id,
    })
}

// ---- OCI-layout parsing ----

fn parse_oci_format(
    path: &Path,
    name: &str,
    tag: &str,
    on_layer: &mut Option<OnLayerParsed>,
) -> Result<ArchiveResult> {
    // Pass 1: read index.json and small blobs (manifest, config).
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

    // Resolve index -> manifest -> config
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

    let config: ImageConfig = serde_json::from_slice(
        small_blobs
            .get(&manifest.config.digest)
            .with_context(|| format!("Config blob {} not found", manifest.config.digest))?,
    )
    .context("Failed to parse OCI image config")?;

    let diff_ids = config.rootfs.diff_ids;

    // Build compressed-digest -> diff_id mapping
    let mut digest_to_diffid: HashMap<&str, &str> = HashMap::new();
    for (i, layer_desc) in manifest.layers.iter().enumerate() {
        if let Some(diff_id) = diff_ids.get(i) {
            digest_to_diffid.insert(&layer_desc.digest, diff_id);
        }
    }

    // Pass 2: read layer blobs (large entries skipped in pass 1)
    let mut files_by_diff_id: HashMap<String, Vec<FileEntry>> = HashMap::new();

    let file = std::fs::File::open(path)?;
    let mut archive = tar::Archive::new(file);

    for entry_result in archive.entries()? {
        let mut entry = entry_result?;
        let entry_path = entry.path()?.to_string_lossy().to_string();

        if let Some(hash) = entry_path.strip_prefix("blobs/sha256/") {
            let digest_str = format!("sha256:{hash}");
            if let Some(diff_id) = digest_to_diffid.get(digest_str.as_str()) {
                if !files_by_diff_id.contains_key(*diff_id) {
                    if let Some(cb) = on_layer {
                        cb();
                    }
                    let files = parse_layer_entry(&mut entry)
                        .with_context(|| format!("Failed to parse layer {digest_str}"))?;
                    files_by_diff_id.insert((*diff_id).to_string(), files);
                }
            }
        }
    }

    // Also parse any tiny layers that ended up in small_blobs
    for (digest, data) in &small_blobs {
        if let Some(diff_id) = digest_to_diffid.get(digest.as_str()) {
            if !files_by_diff_id.contains_key(*diff_id) {
                if let Some(cb) = on_layer {
                    cb();
                }
                let files = parse_layer_bytes(data)
                    .with_context(|| format!("Failed to parse layer {digest}"))?;
                files_by_diff_id.insert((*diff_id).to_string(), files);
            }
        }
    }

    // Match non-empty history entries to diff_ids
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

    Ok(ArchiveResult {
        info: ImageInfo {
            name: name.to_string(),
            tag: Some(tag.to_string()),
            architecture: config.architecture,
            total_size,
            layers,
        },
        files: files_by_diff_id,
    })
}

// ---- Layer parsing (shared by both formats) ----

/// Read a layer tar entry and enumerate its files (auto-detects gzip).
pub fn parse_layer_entry<R: Read>(entry: &mut R) -> Result<Vec<FileEntry>> {
    let mut data = Vec::new();
    entry.read_to_end(&mut data)?;
    parse_layer_bytes(&data)
}

pub fn parse_layer_bytes(data: &[u8]) -> Result<Vec<FileEntry>> {
    let is_gzip = data.len() >= 2 && data[0] == 0x1f && data[1] == 0x8b;
    let cursor = Cursor::new(data);

    if is_gzip {
        parse_inner_tar(flate2::read::GzDecoder::new(cursor))
    } else {
        parse_inner_tar(cursor)
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

// ---- Helpers ----

/// Parse `name:tag` handling registry port syntax (`registry:5000/foo:bar`).
pub fn parse_image_ref(image: &str) -> (String, String) {
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
