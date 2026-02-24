mod cmd;
mod config;
mod inspector;
mod probe;
mod progress;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "peel")]
#[command(about = "A container image layer inspection tool")]
#[command(version)]
struct Cli {
    /// Override runtime selection (docker, podman, containerd)
    #[arg(long, global = true)]
    runtime: Option<String>,

    /// Output as JSON (optionally to a file)
    #[arg(long, global = true, num_args = 0..=1, default_missing_value = "-")]
    json: Option<String>,

    /// Use OCI/Docker API instead of direct storage access (no root needed, slower)
    #[arg(long, global = true)]
    use_oci: bool,

    /// Disable the interactive web report
    #[arg(long, global = true)]
    no_web: bool,

    /// Don't auto-escalate to sudo for direct storage access
    #[arg(long, global = true)]
    no_sudo: bool,

    #[command(subcommand)]
    command: Option<Commands>,

    /// Image name or path to a tar archive (shorthand for `peel inspect <image>`)
    image: Option<String>,
}

#[derive(Subcommand)]
enum Commands {
    /// Inspect layers of a container image
    Inspect {
        /// Image name or path to a tar archive
        image: String,
    },

    /// Detect installed container runtimes
    Probe,

    /// Update peel to the latest version
    Update,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // Resolve: `peel <image>` is shorthand for `peel inspect <image>`
    let image_to_inspect = match &cli.command {
        Some(Commands::Inspect { image }) => Some(image.clone()),
        Some(_) => None,
        None => cli.image.clone(),
    };

    if cli.command.is_none() && image_to_inspect.is_none() {
        Cli::parse_from(["peel", "--help"]);
        return Ok(());
    }

    if let Some(image) = &image_to_inspect {
        let web = !cli.no_web && cli.json.is_none();
        cmd::inspect::run(image, cli.use_oci, cli.json.as_deref(), cli.runtime, web, cli.no_sudo)?;
    } else if matches!(cli.command, Some(Commands::Probe)) {
        cmd::probe::run(cli.json.is_some(), cli.runtime)?;
    } else if matches!(cli.command, Some(Commands::Update)) {
        cmd::self_update::run()?;
    }

    Ok(())
}
