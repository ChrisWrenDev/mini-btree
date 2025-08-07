use anyhow::Result;

use clap::Parser;

#[derive(clap::Subcommand, Debug)]
enum Action {
    /// Check.
    Check,
    /// Build and serve book.
    Book,
    /// Install necessary tools for development.
    InstallTools,
    /// Show environment variables.
    Show,
    /// Run CI jobs
    Ci,
    /// Sync starter repo and reference solution.
    Sync,
    /// Check starter code
    Scheck,
    /// Copy test cases
    CopyTest(CopyTestAction),
}

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[command(subcommand)]
    action: Action,
}

fn main() -> Result<()> {
    let args = Args::parse();

    match args.action {
        _ => {}
    }
}
