pub mod docker_archive;
pub mod oci;

#[cfg(target_os = "linux")]
pub mod overlay2;

use std::path::PathBuf;

use anyhow::Result;
use serde::Serialize;

/// Metadata about a single layer in an image.
#[derive(Debug, Clone, Serialize)]
pub struct LayerInfo {
    /// Layer digest (e.g. sha256:abc123...)
    pub digest: String,

    /// The Dockerfile command that created this layer (if available)
    pub created_by: Option<String>,

    /// Total size of files in this layer, in bytes
    pub size: u64,
}

/// A single file entry within a layer.
#[derive(Debug, Clone, Serialize)]
pub struct FileEntry {
    /// Full path within the layer
    pub path: PathBuf,

    /// File size in bytes
    pub size: u64,

    /// Whether this is a whiteout (deletion marker)
    pub is_whiteout: bool,
}

/// Common interface for reading image layers from different backends.
pub trait Inspector {
    /// List all layers in an image.
    fn list_layers(&mut self, image: &str) -> Result<Vec<LayerInfo>>;

    /// List all files in a specific layer.
    fn list_files(&mut self, layer: &LayerInfo) -> Result<Vec<FileEntry>>;
}
