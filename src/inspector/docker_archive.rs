use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result};

use super::archive;
use super::{FileEntry, ImageInfo, Inspector, LayerInfo};

/// Reads layers from a pre-existing tar archive (`docker save`, `podman save`,
/// `ctr image export`, or any OCI-layout tar).
pub struct DockerArchiveInspector {
    archive_path: PathBuf,
    cached_files: HashMap<String, Vec<FileEntry>>,
    cache_populated: bool,
}

impl DockerArchiveInspector {
    pub fn new(archive_path: PathBuf) -> Self {
        Self {
            archive_path,
            cached_files: HashMap::new(),
            cache_populated: false,
        }
    }
}

impl Inspector for DockerArchiveInspector {
    fn inspect(&mut self, _image: &str) -> Result<ImageInfo> {
        let filename = self
            .archive_path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();

        let result = archive::parse_archive(
            &self.archive_path,
            &filename,
            "",
            None,
            &mut None,
        )
        .with_context(|| format!("Failed to parse archive {}", self.archive_path.display()))?;

        self.cached_files = result.files;
        self.cache_populated = true;

        Ok(result.info)
    }

    fn list_files(&mut self, layer: &LayerInfo) -> Result<Vec<FileEntry>> {
        if !self.cache_populated {
            anyhow::bail!("inspect() must be called before list_files()");
        }

        self.cached_files
            .remove(&layer.digest)
            .with_context(|| format!("Layer {} not found in archive", layer.digest))
    }
}
