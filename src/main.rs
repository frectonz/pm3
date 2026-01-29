mod cli;
mod config;
mod paths;
mod protocol;

use clap::Parser;
use cli::Cli;

fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    let cli = Cli::parse();
    println!("{cli:?}");
    Ok(())
}
