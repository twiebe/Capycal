use std::process::ExitCode;

use capycal::cli::{self, Cli};
use clap::Parser;

#[tokio::main]
async fn main() -> ExitCode {
    cli::execute(Cli::parse()).await
}
