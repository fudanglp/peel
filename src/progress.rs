use crossterm::style::Stylize;
use indicatif::{ProgressBar, ProgressStyle};

/// A simple spinner for long-running stages.
pub struct Spinner {
    bar: ProgressBar,
}

fn spinner_style() -> ProgressStyle {
    ProgressStyle::default_spinner()
        .template("{spinner:.dim} {msg}")
        .unwrap()
}

impl Spinner {
    pub fn new(message: impl Into<String>) -> Self {
        let bar = ProgressBar::new_spinner();
        bar.set_style(spinner_style());
        bar.set_message(message.into());
        bar.enable_steady_tick(std::time::Duration::from_millis(80));
        Self { bar }
    }

    pub fn set_message(&self, message: impl Into<String>) {
        self.bar.set_message(message.into());
    }

    /// Return a cheap clone of the inner progress bar (shares the same Arc).
    pub fn clone_bar(&self) -> ProgressBar {
        self.bar.clone()
    }

    /// Clear the spinner and print a `✔ message` line to stderr.
    pub fn finish(self, message: impl Into<String>) {
        self.bar.disable_steady_tick();
        self.bar.finish_and_clear();
        eprintln!("{} {}", "✔".green(), message.into());
    }
}
