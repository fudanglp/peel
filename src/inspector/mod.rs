pub mod docker_archive;
pub mod oci;

#[cfg(target_os = "linux")]
pub mod overlay2;

use std::path::PathBuf;

use anyhow::Result;
use serde::Serialize;

/// Full inspection result for a container image.
#[derive(Debug, Clone, Serialize)]
pub struct ImageInfo {
    /// Image reference as provided by the user (e.g. "nginx:latest", "./image.tar")
    pub name: String,

    /// Image tag (e.g. "latest")
    pub tag: Option<String>,

    /// Target architecture (e.g. "amd64")
    pub architecture: Option<String>,

    /// Total size across all layers, in bytes
    pub total_size: u64,

    /// Layers in order (base first)
    pub layers: Vec<LayerInfo>,
}

/// Metadata about a single layer in an image.
#[derive(Debug, Clone, Serialize)]
pub struct LayerInfo {
    /// Layer digest (e.g. sha256:abc123...)
    pub digest: String,

    /// The Dockerfile command that created this layer (if available)
    pub created_by: Option<String>,

    /// Total size of files in this layer, in bytes
    pub size: u64,

    /// Files in this layer (populated separately via list_files)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<FileEntry>,
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
    /// Inspect an image and return full metadata with layers.
    fn inspect(&mut self, image: &str) -> Result<ImageInfo>;

    /// List all files in a specific layer.
    fn list_files(&mut self, layer: &LayerInfo) -> Result<Vec<FileEntry>>;
}
