use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestartPolicy {
    OnFailure,
    Always,
    Never,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum EnvFile {
    Single(String),
    Multiple(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum Watch {
    Enabled(bool),
    Path(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProcessConfig {
    pub command: String,
    pub cwd: Option<String>,
    pub env: Option<HashMap<String, String>>,
    pub env_file: Option<EnvFile>,
    pub health_check: Option<String>,
    pub kill_timeout: Option<u64>,
    pub kill_signal: Option<String>,
    pub max_restarts: Option<u32>,
    pub max_memory: Option<String>,
    pub min_uptime: Option<u64>,
    pub stop_exit_codes: Option<Vec<i32>>,
    pub watch: Option<Watch>,
    pub watch_ignore: Option<Vec<String>>,
    pub depends_on: Option<Vec<String>>,
    pub restart: Option<RestartPolicy>,
    pub group: Option<String>,
    pub pre_start: Option<String>,
    pub post_stop: Option<String>,
    pub notify: Option<String>,
    pub cron_restart: Option<String>,
    pub log_date_format: Option<String>,
    pub environments: HashMap<String, HashMap<String, String>>,
}

#[derive(Debug, Deserialize)]
struct RawProcessConfig {
    command: String,
    cwd: Option<String>,
    env: Option<HashMap<String, String>>,
    env_file: Option<EnvFile>,
    health_check: Option<String>,
    kill_timeout: Option<u64>,
    kill_signal: Option<String>,
    max_restarts: Option<u32>,
    max_memory: Option<String>,
    min_uptime: Option<u64>,
    stop_exit_codes: Option<Vec<i32>>,
    watch: Option<Watch>,
    watch_ignore: Option<Vec<String>>,
    depends_on: Option<Vec<String>>,
    restart: Option<RestartPolicy>,
    group: Option<String>,
    pre_start: Option<String>,
    post_stop: Option<String>,
    notify: Option<String>,
    cron_restart: Option<String>,
    log_date_format: Option<String>,
    #[serde(flatten)]
    extra: HashMap<String, toml::Value>,
}

#[derive(Debug, PartialEq)]
pub enum ConfigError {
    Empty,
    TomlParse(String),
    UnknownField { process: String, field: String },
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Empty => write!(f, "config file is empty"),
            ConfigError::TomlParse(msg) => write!(f, "TOML parse error: {msg}"),
            ConfigError::UnknownField { process, field } => {
                write!(f, "unknown field `{field}` in process `{process}`")
            }
        }
    }
}

impl std::error::Error for ConfigError {}

pub fn parse_config(content: &str) -> Result<HashMap<String, ProcessConfig>, ConfigError> {
    let table: HashMap<String, toml::Value> =
        toml::from_str(content).map_err(|e| ConfigError::TomlParse(e.to_string()))?;

    if table.is_empty() {
        return Err(ConfigError::Empty);
    }

    let mut configs = HashMap::new();

    for (name, value) in table {
        let raw: RawProcessConfig = value
            .try_into()
            .map_err(|e: toml::de::Error| ConfigError::TomlParse(e.to_string()))?;

        let mut environments: HashMap<String, HashMap<String, String>> = HashMap::new();

        for (key, val) in &raw.extra {
            if let Some(env_name) = key.strip_prefix("env_") {
                let env_map: HashMap<String, String> = val
                    .clone()
                    .try_into()
                    .map_err(|e: toml::de::Error| ConfigError::TomlParse(e.to_string()))?;
                environments.insert(env_name.to_string(), env_map);
            } else {
                return Err(ConfigError::UnknownField {
                    process: name.clone(),
                    field: key.clone(),
                });
            }
        }

        configs.insert(
            name,
            ProcessConfig {
                command: raw.command,
                cwd: raw.cwd,
                env: raw.env,
                env_file: raw.env_file,
                health_check: raw.health_check,
                kill_timeout: raw.kill_timeout,
                kill_signal: raw.kill_signal,
                max_restarts: raw.max_restarts,
                max_memory: raw.max_memory,
                min_uptime: raw.min_uptime,
                stop_exit_codes: raw.stop_exit_codes,
                watch: raw.watch,
                watch_ignore: raw.watch_ignore,
                depends_on: raw.depends_on,
                restart: raw.restart,
                group: raw.group,
                pre_start: raw.pre_start,
                post_stop: raw.post_stop,
                notify: raw.notify,
                cron_restart: raw.cron_restart,
                log_date_format: raw.log_date_format,
                environments,
            },
        );
    }

    Ok(configs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_toml_parses() {
        let input = r#"
[web]
command = "node server.js"
cwd = "/app"
env = { NODE_ENV = "production", PORT = "3000" }
env_file = ".env"
health_check = "http://localhost:3000/health"
kill_timeout = 5000
kill_signal = "SIGTERM"
max_restarts = 10
max_memory = "512M"
min_uptime = 1000
stop_exit_codes = [0, 143]
watch = true
watch_ignore = ["node_modules", ".git"]
depends_on = ["db"]
restart = "on_failure"
group = "backend"
pre_start = "npm run migrate"
post_stop = "echo stopped"
notify = "slack"
cron_restart = "0 3 * * *"
log_date_format = "%Y-%m-%d %H:%M:%S"

[web.env_production]
DATABASE_URL = "postgres://prod/db"
"#;
        let configs = parse_config(input).unwrap();
        assert_eq!(configs.len(), 1);

        let web = &configs["web"];
        assert_eq!(web.command, "node server.js");
        assert_eq!(web.cwd.as_deref(), Some("/app"));
        assert_eq!(
            web.env.as_ref().unwrap().get("NODE_ENV").unwrap(),
            "production"
        );
        assert_eq!(web.env_file, Some(EnvFile::Single(".env".to_string())));
        assert_eq!(
            web.health_check.as_deref(),
            Some("http://localhost:3000/health")
        );
        assert_eq!(web.kill_timeout, Some(5000));
        assert_eq!(web.kill_signal.as_deref(), Some("SIGTERM"));
        assert_eq!(web.max_restarts, Some(10));
        assert_eq!(web.max_memory.as_deref(), Some("512M"));
        assert_eq!(web.min_uptime, Some(1000));
        assert_eq!(web.stop_exit_codes, Some(vec![0, 143]));
        assert_eq!(web.watch, Some(Watch::Enabled(true)));
        assert_eq!(
            web.watch_ignore,
            Some(vec!["node_modules".to_string(), ".git".to_string()])
        );
        assert_eq!(web.depends_on, Some(vec!["db".to_string()]));
        assert_eq!(web.restart, Some(RestartPolicy::OnFailure));
        assert_eq!(web.group.as_deref(), Some("backend"));
        assert_eq!(web.pre_start.as_deref(), Some("npm run migrate"));
        assert_eq!(web.post_stop.as_deref(), Some("echo stopped"));
        assert_eq!(web.notify.as_deref(), Some("slack"));
        assert_eq!(web.cron_restart.as_deref(), Some("0 3 * * *"));
        assert_eq!(
            web.log_date_format.as_deref(),
            Some("%Y-%m-%d %H:%M:%S")
        );
        assert_eq!(
            web.environments
                .get("production")
                .unwrap()
                .get("DATABASE_URL")
                .unwrap(),
            "postgres://prod/db"
        );
    }

    #[test]
    fn test_missing_command_errors() {
        let input = r#"
[web]
cwd = "/app"
"#;
        let result = parse_config(input);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ConfigError::TomlParse(_)));
    }

    #[test]
    fn test_unknown_field_errors() {
        let input = r#"
[web]
command = "node server.js"
bogus_field = "x"
"#;
        let result = parse_config(input);
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err(),
            ConfigError::UnknownField {
                process: "web".to_string(),
                field: "bogus_field".to_string(),
            }
        );
    }

    #[test]
    fn test_empty_file_errors() {
        let result = parse_config("");
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), ConfigError::Empty);
    }

    #[test]
    fn test_optional_fields_default() {
        let input = r#"
[api]
command = "cargo run"
"#;
        let configs = parse_config(input).unwrap();
        let api = &configs["api"];
        assert_eq!(api.command, "cargo run");
        assert!(api.cwd.is_none());
        assert!(api.env.is_none());
        assert!(api.env_file.is_none());
        assert!(api.health_check.is_none());
        assert!(api.kill_timeout.is_none());
        assert!(api.kill_signal.is_none());
        assert!(api.max_restarts.is_none());
        assert!(api.max_memory.is_none());
        assert!(api.min_uptime.is_none());
        assert!(api.stop_exit_codes.is_none());
        assert!(api.watch.is_none());
        assert!(api.watch_ignore.is_none());
        assert!(api.depends_on.is_none());
        assert!(api.restart.is_none());
        assert!(api.group.is_none());
        assert!(api.pre_start.is_none());
        assert!(api.post_stop.is_none());
        assert!(api.notify.is_none());
        assert!(api.cron_restart.is_none());
        assert!(api.log_date_format.is_none());
        assert!(api.environments.is_empty());
    }

    #[test]
    fn test_multiple_sections() {
        let input = r#"
[web]
command = "node server.js"

[api]
command = "cargo run"

[worker]
command = "python worker.py"
"#;
        let configs = parse_config(input).unwrap();
        assert_eq!(configs.len(), 3);
        assert!(configs.contains_key("web"));
        assert!(configs.contains_key("api"));
        assert!(configs.contains_key("worker"));
        assert_eq!(configs["web"].command, "node server.js");
        assert_eq!(configs["api"].command, "cargo run");
        assert_eq!(configs["worker"].command, "python worker.py");
    }

    #[test]
    fn test_env_file_string_and_array() {
        let single = r#"
[web]
command = "node server.js"
env_file = ".env"
"#;
        let configs = parse_config(single).unwrap();
        assert_eq!(
            configs["web"].env_file,
            Some(EnvFile::Single(".env".to_string()))
        );

        let multi = r#"
[web]
command = "node server.js"
env_file = [".env", ".env.local"]
"#;
        let configs = parse_config(multi).unwrap();
        assert_eq!(
            configs["web"].env_file,
            Some(EnvFile::Multiple(vec![
                ".env".to_string(),
                ".env.local".to_string()
            ]))
        );
    }

    #[test]
    fn test_watch_bool_and_string() {
        let bool_input = r#"
[web]
command = "node server.js"
watch = true
"#;
        let configs = parse_config(bool_input).unwrap();
        assert_eq!(configs["web"].watch, Some(Watch::Enabled(true)));

        let path_input = r#"
[web]
command = "node server.js"
watch = "./src"
"#;
        let configs = parse_config(path_input).unwrap();
        assert_eq!(configs["web"].watch, Some(Watch::Path("./src".to_string())));
    }

    #[test]
    fn test_restart_policy_variants() {
        let input = r#"
[a]
command = "a"
restart = "on_failure"

[b]
command = "b"
restart = "always"

[c]
command = "c"
restart = "never"
"#;
        let configs = parse_config(input).unwrap();
        assert_eq!(configs["a"].restart, Some(RestartPolicy::OnFailure));
        assert_eq!(configs["b"].restart, Some(RestartPolicy::Always));
        assert_eq!(configs["c"].restart, Some(RestartPolicy::Never));
    }

    #[test]
    fn test_env_environment_sections() {
        let input = r#"
[web]
command = "node server.js"

[web.env_production]
DATABASE_URL = "postgres://prod/db"
API_KEY = "prod-key"

[web.env_staging]
DATABASE_URL = "postgres://staging/db"
"#;
        let configs = parse_config(input).unwrap();
        let web = &configs["web"];

        assert_eq!(web.environments.len(), 2);
        let prod = web.environments.get("production").unwrap();
        assert_eq!(prod.get("DATABASE_URL").unwrap(), "postgres://prod/db");
        assert_eq!(prod.get("API_KEY").unwrap(), "prod-key");

        let staging = web.environments.get("staging").unwrap();
        assert_eq!(
            staging.get("DATABASE_URL").unwrap(),
            "postgres://staging/db"
        );
    }
}
