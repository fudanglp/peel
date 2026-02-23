use std::process::Command;

use anyhow::{Context, Result};

pub fn run() -> Result<()> {
    let updater = format!("{}-update", env!("CARGO_PKG_NAME"));

    let status = Command::new(&updater)
        .status()
        .with_context(|| {
            format!(
                "Could not find `{updater}`. \
                 Reinstall peel via the shell installer to get the updater:\n\n  \
                 curl --proto '=https' --tlsv1.2 -LsSf \
                 https://github.com/fudanglp/peel/releases/latest/download/peel-installer.sh | sh"
            )
        })?;

    if !status.success() {
        anyhow::bail!("Update failed (exit code: {})", status.code().unwrap_or(-1));
    }

    Ok(())
}
