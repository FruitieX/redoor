//! Agent-process facts recorded at startup for `GetAgentDetails`.

/// Process-local agent config path recorded once at agent startup.
///
/// Empty when the agent was launched from CLI/env only without loading a TOML file.
static AGENT_LOADED_CONFIG_PATH: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Records the config file this agent process loaded, when any.
///
/// Called once during agent startup so `GetAgentDetails` can surface it without
/// threading config state through every command handler call site.
pub fn set_agent_loaded_config_path(path: Option<std::path::PathBuf>) {
    let value = path
        .map(|path| path.display().to_string())
        .unwrap_or_default();
    let _ = AGENT_LOADED_CONFIG_PATH.set(value);
}

/// Absolute path of the TOML file this agent loaded, or empty when none was used.
pub fn agent_loaded_config_path() -> String {
    AGENT_LOADED_CONFIG_PATH.get().cloned().unwrap_or_default()
}
