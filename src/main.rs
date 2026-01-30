use clap::{CommandFactory, Parser};
use comfy_table::{Attribute, Cell, Color, Table, presets::UTF8_FULL_CONDENSED};
use owo_colors::OwoColorize;
use pm3::cli::{Cli, Command};
use pm3::protocol::{ProcessStatus, Request, Response};

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    let cli = Cli::parse();

    if cli.daemon {
        let paths = pm3::paths::Paths::new()?;
        pm3::daemon::run(paths).await?;
    } else if let Some(command) = cli.command {
        let paths = pm3::paths::Paths::new()?;

        // Log command needs special handling for streaming responses
        if let Command::Log { name, lines, follow } = command {
            let request = Request::Log { name, lines, follow };
            let json_mode = cli.json;

            let final_response = pm3::client::send_log_request(&paths, &request, |response| {
                if json_mode {
                    print_response_json(response);
                } else {
                    print_response(response);
                }
            })?;

            // Print final response if any (Success or Error)
            if let Some(response) = final_response {
                if json_mode {
                    print_response_json(&response);
                } else {
                    // Don't print "ok" for success with no message on log command
                    if !matches!(&response, Response::Success { message: None }) {
                        print_response(&response);
                    }
                }
            }
        } else {
            let request = command_to_request(command)?;
            let response = pm3::client::send_request(&paths, &request)?;
            if cli.json {
                print_response_json(&response);
            } else {
                print_response(&response);
                if should_auto_list(&request) {
                    let list_resp = pm3::client::send_request(&paths, &Request::List)?;
                    print_response(&list_resp);
                }
            }
        }
    } else {
        Cli::command().print_help()?;
    }

    Ok(())
}

fn should_auto_list(request: &Request) -> bool {
    matches!(
        request,
        Request::Start { .. }
            | Request::Stop { .. }
            | Request::Restart { .. }
            | Request::Reload { .. }
    )
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

fn print_response_json(response: &Response) {
    let json = serde_json::to_string(response).expect("failed to serialize response");
    println!("{json}");
}

fn status_color(status: &ProcessStatus) -> Color {
    match status {
        ProcessStatus::Online => Color::Green,
        ProcessStatus::Starting => Color::Yellow,
        ProcessStatus::Unhealthy => Color::Magenta,
        ProcessStatus::Stopped => Color::Reset,
        ProcessStatus::Errored => Color::Red,
    }
}

fn print_response(response: &Response) {
    match response {
        Response::Success { message } => {
            if let Some(msg) = message {
                println!("{}", msg.green());
            } else {
                println!("{}", "ok".green());
            }
        }
        Response::Error { message } => {
            eprintln!("{} {}", "error:".red().bold(), message);
        }
        Response::ProcessList { processes } => {
            if processes.is_empty() {
                println!("{}", "no processes running".yellow());
            } else {
                let mut table = Table::new();
                table.load_preset(UTF8_FULL_CONDENSED);
                table.set_header(vec![
                    Cell::new("name").add_attribute(Attribute::Bold),
                    Cell::new("pid").add_attribute(Attribute::Bold),
                    Cell::new("status").add_attribute(Attribute::Bold),
                    Cell::new("uptime").add_attribute(Attribute::Bold),
                    Cell::new("restarts").add_attribute(Attribute::Bold),
                ]);
                for p in processes {
                    let pid = p
                        .pid
                        .map(|id| id.to_string())
                        .unwrap_or_else(|| "-".to_string());
                    let uptime = format_uptime(p.uptime);
                    let status = p.status.to_string();
                    let restarts = p.restarts.to_string();
                    let restarts_cell = if p.restarts > 0 {
                        Cell::new(&restarts).fg(Color::Yellow)
                    } else {
                        Cell::new(&restarts)
                    };
                    table.add_row(vec![
                        Cell::new(&p.name).fg(Color::Cyan),
                        Cell::new(&pid),
                        Cell::new(&status).fg(status_color(&p.status)),
                        Cell::new(&uptime),
                        restarts_cell,
                    ]);
                }
                println!("{table}");
            }
        }
        Response::ProcessDetail { info } => {
            let status_str = info.status.to_string();
            let colored_status = match info.status {
                ProcessStatus::Online => status_str.green().to_string(),
                ProcessStatus::Starting => status_str.yellow().to_string(),
                ProcessStatus::Unhealthy => status_str.magenta().to_string(),
                ProcessStatus::Stopped => status_str.to_string(),
                ProcessStatus::Errored => status_str.red().to_string(),
            };
            println!("{}: {}", info.name.cyan().bold(), colored_status);
            println!("  {} {}", "command:".dimmed(), info.command);
            if let Some(pid) = info.pid {
                println!("  {} {pid}", "pid:".dimmed());
            }
            if let Some(cwd) = &info.cwd {
                println!("  {} {cwd}", "cwd:".dimmed());
            }
        }
        Response::LogLine { name, line } => {
            if let Some(name) = name {
                println!("{} {line}", format!("[{name}]").cyan().bold());
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
