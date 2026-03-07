use crate::env_file;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestartPolicy {
    OnFailure,
    Always,
    Never,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum EnvFile {
    Single(String),
    Multiple(Vec<String>),
}

impl EnvFile {
    pub fn paths(&self) -> Vec<&str> {
        match self {
            EnvFile::Single(p) => vec![p.as_str()],
            EnvFile::Multiple(ps) => ps.iter().map(|s| s.as_str()).collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Watch {
    Enabled(bool),
    Path(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    pub watch_debounce: Option<u64>,
    pub depends_on: Option<Vec<String>>,
    pub restart: Option<RestartPolicy>,
    pub group: Option<String>,
    pub pre_start: Option<String>,
    pub post_stop: Option<String>,
    pub cron_restart: Option<String>,
    pub log_date_format: Option<String>,
    pub instances: Option<u32>,
    pub environments: HashMap<String, HashMap<String, String>>,
}

impl ProcessConfig {
    /// Merge a named environment into `env`. Returns true if applied.
    pub fn apply_environment(&mut self, env_name: &str) -> bool {
        let Some(env_vars) = self.environments.get(env_name) else {
            return false;
        };
        let base = self.env.get_or_insert_with(HashMap::new);
        for (k, v) in env_vars {
            base.insert(k.clone(), v.clone());
        }
        true
    }

    /// Load env file variables, resolving relative paths against `cwd` when set.
    pub fn load_env_files(&self) -> Result<HashMap<String, String>, env_file::EnvFileError> {
        let mut env_file_vars = HashMap::new();
        let Some(env_file) = &self.env_file else {
            return Ok(env_file_vars);
        };

        for file_path in env_file.paths() {
            let path = Path::new(file_path);
            let resolved: PathBuf = if path.is_relative() {
                if let Some(ref cwd) = self.cwd {
                    PathBuf::from(cwd).join(path)
                } else {
                    path.to_path_buf()
                }
            } else {
                path.to_path_buf()
            };

            let vars = env_file::load_env_file(&resolved)?;
            env_file_vars.extend(vars);
        }

        Ok(env_file_vars)
    }
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
    watch_debounce: Option<u64>,
    depends_on: Option<Vec<String>>,
    restart: Option<RestartPolicy>,
    group: Option<String>,
    pre_start: Option<String>,
    post_stop: Option<String>,
    cron_restart: Option<String>,
    log_date_format: Option<String>,
    instances: Option<u32>,
    #[serde(flatten)]
    extra: HashMap<String, toml::Value>,
}

#[derive(Debug, PartialEq, thiserror::Error)]
pub enum ConfigError {
    #[error("config file is empty")]
    Empty,
    #[error("TOML parse error: {0}")]
    TomlParse(String),
    #[error("unknown field `{field}` in process `{process}`")]
    UnknownField { process: String, field: String },
    #[error("invalid process name `{0}`: must not contain path separators or `..`")]
    InvalidProcessName(String),
    #[error("{0}")]
    IoError(String),
}

pub fn load_config(path: &std::path::Path) -> Result<HashMap<String, ProcessConfig>, ConfigError> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| ConfigError::IoError(format!("{}: {}", path.display(), e)))?;
    parse_config(&content)
}

pub fn parse_config(content: &str) -> Result<HashMap<String, ProcessConfig>, ConfigError> {
    let table: HashMap<String, toml::Value> =
        toml::from_str(content).map_err(|e| ConfigError::TomlParse(e.to_string()))?;

    if table.is_empty() {
        return Err(ConfigError::Empty);
    }

    let mut configs = HashMap::new();

    for (name, value) in table {
        if name.contains('/') || name.contains('\\') || name.contains("..") || name.contains(':') {
            return Err(ConfigError::InvalidProcessName(name));
        }

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
                watch_debounce: raw.watch_debounce,
                depends_on: raw.depends_on,
                restart: raw.restart,
                group: raw.group,
                pre_start: raw.pre_start,
                post_stop: raw.post_stop,
                cron_restart: raw.cron_restart,
                log_date_format: raw.log_date_format,
                instances: raw.instances,
                environments,
            },
        );
    }

    Ok(configs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use tempfile::tempdir;

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
watch_debounce = 750
depends_on = ["db"]
restart = "on_failure"
group = "backend"
pre_start = "npm run migrate"
post_stop = "echo stopped"
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
        assert_eq!(web.watch_debounce, Some(750));
        assert_eq!(web.depends_on, Some(vec!["db".to_string()]));
        assert_eq!(web.restart, Some(RestartPolicy::OnFailure));
        assert_eq!(web.group.as_deref(), Some("backend"));
        assert_eq!(web.pre_start.as_deref(), Some("npm run migrate"));
        assert_eq!(web.post_stop.as_deref(), Some("echo stopped"));
        assert_eq!(web.cron_restart.as_deref(), Some("0 3 * * *"));
        assert_eq!(web.log_date_format.as_deref(), Some("%Y-%m-%d %H:%M:%S"));
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

    fn base_config() -> ProcessConfig {
        ProcessConfig {
            command: "echo hi".to_string(),
            cwd: None,
            env: None,
            env_file: None,
            health_check: None,
            kill_timeout: None,
            kill_signal: None,
            max_restarts: None,
            max_memory: None,
            min_uptime: None,
            stop_exit_codes: None,
            watch: None,
            watch_ignore: None,
            watch_debounce: None,
            depends_on: None,
            restart: None,
            group: None,
            pre_start: None,
            post_stop: None,
            cron_restart: None,
            log_date_format: None,
            instances: None,
            environments: HashMap::new(),
        }
    }

    #[test]
    fn test_apply_environment_merges() {
        let input = r#"
[web]
command = "node server.js"
env = { A = "1" }

[web.env_prod]
A = "2"
B = "3"
"#;
        let mut configs = parse_config(input).unwrap();
        let mut web = configs.remove("web").unwrap();
        assert!(web.apply_environment("prod"));
        let env = web.env.as_ref().unwrap();
        assert_eq!(env.get("A").unwrap(), "2");
        assert_eq!(env.get("B").unwrap(), "3");
        assert!(!web.apply_environment("missing"));
    }

    #[test]
    fn test_load_env_files_relative_to_cwd() {
        let dir = tempdir().unwrap();
        let env_path = dir.path().join(".env");
        std::fs::write(&env_path, "FOO=bar\n").unwrap();

        let mut config = base_config();
        config.cwd = Some(dir.path().to_string_lossy().into_owned());
        config.env_file = Some(EnvFile::Single(".env".to_string()));

        let vars = config.load_env_files().unwrap();
        assert_eq!(vars.get("FOO").unwrap(), "bar");
    }

    #[test]
    fn test_path_traversal_rejected() {
        let input = "[\"../../etc/foo\"]\ncommand = \"evil\"\n";
        let result = parse_config(input);
        assert_eq!(
            result.unwrap_err(),
            ConfigError::InvalidProcessName("../../etc/foo".to_string())
        );
    }

    #[test]
    fn test_forward_slash_in_name_rejected() {
        let input = "[\"foo/bar\"]\ncommand = \"evil\"\n";
        let result = parse_config(input);
        assert_eq!(
            result.unwrap_err(),
            ConfigError::InvalidProcessName("foo/bar".to_string())
        );
    }

    #[test]
    fn test_backslash_in_name_rejected() {
        let input = "[\"foo\\\\bar\"]\ncommand = \"evil\"\n";
        let result = parse_config(input);
        assert_eq!(
            result.unwrap_err(),
            ConfigError::InvalidProcessName("foo\\bar".to_string())
        );
    }

    #[test]
    fn test_valid_process_name_accepted() {
        let input = "[valid-name_123]\ncommand = \"echo hi\"\n";
        let configs = parse_config(input).unwrap();
        assert!(configs.contains_key("valid-name_123"));
    }

    #[test]
    fn test_colon_in_name_rejected() {
        let input = "[\"web:0\"]\ncommand = \"echo hi\"\n";
        let result = parse_config(input);
        assert_eq!(
            result.unwrap_err(),
            ConfigError::InvalidProcessName("web:0".to_string())
        );
    }

    #[test]
    fn test_instances_field_parsed() {
        let input = r#"
[web]
command = "node server.js"
instances = 4
"#;
        let configs = parse_config(input).unwrap();
        assert_eq!(configs["web"].instances, Some(4));
    }

    #[test]
    fn test_instances_default_none() {
        let input = r#"
[web]
command = "node server.js"
"#;
        let configs = parse_config(input).unwrap();
        assert!(configs["web"].instances.is_none());
    }
}
