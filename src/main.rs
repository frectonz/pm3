use clap::Parser;
use pm3::cli::{Cli, Command};
use pm3::protocol::{Request, Response};

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    let cli = Cli::parse();

    if cli.daemon {
        let paths = pm3::paths::Paths::new()?;
        pm3::daemon::run(paths).await?;
    } else if let Some(command) = cli.command {
        let paths = pm3::paths::Paths::new()?;
        let request = command_to_request(command)?;
        let response = pm3::client::send_request(&paths, &request)?;
        print_response(&response);
    } else {
        println!("pm3: no command specified. Use --help for usage.");
    }

    Ok(())
}

fn command_to_request(command: Command) -> color_eyre::Result<Request> {
    match command {
        Command::Start { names, env } => {
            let config_path = std::env::current_dir()?.join("pm3.toml");
            let configs = pm3::config::load_config(&config_path)
                .map_err(|e| color_eyre::eyre::eyre!("{e}"))?;
            Ok(Request::Start {
                configs,
                names: Command::optional_names(names),
                env,
            })
        }
        Command::Stop { names } => Ok(Request::Stop {
            names: Command::optional_names(names),
        }),
        Command::Restart { names } => Ok(Request::Restart {
            names: Command::optional_names(names),
        }),
        Command::List => Ok(Request::List),
        Command::Kill => Ok(Request::Kill),
        Command::Reload { names } => Ok(Request::Reload {
            names: Command::optional_names(names),
        }),
        Command::Info { name } => Ok(Request::Info { name }),
        Command::Signal { name, signal } => Ok(Request::Signal { name, signal }),
        Command::Save => Ok(Request::Save),
        Command::Resurrect => Ok(Request::Resurrect),
        Command::Flush { names } => Ok(Request::Flush {
            names: Command::optional_names(names),
        }),
        Command::Log {
            name,
            lines,
            follow,
        } => Ok(Request::Log {
            name,
            lines,
            follow,
        }),
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
                println!(
                    "{:<20} {:<10} {:<10} {:<12} {:<10}",
                    "name", "pid", "status", "uptime", "restarts"
                );
                for p in processes {
                    let pid = p
                        .pid
                        .map(|id| id.to_string())
                        .unwrap_or_else(|| "-".to_string());
                    let uptime = format_uptime(p.uptime);
                    println!(
                        "{:<20} {:<10} {:<10} {:<12} {:<10}",
                        p.name, pid, p.status, uptime, p.restarts
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

fn format_uptime(seconds: Option<u64>) -> String {
    match seconds {
        None => "-".to_string(),
        Some(s) if s < 60 => format!("{s}s"),
        Some(s) if s < 3600 => format!("{}m {}s", s / 60, s % 60),
        Some(s) if s < 86400 => format!("{}h {}m", s / 3600, (s % 3600) / 60),
        Some(s) => format!("{}d {}h", s / 86400, (s % 86400) / 3600),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_uptime_none() {
        assert_eq!(format_uptime(None), "-");
    }

    #[test]
    fn test_format_uptime_seconds() {
        assert_eq!(format_uptime(Some(0)), "0s");
        assert_eq!(format_uptime(Some(30)), "30s");
        assert_eq!(format_uptime(Some(59)), "59s");
    }

    #[test]
    fn test_format_uptime_minutes() {
        assert_eq!(format_uptime(Some(60)), "1m 0s");
        assert_eq!(format_uptime(Some(90)), "1m 30s");
        assert_eq!(format_uptime(Some(3599)), "59m 59s");
    }

    #[test]
    fn test_format_uptime_hours() {
        assert_eq!(format_uptime(Some(3600)), "1h 0m");
        assert_eq!(format_uptime(Some(7260)), "2h 1m");
        assert_eq!(format_uptime(Some(86399)), "23h 59m");
    }

    #[test]
    fn test_format_uptime_days() {
        assert_eq!(format_uptime(Some(86400)), "1d 0h");
        assert_eq!(format_uptime(Some(90000)), "1d 1h");
        assert_eq!(format_uptime(Some(172800)), "2d 0h");
    }
}
