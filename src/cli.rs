use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "pm3", about = "A Rust process manager")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    Start {
        names: Vec<String>,
        #[arg(long)]
        env: Option<String>,
    },
    Stop {
        names: Vec<String>,
    },
    Restart {
        names: Vec<String>,
    },
    #[command(visible_alias = "view")]
    List,
    Kill,
    Reload {
        names: Vec<String>,
    },
    Info {
        name: String,
    },
    Signal {
        name: String,
        signal: String,
    },
    Save,
    Resurrect,
    Flush {
        names: Vec<String>,
    },
    Log {
        name: Option<String>,
        #[arg(long, default_value_t = 15)]
        lines: usize,
        #[arg(short, long)]
        follow: bool,
    },
}

impl Command {
    pub fn optional_names(names: Vec<String>) -> Option<Vec<String>> {
        if names.is_empty() {
            None
        } else {
            Some(names)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Core subcommand parsing

    #[test]
    fn test_start_no_args() {
        let cli = Cli::try_parse_from(["pm3", "start"]).unwrap();
        match cli.command {
            Command::Start { names, env } => {
                assert!(names.is_empty());
                assert!(env.is_none());
            }
            _ => panic!("expected Start"),
        }
    }

    #[test]
    fn test_stop_no_args() {
        let cli = Cli::try_parse_from(["pm3", "stop"]).unwrap();
        match cli.command {
            Command::Stop { names } => assert!(names.is_empty()),
            _ => panic!("expected Stop"),
        }
    }

    #[test]
    fn test_restart_no_args() {
        let cli = Cli::try_parse_from(["pm3", "restart"]).unwrap();
        match cli.command {
            Command::Restart { names } => assert!(names.is_empty()),
            _ => panic!("expected Restart"),
        }
    }

    #[test]
    fn test_list() {
        let cli = Cli::try_parse_from(["pm3", "list"]).unwrap();
        assert!(matches!(cli.command, Command::List));
    }

    #[test]
    fn test_kill() {
        let cli = Cli::try_parse_from(["pm3", "kill"]).unwrap();
        assert!(matches!(cli.command, Command::Kill));
    }

    // Names handling

    #[test]
    fn test_start_with_name() {
        let cli = Cli::try_parse_from(["pm3", "start", "web"]).unwrap();
        match cli.command {
            Command::Start { names, .. } => assert_eq!(names, vec!["web"]),
            _ => panic!("expected Start"),
        }
    }

    #[test]
    fn test_start_with_multiple_names() {
        let cli = Cli::try_parse_from(["pm3", "start", "web", "api"]).unwrap();
        match cli.command {
            Command::Start { names, .. } => assert_eq!(names, vec!["web", "api"]),
            _ => panic!("expected Start"),
        }
    }

    #[test]
    fn test_start_with_env() {
        let cli = Cli::try_parse_from(["pm3", "start", "--env", "production"]).unwrap();
        match cli.command {
            Command::Start { names, env } => {
                assert!(names.is_empty());
                assert_eq!(env.as_deref(), Some("production"));
            }
            _ => panic!("expected Start"),
        }
    }

    #[test]
    fn test_start_with_name_and_env() {
        let cli = Cli::try_parse_from(["pm3", "start", "web", "--env", "staging"]).unwrap();
        match cli.command {
            Command::Start { names, env } => {
                assert_eq!(names, vec!["web"]);
                assert_eq!(env.as_deref(), Some("staging"));
            }
            _ => panic!("expected Start"),
        }
    }

    // Remaining subcommands

    #[test]
    fn test_reload() {
        let cli = Cli::try_parse_from(["pm3", "reload"]).unwrap();
        match cli.command {
            Command::Reload { names } => assert!(names.is_empty()),
            _ => panic!("expected Reload"),
        }

        let cli = Cli::try_parse_from(["pm3", "reload", "web"]).unwrap();
        match cli.command {
            Command::Reload { names } => assert_eq!(names, vec!["web"]),
            _ => panic!("expected Reload"),
        }
    }

    #[test]
    fn test_info() {
        let cli = Cli::try_parse_from(["pm3", "info", "web"]).unwrap();
        match cli.command {
            Command::Info { name } => assert_eq!(name, "web"),
            _ => panic!("expected Info"),
        }
    }

    #[test]
    fn test_signal() {
        let cli = Cli::try_parse_from(["pm3", "signal", "web", "SIGHUP"]).unwrap();
        match cli.command {
            Command::Signal { name, signal } => {
                assert_eq!(name, "web");
                assert_eq!(signal, "SIGHUP");
            }
            _ => panic!("expected Signal"),
        }
    }

    #[test]
    fn test_save() {
        let cli = Cli::try_parse_from(["pm3", "save"]).unwrap();
        assert!(matches!(cli.command, Command::Save));
    }

    #[test]
    fn test_resurrect() {
        let cli = Cli::try_parse_from(["pm3", "resurrect"]).unwrap();
        assert!(matches!(cli.command, Command::Resurrect));
    }

    #[test]
    fn test_flush() {
        let cli = Cli::try_parse_from(["pm3", "flush"]).unwrap();
        match cli.command {
            Command::Flush { names } => assert!(names.is_empty()),
            _ => panic!("expected Flush"),
        }

        let cli = Cli::try_parse_from(["pm3", "flush", "web"]).unwrap();
        match cli.command {
            Command::Flush { names } => assert_eq!(names, vec!["web"]),
            _ => panic!("expected Flush"),
        }
    }

    #[test]
    fn test_log_defaults() {
        let cli = Cli::try_parse_from(["pm3", "log"]).unwrap();
        match cli.command {
            Command::Log {
                name,
                lines,
                follow,
            } => {
                assert!(name.is_none());
                assert_eq!(lines, 15);
                assert!(!follow);
            }
            _ => panic!("expected Log"),
        }
    }

    #[test]
    fn test_log_with_options() {
        let cli = Cli::try_parse_from(["pm3", "log", "web", "--lines", "50", "-f"]).unwrap();
        match cli.command {
            Command::Log {
                name,
                lines,
                follow,
            } => {
                assert_eq!(name.as_deref(), Some("web"));
                assert_eq!(lines, 50);
                assert!(follow);
            }
            _ => panic!("expected Log"),
        }
    }

    #[test]
    fn test_list_view_alias() {
        let cli = Cli::try_parse_from(["pm3", "view"]).unwrap();
        assert!(matches!(cli.command, Command::List));
    }

    // Error cases

    #[test]
    fn test_unknown_subcommand() {
        assert!(Cli::try_parse_from(["pm3", "bogus"]).is_err());
    }

    #[test]
    fn test_info_missing_name() {
        assert!(Cli::try_parse_from(["pm3", "info"]).is_err());
    }

    #[test]
    fn test_signal_missing_args() {
        assert!(Cli::try_parse_from(["pm3", "signal"]).is_err());
    }

    // Helper

    #[test]
    fn test_optional_names() {
        assert_eq!(Command::optional_names(vec![]), None);
        assert_eq!(
            Command::optional_names(vec!["web".to_string()]),
            Some(vec!["web".to_string()])
        );
    }
}
