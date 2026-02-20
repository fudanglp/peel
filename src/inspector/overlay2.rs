use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::{FileEntry, ImageInfo, Inspector, LayerInfo};

/// Reads layers directly from overlay2 storage on disk.
/// Fastest path — no decompression, but requires root.
pub struct Overlay2Inspector {
    storage_root: PathBuf,
}

#[derive(Deserialize)]
struct Repositories {
    #[serde(rename = "Repositories")]
    repositories: HashMap<String, HashMap<String, String>>,
}

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

impl Overlay2Inspector {
    pub fn new(storage_root: PathBuf) -> Self {
        Self { storage_root }
    }

    /// Parse "name:tag" or "name" (defaults to "latest"), look up in repositories.json.
    /// Returns (name, tag, config_digest_hex).
    fn resolve_image(&self, image: &str) -> Result<(String, String, String)> {
        let (name, tag) = if let Some((n, t)) = image.rsplit_once(':') {
            // If the part after ':' contains '/', it's a registry port, not a tag
            if t.contains('/') {
                (image.to_string(), "latest".to_string())
            } else {
                (n.to_string(), t.to_string())
            }
        } else {
            (image.to_string(), "latest".to_string())
        };

        let repos_path = self.storage_root.join("image/overlay2/repositories.json");
        let repos_data = fs::read_to_string(&repos_path)
            .with_context(|| format!("Failed to read {}", repos_path.display()))?;
        let repos: Repositories = serde_json::from_str(&repos_data)
            .with_context(|| format!("Failed to parse {}", repos_path.display()))?;

        let tagged_ref = format!("{name}:{tag}");

        let tags = repos
            .repositories
            .get(&name)
            .with_context(|| format!("Image '{name}' not found in repositories.json"))?;

        let config_digest = tags
            .get(&tagged_ref)
            .with_context(|| format!("Tag '{tag}' not found for image '{name}'"))?;

        let digest_hex = config_digest
            .strip_prefix("sha256:")
            .unwrap_or(config_digest);

        Ok((name, tag, digest_hex.to_string()))
    }

    fn read_image_config(&self, digest_hex: &str) -> Result<ImageConfig> {
        let config_path = self
            .storage_root
            .join("image/overlay2/imagedb/content/sha256")
            .join(digest_hex);
        let config_data = fs::read_to_string(&config_path)
            .with_context(|| format!("Failed to read image config {}", config_path.display()))?;
        serde_json::from_str(&config_data).context("Failed to parse image config")
    }

    /// Compute chain IDs from diff IDs.
    ///
    /// chain\[0\] = diff\[0\]
    /// chain\[i\] = sha256(chain\[i-1\] + " " + diff\[i\])
    fn compute_chain_ids(diff_ids: &[String]) -> Vec<String> {
        let mut chain_ids = Vec::with_capacity(diff_ids.len());
        for (i, diff_id) in diff_ids.iter().enumerate() {
            if i == 0 {
                chain_ids.push(diff_id.clone());
            } else {
                let input = format!("{} {}", chain_ids[i - 1], diff_id);
                let hash = Sha256::digest(input.as_bytes());
                chain_ids.push(format!("sha256:{hash:x}"));
            }
        }
        chain_ids
    }

    fn get_cache_id(&self, chain_id: &str) -> Result<String> {
        let chain_hex = chain_id.strip_prefix("sha256:").unwrap_or(chain_id);
        let path = self
            .storage_root
            .join("image/overlay2/layerdb/sha256")
            .join(chain_hex)
            .join("cache-id");
        let cache_id = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read cache-id for chain {chain_id}"))?;
        Ok(cache_id.trim().to_string())
    }

    fn get_layer_size(&self, chain_id: &str) -> Result<u64> {
        let chain_hex = chain_id.strip_prefix("sha256:").unwrap_or(chain_id);
        let path = self
            .storage_root
            .join("image/overlay2/layerdb/sha256")
            .join(chain_hex)
            .join("size");
        let size_str = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read size for chain {chain_id}"))?;
        size_str.trim().parse::<u64>().context("Failed to parse layer size")
    }

    fn walk_layer_dir(dir: &Path, base: &Path, entries: &mut Vec<FileEntry>) -> Result<()> {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            let metadata = entry.metadata()?;
            let relative = path.strip_prefix(base).unwrap_or(&path).to_path_buf();
            let name = entry.file_name();
            let name = name.to_string_lossy();

            if metadata.is_dir() {
                Self::walk_layer_dir(&path, base, entries)?;
            } else {
                let is_whiteout = name.starts_with(".wh.");
                entries.push(FileEntry {
                    path: relative,
                    size: if is_whiteout { 0 } else { metadata.len() },
                    is_whiteout,
                });
            }
        }
        Ok(())
    }
}

impl Inspector for Overlay2Inspector {
    fn inspect(&mut self, image: &str) -> Result<ImageInfo> {
        let (name, tag, digest_hex) = self.resolve_image(image)?;
        let config = self.read_image_config(&digest_hex)?;
        let chain_ids = Self::compute_chain_ids(&config.rootfs.diff_ids);

        // Match history entries (skipping empty layers) to diff_ids
        let mut created_by_list: Vec<Option<String>> = Vec::new();
        for entry in &config.history {
            if !entry.empty_layer {
                created_by_list.push(entry.created_by.clone());
            }
        }

        let mut layers = Vec::with_capacity(chain_ids.len());
        let mut total_size = 0u64;

        for (i, chain_id) in chain_ids.iter().enumerate() {
            let size = self.get_layer_size(chain_id).unwrap_or(0);
            total_size += size;
            layers.push(LayerInfo {
                digest: chain_id.clone(),
                created_by: created_by_list.get(i).cloned().flatten(),
                size,
                files: Vec::new(),
            });
        }

        Ok(ImageInfo {
            name: name.to_string(),
            tag: Some(tag),
            architecture: config.architecture,
            total_size,
            layers,
        })
    }

    fn list_files(&mut self, layer: &LayerInfo) -> Result<Vec<FileEntry>> {
        let cache_id = self.get_cache_id(&layer.digest)?;
        let diff_dir = self.storage_root.join("overlay2").join(&cache_id).join("diff");

        if !diff_dir.exists() {
            anyhow::bail!("Layer directory not found: {}", diff_dir.display());
        }

        let mut entries = Vec::new();
        Self::walk_layer_dir(&diff_dir, &diff_dir, &mut entries)?;
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(entries)
    }
}
