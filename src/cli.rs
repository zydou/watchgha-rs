//! Command-line interface definition (clap), mirroring the original's click
//! options and arguments.

use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "watchgha",
    version,
    about = "Watch GitHub Action runs.",
    long_about = "Watch GitHub Action runs.\n\n\
        Repeatedly gets the latest status and redraws the screen, until all of the\n\
        jobs are complete.\n\n\
        REPO is a local directory or GitHub URL, defaulting to \".\".\n\n\
        BRANCH is defaulted from the git repo."
)]
pub struct Cli {
    /// The commit SHA to use. Must be a full SHA.
    #[arg(long)]
    pub sha: Option<String>,

    /// How many seconds between refreshes.
    #[arg(long, default_value_t = 15)]
    pub poll: i64,

    /// Wait for jobs to start.
    #[arg(long = "wait", visible_alias = "wait-for-start")]
    pub wait: bool,

    /// Words to limit the workflows shown. Only workflows with these comma
    /// separated case insensitive substrings in their names will be shown.
    #[arg(long)]
    pub only: Option<String>,

    /// A message to display at the top of the screen.
    #[arg(long)]
    pub message: Option<String>,

    /// A local directory or GitHub URL, defaulting to ".".
    #[arg(default_value = ".")]
    pub repo: String,

    /// Defaults from the git repo.
    pub branch: Option<String>,
}

impl Cli {
    /// Parse `--only` into the list of lowercase-free trimmed words, or `None`
    /// when the option was not supplied. The original splits on commas and
    /// trims whitespace around each word.
    pub fn only_words(&self) -> Option<Vec<String>> {
        self.only
            .as_ref()
            .map(|only| only.split(',').map(|w| w.trim().to_string()).collect())
    }
}
