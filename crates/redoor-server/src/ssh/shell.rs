//! Renders values as POSIX `sh` words and wraps scripts so any login shell can start them.
//!
//! sshd hands the remote command line to the user's login shell, which may be fish, tcsh
//! or another shell that does not understand POSIX syntax such as `${VAR:-default}`. Every
//! remote command is therefore sent as one `sh -c '<script>' sh '<arg>'...` line made of
//! single-quoted words, the one quoting form all of those shells read the same way, and
//! the script itself runs in POSIX `sh`. The script must be a single line because
//! csh-family shells reject newlines inside quotes.

/// Remote path prefixes that are expanded on the device, with the `sh` word each becomes.
///
/// Paths are otherwise passed literally, so a configured path cannot run commands on the
/// device. These prefixes keep the default install location and the home-relative paths
/// operators already use working, which previously relied on the login shell expanding
/// the whole unquoted path.
const EXPANDED_PREFIXES: &[(&str, &str)] = &[
    (
        "${XDG_DATA_HOME:-$HOME/.local/share}",
        "\"${XDG_DATA_HOME:-$HOME/.local/share}\"",
    ),
    ("${HOME}", "\"$HOME\""),
    ("$HOME", "\"$HOME\""),
    ("~", "\"$HOME\""),
];

/// Quotes a value as one POSIX shell word. Single quotes keep every character literal, and
/// an embedded quote is closed, escaped and reopened, which bash, zsh, dash, fish and tcsh
/// all read the same way.
pub(super) fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r#"'\''"#))
}

/// Renders a remote path as a `sh` word: a known home or data directory prefix is expanded
/// on the device and the rest of the path stays literal.
pub(super) fn path_word(path: &str) -> String {
    for (prefix, word) in EXPANDED_PREFIXES {
        let Some(rest) = path.strip_prefix(prefix) else {
            continue;
        };
        // `$HOMEDIR` or `~user` are not the prefix, only an exact match or a `/` after it.
        if rest.is_empty() {
            return (*word).to_string();
        }
        if rest.starts_with('/') {
            return format!("{word}{}", quote(rest));
        }
    }
    quote(path)
}

/// Builds the remote command line that runs `script` in POSIX `sh` with `args` as its
/// positional parameters (`"$@"`), whatever the user's login shell is.
pub(super) fn sh_command(script: &str, args: &[&str]) -> String {
    debug_assert!(
        !script.contains('\n'),
        "remote sh -c scripts must be a single line for csh-family login shells"
    );
    let mut line = format!("sh -c {} sh", quote(script));
    for arg in args {
        line.push(' ');
        line.push_str(&quote(arg));
    }
    line
}

#[cfg(test)]
mod tests {
    use super::{path_word, quote, sh_command};

    /// Runs `command` through a local shell the way sshd runs it through the login shell.
    async fn run_as_login_shell(shell: &str, command: &str, home: &str) -> String {
        let output = tokio::process::Command::new(shell)
            .arg("-c")
            .arg(command)
            .env("HOME", home)
            .env_remove("XDG_DATA_HOME")
            .output()
            .await
            .unwrap();
        // The command must run, otherwise the printed output would be meaningless.
        assert!(
            output.status.success(),
            "{shell} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    /// Values with quotes, spaces and shell syntax reach the script unchanged.
    #[tokio::test]
    async fn sh_command_passes_arguments_literally() {
        let tricky = "it's a $(touch pwned); `x` \"y\" *";
        let command = sh_command(r#"printf '%s|' "$@""#, &["plain", tricky, ""]);
        // The login shell sees one line, which csh-family shells require.
        assert!(!command.contains('\n'));
        let output = run_as_login_shell("/bin/sh", &command, "/home/user").await;
        // Every argument arrives as exactly one positional parameter, including the empty one.
        assert_eq!(output, format!("plain|{tricky}||"));
    }

    /// Known prefixes expand on the device, everything else is literal.
    #[tokio::test]
    async fn path_word_expands_only_known_prefixes() {
        let cases = [
            (
                "${XDG_DATA_HOME:-$HOME/.local/share}/redoor/bin",
                "/home/u/.local/share/redoor/bin",
            ),
            ("$HOME/my bin/redoor", "/home/u/my bin/redoor"),
            ("${HOME}/redoor", "/home/u/redoor"),
            ("~/redoor", "/home/u/redoor"),
            ("~", "/home/u"),
            ("/opt/$(touch pwned)/redoor", "/opt/$(touch pwned)/redoor"),
            ("$HOMEDIR/redoor", "$HOMEDIR/redoor"),
            ("~other/redoor", "~other/redoor"),
        ];
        for (path, expected) in cases {
            let script = format!("printf '%s' {}", path_word(path));
            let output = run_as_login_shell("/bin/sh", &sh_command(&script, &[]), "/home/u").await;
            // Each path must resolve exactly as documented for its prefix.
            assert_eq!(output, expected, "path {path}");
        }
    }

    /// An embedded single quote is escaped instead of ending the word.
    #[test]
    fn quote_escapes_single_quotes() {
        assert_eq!(quote("a'b"), r#"'a'\''b'"#);
    }
}
