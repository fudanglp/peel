use anyhow::Result;

use super::{FileEntry, Inspector, LayerInfo};

/// Reads layers via the OCI image layout or container runtime API.
/// Cross-platform, no root needed, but slower (requires API calls).
pub struct OciInspector {
    runtime_cmd: String,
}

impl OciInspector {
    pub fn new(runtime_cmd: String) -> Self {
        Self { runtime_cmd }
    }
}

impl Inspector for OciInspector {
    fn list_layers(&mut self, _image: &str) -> Result<Vec<LayerInfo>> {
        // TODO: use runtime CLI (docker/podman) to inspect image and list layers
        let _ = &self.runtime_cmd;
        anyhow::bail!("OCI inspector not yet implemented")
    }

    fn list_files(&mut self, _layer: &LayerInfo) -> Result<Vec<FileEntry>> {
        // TODO: extract layer tar via runtime CLI and list contents
        anyhow::bail!("OCI inspector not yet implemented")
    }
}
