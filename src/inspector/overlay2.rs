use std::path::PathBuf;

use anyhow::Result;

use super::{FileEntry, Inspector, LayerInfo};

/// Reads layers directly from overlay2 storage on disk.
/// Fastest path — no decompression, but requires root.
pub struct Overlay2Inspector {
    storage_root: PathBuf,
}

impl Overlay2Inspector {
    pub fn new(storage_root: PathBuf) -> Self {
        Self { storage_root }
    }
}

impl Inspector for Overlay2Inspector {
    fn list_layers(&mut self, _image: &str) -> Result<Vec<LayerInfo>> {
        // TODO: read image manifest from storage_root, resolve layer chain
        let _ = &self.storage_root;
        anyhow::bail!("overlay2 inspector not yet implemented")
    }

    fn list_files(&mut self, _layer: &LayerInfo) -> Result<Vec<FileEntry>> {
        // TODO: walk overlay2/<id>/diff/ directory
        anyhow::bail!("overlay2 inspector not yet implemented")
    }
}
