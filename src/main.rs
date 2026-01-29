use clap::Parser;
use pm3::cli::{Cli, Command};
use pm3::protocol::{Request, Response};
use std::collections::HashMap;

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    let cli = Cli::parse();

    if cli.daemon {
        let paths = pm3::paths::Paths::new()?;
        pm3::daemon::run(paths).await?;
    } else if let Some(command) = cli.command {
        let paths = pm3::paths::Paths::new()?;
        let request = command_to_request(command);
        let response = pm3::client::send_request(&paths, &request)?;
        print_response(&response);
    } else {
        println!("pm3: no command specified. Use --help for usage.");
    }

    Ok(())
}

fn command_to_request(command: Command) -> Request {
    match command {
        Command::Start { names, env } => Request::Start {
            configs: HashMap::new(),
            names: Command::optional_names(names),
            env,
        },
        Command::Stop { names } => Request::Stop {
            names: Command::optional_names(names),
        },
        Command::Restart { names } => Request::Restart {
            names: Command::optional_names(names),
        },
        Command::List => Request::List,
        Command::Kill => Request::Kill,
        Command::Reload { names } => Request::Reload {
            names: Command::optional_names(names),
        },
        Command::Info { name } => Request::Info { name },
        Command::Signal { name, signal } => Request::Signal { name, signal },
        Command::Save => Request::Save,
        Command::Resurrect => Request::Resurrect,
        Command::Flush { names } => Request::Flush {
            names: Command::optional_names(names),
        },
        Command::Log {
            name,
            lines,
            follow,
        } => Request::Log {
            name,
            lines,
            follow,
        },
    }
}

fn print_response(response: &Response) {
    match response {
        Response::Success { message } => {
            if let Some(msg) = message {
                println!("{msg}");
            } else {
                println!("ok");
            }
        }
        Response::Error { message } => {
            eprintln!("error: {message}");
        }
        Response::ProcessList { processes } => {
            if processes.is_empty() {
                println!("no processes running");
            } else {
                for p in processes {
                    println!(
                        "{}\t{}\t{:?}",
                        p.name,
                        p.pid.map(|p| p.to_string()).unwrap_or_default(),
                        p.status
                    );
                }
            }
        }
        Response::ProcessDetail { info } => {
            println!("{}: {:?}", info.name, info.status);
            println!("  command: {}", info.command);
            if let Some(pid) = info.pid {
                println!("  pid: {pid}");
            }
            if let Some(cwd) = &info.cwd {
                println!("  cwd: {cwd}");
            }
        }
        Response::LogLine { name, line } => {
            if let Some(name) = name {
                println!("[{name}] {line}");
            } else {
                println!("{line}");
            }
        }
    }
}
