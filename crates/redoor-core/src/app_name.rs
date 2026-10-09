//! Resolves the application namespace used for persistent paths and service names.

/// Environment variable that lets independent Redoor installations avoid sharing state.
pub const APP_NAME_ENV: &str = "REDOOR_APP_NAME";

/// Stable namespace used when no installation-specific name is configured.
pub const DEFAULT_APP_NAME: &str = "redoor";

/// Parses and validates a root `--app-name` value for Clap.
pub fn parse_app_name(name: &str) -> Result<String, String> {
    validate_app_name(name).map(|()| name.to_string())
}

/// Rejects names that could escape their intended path or systemd namespace.
fn validate_app_name(name: &str) -> Result<(), String> {
    let valid = !name.is_empty()
        && name != "."
        && name != ".."
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "._-".contains(character));
    if !valid {
        return Err(format!(
            "{APP_NAME_ENV} must contain only ASCII letters, numbers, '.', '_' or '-', and must not be '.' or '..'"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::validate_app_name;

    /// Ensures installation names remain safe for both paths and systemd unit names.
    #[test]
    fn validates_application_names() {
        for valid in ["redoor", "redoor-dev", "redoor_v2", "redoor.2", "2-redoor"] {
            assert!(
                validate_app_name(valid).is_ok(),
                "a safe application namespace should be accepted: {valid}"
            );
        }
        for invalid in ["", ".", "..", "../redoor", "redoor/dev", "redoor dev", "å"] {
            assert!(
                validate_app_name(invalid).is_err(),
                "an unsafe application namespace should be rejected: {invalid}"
            );
        }
    }
}
