//! Selects, verifies, and streams compatible agent binaries to remote hosts.

use std::path::{Path, PathBuf};

use redoor_core::log;
use redoor_protocol::wire::log::Level;
use tokio::io::AsyncReadExt;
use tokio::time::{Duration, timeout};

use super::ProvisionError;
use super::shell::{path_word, quote};
use super::transport::{SshHost, SshRunOptions};
use crate::watchdog::ProvisioningStatusSink;

/// Bounds cancellation-triggered SSH cleanup so a second unreachable host cannot leak a task.
const REMOTE_TEMP_CLEANUP_TIMEOUT: Duration = Duration::from_secs(30);

/// First line of a probe report, so output from anything else (a login banner printed
/// by a shell startup file, for example) is not mistaken for probe fields.
const PROBE_MARKER: &str = "redoor-probe";

/// Lines of a probe report: the marker, os, arch, sha1, path and version.
pub(super) const PROBE_REPORT_LINES: usize = 6;

/// Last line of a launch report when the binary matched and is being started.
pub(super) const LAUNCH_READY: &str = "ready";

/// Last line of a launch report when the binary did not match and nothing was started.
pub(super) const LAUNCH_NEEDS_INSTALL: &str = "install";

/// Owns a remote upload sibling until it is atomically renamed into place.
struct RemoteTempGuard {
    host: SshHost,
    path: Option<String>,
}

impl RemoteTempGuard {
    /// Starts guarding immediately after the path can first be created remotely.
    fn new(host: &SshHost, path: String) -> Self {
        Self {
            host: host.clone(),
            path: Some(path),
        }
    }

    /// Removes the sibling on ordinary errors while retaining drop cleanup if this await is canceled.
    async fn cleanup(&mut self) {
        let Some(path) = self.path.as_ref() else {
            return;
        };
        if remove_remote_temp(&self.host, path).await {
            self.path = None;
        }
    }

    /// Transfers ownership to the final binary after a successful remote rename.
    fn disarm(&mut self) {
        self.path = None;
    }
}

impl Drop for RemoteTempGuard {
    /// Schedules bounded cleanup because provisioning futures are intentionally cancellable.
    fn drop(&mut self) {
        let Some(path) = self.path.take() else {
            return;
        };
        let host = self.host.clone();
        tokio::spawn(async move {
            remove_remote_temp(&host, &path).await;
        });
    }
}

/// Best-effort removal is bounded and its SSH child is kill-on-drop through [`SshHost::run`].
async fn remove_remote_temp(host: &SshHost, path: &str) -> bool {
    let options = SshRunOptions::default().compressed();
    let command = format!("rm -f {}", path_word(path));
    match timeout(
        REMOTE_TEMP_CLEANUP_TIMEOUT,
        host.run(&command, &[], &options),
    )
    .await
    {
        Ok(Ok(status)) if status.success() => true,
        Ok(Ok(status)) => {
            redoor_core::log_failure!(
                Level::Error,
                "Remote provisioning temp cleanup failed: target={}, path={}, status={}",
                host.target(),
                path,
                status.code().unwrap_or(-1)
            );
            false
        }
        Ok(Err(error)) => {
            redoor_core::log_failure!(
                Level::Error,
                "Remote provisioning temp cleanup failed: target={}, path={}, error={:#}",
                host.target(),
                path,
                error
            );
            false
        }
        Err(_) => {
            redoor_core::log_failure!(
                Level::Error,
                "Remote provisioning temp cleanup timed out: target={}, path={}",
                host.target(),
                path
            );
            false
        }
    }
}

/// Default remote redoor binary path when the user does not override it
/// via `--remote-bin` or `REDOOR_REMOTE_BIN`.
pub(super) fn default_remote_bin(app_name: &str) -> String {
    format!(
        "${{XDG_DATA_HOME:-$HOME/.local/share}}/{app_name}/binaries/{}/redoor",
        env!("CARGO_PKG_VERSION")
    )
}

/// Dedicated remote path for local debug binary uploads so iterative debug
/// deploys never overwrite a versioned release install on the same host.
fn debug_remote_bin(app_name: &str) -> String {
    format!("${{XDG_DATA_HOME:-$HOME/.local/share}}/{app_name}/binaries/debug/redoor")
}

/// Result of probing a remote host: which OS/arch it runs, what its existing
/// redoor binary reports for `--version`, and the SHA-1 of that binary file
/// (empty version/sha1 when there is no readable binary at the path).
#[derive(Debug)]
pub(super) struct RemoteSniff {
    pub(super) os: String,
    pub(super) arch: String,
    /// First line of `<remote_bin> --version` with surrounding whitespace stripped.
    /// Empty when the binary is missing, not executable, or otherwise fails to run,
    /// in which case it needs to be (re)installed.
    pub(super) version_output: String,
    /// Lowercase hex SHA-1 of the remote binary file contents. Empty when the
    /// file is missing or unreadable. Used to skip re-upload when the remote
    /// bytes already match the local binary (critical for debug builds where
    /// `--version` stays constant across rebuilds).
    pub(super) sha1sum: String,
    /// Shell-expanded remote path so status lines can show the real file.
    pub(super) remote_bin: String,
}

/// Builds the probe statements shared by the standalone probe and the launch script.
///
/// The script stays a single line so it can also be passed through `sh -c`. Each field is
/// printed on its own line, and only the first line of `--version` is kept, so a version
/// string or path containing commas, or a broken binary printing garbage, cannot shift
/// the other fields. Prefer `sha1sum` (Linux) and fall back to `shasum` (macOS); hosts
/// with neither report an empty SHA-1 and are matched by version. `awk` is not needed:
/// `${sha1%% *}` keeps the digest before the file name.
fn probe_statements(remote_bin: &str) -> String {
    format!(
        "bin={bin}; os=$(uname); arch=$(uname -m); \
         version=$(\"$bin\" --version 2>/dev/null | head -n 1); sha1=; \
         if [ -f \"$bin\" ]; then \
         if command -v sha1sum >/dev/null 2>&1; then sha1=$(sha1sum \"$bin\" 2>/dev/null); \
         else sha1=$(shasum -a 1 \"$bin\" 2>/dev/null); fi; fi; \
         sha1=${{sha1%% *}}; \
         printf '%s\\n' {PROBE_MARKER} \"$os\" \"$arch\" \"$sha1\" \"$bin\" \"$version\"",
        bin = path_word(remote_bin)
    )
}

/// Builds the shell script used to inspect a remote binary in one SSH
/// round-trip. The outer brace group must be parsed in full before execution,
/// so a truncated SSH stdin stream cannot run a partially received probe.
fn remote_sniff_script(remote_bin: &str) -> String {
    format!("{{ {}; }}\n", probe_statements(remote_bin))
}

impl RemoteSniff {
    /// Parses the probe report lines printed by [`probe_statements`].
    pub(super) fn parse(target: &str, report: &[String]) -> Result<Self, ProvisionError> {
        let [marker, os, arch, sha1sum, remote_bin, version_output] = report else {
            return Err(Self::unexpected(target, report));
        };
        if marker != PROBE_MARKER {
            return Err(Self::unexpected(target, report));
        }
        // Map `uname` values to the os component used in the release artifact
        // filenames (e.g. `redoor-aarch64-linux.tar.gz`).
        let os = match os.trim() {
            "Linux" => "linux",
            "Darwin" => "macos",
            other => {
                return Err(ProvisionError::Remote(format!(
                    "unsupported remote os '{other}' on {target}"
                )));
            }
        };
        // macOS reports `arm64` for Apple Silicon but the release artifacts use
        // `aarch64`, so normalize before looking up the download URL.
        let arch = match arch.trim() {
            "x86_64" => "x86_64",
            "aarch64" | "arm64" => "aarch64",
            other => {
                return Err(ProvisionError::Remote(format!(
                    "unsupported remote arch '{other}' on {target}"
                )));
            }
        };
        Ok(Self {
            os: os.to_string(),
            arch: arch.to_string(),
            version_output: version_output.trim().to_string(),
            sha1sum: sha1sum.trim().to_string(),
            remote_bin: remote_bin.to_string(),
        })
    }

    /// Logs and describes a report that does not have the probe's shape.
    fn unexpected(target: &str, report: &[String]) -> ProvisionError {
        redoor_core::log_failure!(
            Level::Error,
            "Remote probe report has an unexpected shape: target={}, report={:?}",
            target,
            report
        );
        ProvisionError::Remote(format!(
            "unexpected probe output from {target}: {report:?}; expected '{PROBE_MARKER}' followed by os, arch, sha1, path and version lines"
        ))
    }

    /// Formats probe fields for the sticky starting UI without sending raw script output.
    pub(super) fn status_message(&self) -> String {
        format!(
            "Sniff results: arch={}, platform={}, path={}, version={}, sha1sum={}",
            self.arch,
            self.os,
            self.remote_bin,
            if self.version_output.is_empty() {
                "(none)"
            } else {
                self.version_output.as_str()
            },
            if self.sha1sum.is_empty() {
                "(none)"
            } else {
                self.sha1sum.as_str()
            }
        )
    }
}

/// Probes the remote host with a single ssh command that reports its OS,
/// CPU architecture, the `--version` output of the configured redoor binary,
/// and that binary's SHA-1. Batching into one round-trip avoids paying ssh
/// setup latency multiple times for what is conceptually one "is the host
/// ready" check.
pub(super) async fn sniff_remote(
    host: &SshHost,
    remote_bin: &str,
) -> Result<RemoteSniff, ProvisionError> {
    let sniff_start = std::time::Instant::now();
    let script = remote_sniff_script(remote_bin);
    log!(
        Level::Debug,
        "Remote sniff started: target={}, ssh_server_port={}, remote_bin={}, script='{}'",
        host.target(),
        host.server_port_label(),
        remote_bin,
        script.trim_end()
    );
    let options = SshRunOptions::default().compressed();
    let output = host
        .run_script_captured("remote host probe", &script, &options)
        .await
        .map_err(|error| {
            redoor_core::log_failure!(
                Level::Error,
                "Remote sniff script execution failed: target={}, remote_bin={}, elapsed={:?}, error={:#}",
                host.target(),
                remote_bin,
                sniff_start.elapsed(),
                error
            );
            error
        })?;
    let report: Vec<String> = output.lines().map(str::to_string).collect();
    let sniff = RemoteSniff::parse(host.target(), &report)?;
    log!(
        Level::Info,
        "Remote sniff complete: target={}, remote_bin={}, os={}, arch={}, version_output='{}', sha1sum='{}', elapsed={:?}",
        host.target(),
        sniff.remote_bin,
        sniff.os,
        sniff.arch,
        sniff.version_output,
        sniff.sha1sum,
        sniff_start.elapsed()
    );
    Ok(sniff)
}

/// What a remote binary must report before it is used, in the probe script and in Rust.
#[derive(Clone, Debug)]
pub(super) struct BinaryExpectation {
    /// Exact first line of `--version`.
    version: String,
    /// SHA-1 of the local binary the remote one must equal, when it is known. A remote
    /// host without a checksum utility is matched by version alone.
    sha1: Option<String>,
}

impl BinaryExpectation {
    /// Any binary reporting this server's version, as for a versioned release install.
    fn this_version() -> Self {
        Self {
            version: redoor_core::identity::version_line(),
            sha1: None,
        }
    }

    /// This server's version and exactly the bytes of `local_sha1`, used to verify an
    /// uploaded release artifact, which is always built for this server's version.
    fn exact(local_sha1: String) -> Self {
        Self {
            version: redoor_core::identity::version_line(),
            sha1: Some(local_sha1),
        }
    }

    /// The version and exact bytes of the local debug build at `path`, matched by SHA-1
    /// because the version does not change between rebuilds. The version is read from
    /// the build itself rather than taken from this server: the build on disk can be
    /// newer than the running server, for example after a version bump without a
    /// restart, and expecting the server's version would reject every upload of it and
    /// re-upload the same bytes forever.
    async fn local_build(path: &Path) -> Result<Self, std::io::Error> {
        let output = tokio::process::Command::new(path)
            .arg("--version")
            .output()
            .await
            .map_err(|error| {
                std::io::Error::other(format!(
                    "failed to run '{} --version': {error}",
                    path.display()
                ))
            })?;
        if !output.status.success() {
            return Err(std::io::Error::other(format!(
                "'{} --version' exited with {}",
                path.display(),
                output.status
            )));
        }
        // The remote probe compares only the first line, so the expectation must too.
        let version = String::from_utf8_lossy(&output.stdout)
            .lines()
            .next()
            .unwrap_or_default()
            .trim()
            .to_string();
        Ok(Self {
            version,
            sha1: Some(file_sha1sum(path).await?),
        })
    }

    /// Decides whether a probed binary can be used; mirrors the check in [`Self::launch_script`].
    pub(super) fn matches(&self, sniff: &RemoteSniff) -> bool {
        sniff.version_output == self.version
            && match &self.sha1 {
                Some(sha1) => sniff.sha1sum.is_empty() || &sniff.sha1sum == sha1,
                None => true,
            }
    }

    /// Builds the single-line launch script: it prints the probe report, then either
    /// prints [`LAUNCH_READY`] and `exec`s the binary with the script's positional
    /// arguments, or prints [`LAUNCH_NEEDS_INSTALL`] and exits without starting anything.
    /// Probing and starting in one session saves an SSH round-trip on every start.
    pub(super) fn launch_script(&self, remote_bin: &str) -> String {
        let sha1 = quote(self.sha1.as_deref().unwrap_or(""));
        format!(
            "{{ {probe}; \
             if [ \"$version\" = {version} ] && {{ [ -z {sha1} ] || [ -z \"$sha1\" ] || [ \"$sha1\" = {sha1} ]; }}; then \
             echo {LAUNCH_READY}; exec \"$bin\" \"$@\"; fi; \
             echo {LAUNCH_NEEDS_INSTALL}; }}",
            probe = probe_statements(remote_bin),
            version = quote(&self.version),
        )
    }

    /// Explains why a just-uploaded binary was rejected.
    fn mismatch_message(&self, sniff: &RemoteSniff) -> String {
        format!(
            "remote binary at {} did not verify after upload: version '{}' (want '{}'), sha1sum '{}' (want '{}')",
            sniff.remote_bin,
            sniff.version_output,
            self.version,
            sniff.sha1sum,
            self.sha1.as_deref().unwrap_or("any")
        )
    }
}

/// Result of checking the binary at one path against an expectation.
pub(super) struct BinaryCheckResult<T> {
    /// What the probe found at the path.
    pub(super) sniff: RemoteSniff,
    /// The started process or usable path when the binary matched, `None` when it must be
    /// installed first.
    pub(super) ready: Option<T>,
}

/// One way of checking a device's binary: probing only (relays, which start the agent
/// later and repeatedly) or probing while starting the agent (managed agents).
pub(super) trait BinaryCheck {
    /// What a successful check produces.
    type Ready;

    /// Checks the binary at `remote_bin` against `expect`.
    async fn check(
        &mut self,
        remote_bin: &str,
        expect: &BinaryExpectation,
    ) -> Result<BinaryCheckResult<Self::Ready>, ProvisionError>;
}

/// Checks by probing only; the ready value is the shell-expanded remote path.
pub(super) struct ProbeCheck<'a> {
    /// Host to probe.
    pub(super) host: &'a SshHost,
}

impl BinaryCheck for ProbeCheck<'_> {
    type Ready = String;

    async fn check(
        &mut self,
        remote_bin: &str,
        expect: &BinaryExpectation,
    ) -> Result<BinaryCheckResult<String>, ProvisionError> {
        let sniff = sniff_remote(self.host, remote_bin).await?;
        let ready = expect.matches(&sniff).then(|| sniff.remote_bin.clone());
        Ok(BinaryCheckResult { sniff, ready })
    }
}

/// Where a missing or stale binary is installed from and to.
struct InstallTarget {
    /// Remote path the binary is uploaded to.
    remote_bin: String,
    /// Local debug build to upload, or `None` for the release artifact of the platform.
    debug_binary: Option<PathBuf>,
}

/// Makes sure a device runs a binary matching this server, installing one when needed.
///
/// Only the default install locations are managed here; an operator-provided path is
/// used as-is by the caller. A debug server prefers its local debug build at a dedicated
/// path, matched by SHA-1 because the version does not change between rebuilds, and
/// falls back to the versioned release install when the device's platform differs.
pub(super) struct Installer<'a> {
    /// Host to install on.
    pub(super) host: &'a SshHost,
    /// Local namespace and release cache.
    pub(super) provision: &'a super::ProvisionContext,
    /// Progress lines for the lifecycle UI.
    pub(super) status: &'a ProvisioningStatusSink,
}

impl Installer<'_> {
    /// Checks the expected path with `check` and installs the binary when it does not
    /// match, then checks again to verify the upload. Returns what the successful check
    /// produced.
    pub(super) async fn ensure<C: BinaryCheck>(
        &self,
        check: &mut C,
    ) -> Result<C::Ready, ProvisionError> {
        self.status.report("Sniffing the SSH target");
        let (mut remote_bin, mut expect) = self.first_candidate().await?;
        let mut uploaded = false;
        let mut first_check = true;
        // At most: the debug path, the release path after a platform mismatch, and the
        // verification after the upload.
        for _ in 0..3 {
            log!(
                Level::Debug,
                "Checking remote binary: target={}, remote_bin={}, expect={:?}, uploaded={}",
                self.host.target(),
                remote_bin,
                expect,
                uploaded
            );
            let result = check.check(&remote_bin, &expect).await?;
            let sniff = result.sniff;
            if first_check {
                // Later checks follow an upload or a platform switch and have their own lines.
                self.status.report(sniff.status_message());
                first_check = false;
            }
            if let Some(ready) = result.ready {
                if uploaded {
                    self.status.report(format!(
                        "Verified the uploaded binary at {}",
                        sniff.remote_bin
                    ));
                } else {
                    self.status.report(format!(
                        "Using existing remote binary at {}",
                        sniff.remote_bin
                    ));
                }
                log!(
                    Level::Info,
                    "Remote binary matches: target={}, remote_bin={}, version='{}', sha1sum='{}', uploaded={}",
                    self.host.target(),
                    sniff.remote_bin,
                    sniff.version_output,
                    sniff.sha1sum,
                    uploaded
                );
                return Ok(ready);
            }
            if uploaded {
                let message = expect.mismatch_message(&sniff);
                redoor_core::log_failure!(
                    Level::Error,
                    "Uploaded binary did not verify: target={}, {}",
                    self.host.target(),
                    message
                );
                return Err(ProvisionError::Remote(message));
            }
            let target = self.install_target(&sniff).await?;
            if target.remote_bin != remote_bin {
                // The probe looked at the debug path, but this platform needs the release
                // install, which may already be in place.
                log!(
                    Level::Info,
                    "Switching to the release install for a different remote platform: target={}, os={}, arch={}, remote_bin={}",
                    self.host.target(),
                    sniff.os,
                    sniff.arch,
                    target.remote_bin
                );
                self.status.report(format!(
                    "Checking the release install at {} for this platform",
                    target.remote_bin
                ));
                remote_bin = target.remote_bin;
                expect = BinaryExpectation::this_version();
                continue;
            }
            let (local_path, upload_expect) = match target.debug_binary {
                Some(path) => {
                    let upload_expect = BinaryExpectation::local_build(&path).await?;
                    (path, upload_expect)
                }
                None => {
                    let path = self
                        .provision
                        .binaries
                        .ensure(env!("CARGO_PKG_VERSION"), &sniff.os, &sniff.arch, |line| {
                            self.status.report(line)
                        })
                        .await?;
                    let upload_expect = BinaryExpectation::exact(file_sha1sum(&path).await?);
                    (path, upload_expect)
                }
            };
            log!(
                Level::Info,
                "Remote binary missing or stale, uploading: target={}, remote_bin={}, remote_version='{}', remote_sha1sum='{}', local_path={}",
                self.host.target(),
                sniff.remote_bin,
                sniff.version_output,
                sniff.sha1sum,
                local_path.display()
            );
            upload_binary(self.host, &local_path, &remote_bin, self.status).await?;
            uploaded = true;
            expect = upload_expect;
        }
        let message = format!(
            "could not settle on a remote binary for {} after several attempts",
            self.host.target()
        );
        redoor_core::log_failure!(Level::Error, "{}", message);
        Err(ProvisionError::Remote(message))
    }

    /// The path and expectation checked first, before the remote platform is known: the
    /// local debug build when this is a debug server that has one, otherwise the versioned
    /// release install.
    async fn first_candidate(&self) -> Result<(String, BinaryExpectation), ProvisionError> {
        let debug_binary =
            available_debug_binary(std::env::consts::OS, std::env::consts::ARCH).await?;
        match debug_binary {
            Some(path) => {
                self.status
                    .report("Comparing the remote debug install to this local build");
                Ok((
                    debug_remote_bin(&self.provision.app_name),
                    BinaryExpectation::local_build(&path).await?,
                ))
            }
            None => Ok((
                default_remote_bin(&self.provision.app_name),
                BinaryExpectation::this_version(),
            )),
        }
    }

    /// Where the binary for the probed platform is installed from and to.
    async fn install_target(&self, sniff: &RemoteSniff) -> Result<InstallTarget, ProvisionError> {
        let debug_binary = available_debug_binary(&sniff.os, &sniff.arch).await?;
        let remote_bin = if debug_binary.is_some() {
            debug_remote_bin(&self.provision.app_name)
        } else {
            default_remote_bin(&self.provision.app_name)
        };
        Ok(InstallTarget {
            remote_bin,
            debug_binary,
        })
    }
}

/// Streams `path` through SHA-1 so large binaries never need to sit fully in
/// memory, matching the same digest the remote `sha1sum`/`shasum` probe uses.
async fn file_sha1sum(path: &Path) -> Result<String, std::io::Error> {
    use sha1::{Digest, Sha1};

    let mut file = tokio::fs::File::open(path).await.map_err(|error| {
        std::io::Error::other(format!(
            "failed to open '{}' to calculate its SHA-1: {error}",
            path.display()
        ))
    })?;
    let mut hasher = Sha1::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Unconditionally streams an operator-selected binary to the remote install
/// path and verifies that the resulting executable contains the same bytes.
/// This intentionally bypasses version/cache selection so local development or
/// cross-compiled artifacts can replace an otherwise matching remote release.
pub(super) async fn force_upload_binary(
    host: &SshHost,
    local_path: &Path,
    remote_bin: &str,
    status: &ProvisioningStatusSink,
) -> Result<String, ProvisionError> {
    let metadata = tokio::fs::metadata(local_path).await.map_err(|error| {
        std::io::Error::other(format!(
            "failed to access binary source '{}': {error}",
            local_path.display()
        ))
    })?;
    if !metadata.is_file() {
        return Err(ProvisionError::Remote(format!(
            "binary source '{}' is not a regular file",
            local_path.display()
        )));
    }

    let local_sha1 = file_sha1sum(local_path).await?;
    upload_binary(host, local_path, remote_bin, status).await?;
    let uploaded = sniff_remote(host, remote_bin).await?;
    if uploaded.version_output.is_empty() {
        redoor_core::log_failure!(
            Level::Error,
            "Forced binary upload does not run: target={}, remote_bin={}, sha1sum='{}'",
            host.target(),
            uploaded.remote_bin,
            uploaded.sha1sum
        );
        return Err(ProvisionError::Remote(format!(
            "uploaded binary at {} did not execute successfully with --version",
            uploaded.remote_bin
        )));
    }
    if !uploaded.sha1sum.is_empty() && uploaded.sha1sum != local_sha1 {
        redoor_core::log_failure!(
            Level::Error,
            "Forced binary upload checksum mismatch: target={}, remote_bin={}, remote_sha1sum='{}', local_sha1sum='{}'",
            host.target(),
            uploaded.remote_bin,
            uploaded.sha1sum,
            local_sha1
        );
        return Err(ProvisionError::Remote(format!(
            "remote binary at {} sha1sum mismatch after forced upload: got '{}', want '{}'",
            uploaded.remote_bin, uploaded.sha1sum, local_sha1
        )));
    }

    log!(
        Level::Info,
        "Forced binary upload verified: binary_source={}, remote_bin={}, version='{}', sha1sum='{}'",
        local_path.display(),
        uploaded.remote_bin,
        uploaded.version_output,
        uploaded.sha1sum
    );
    Ok(uploaded.remote_bin)
}

/// Returns the workspace debug binary path when the running server was built
/// in debug mode and its platform matches the remote host. Keeping platform
/// checks here prevents accidentally uploading a locally runnable binary that
/// the remote kernel or CPU cannot execute.
fn debug_binary_candidate(
    debug_build: bool,
    local_os: &str,
    local_arch: &str,
    remote_os: &str,
    remote_arch: &str,
    manifest_dir: &Path,
) -> Option<PathBuf> {
    if !debug_build || local_os != remote_os || local_arch != remote_arch {
        return None;
    }

    Some(manifest_dir.join("target/debug/redoor"))
}

/// Root of the cargo workspace this server was built from. `CARGO_MANIFEST_DIR` names
/// `crates/redoor-server`, but `cargo build` writes `target/debug/redoor` at the workspace
/// root, so the debug binary lookup must climb out of the crate directory (the same
/// `../..` that `redoor-core/build.rs` uses to find `.git`).
fn workspace_root() -> PathBuf {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .and_then(Path::parent)
        .unwrap_or(manifest_dir)
        .to_path_buf()
}

/// Resolves the matching local debug binary only when it exists. Missing debug
/// output is not an error because GitHub release provisioning remains the
/// intended fallback for fresh checkouts and cross-compiled server binaries.
async fn available_debug_binary(
    remote_os: &str,
    remote_arch: &str,
) -> Result<Option<PathBuf>, std::io::Error> {
    let candidate = debug_binary_candidate(
        cfg!(debug_assertions),
        std::env::consts::OS,
        std::env::consts::ARCH,
        remote_os,
        remote_arch,
        &workspace_root(),
    );

    match candidate {
        Some(path) if tokio::fs::try_exists(&path).await? => Ok(Some(path)),
        _ => Ok(None),
    }
}

/// Sibling temp path for atomic remote binary replace. Keeping the temp file
/// in the same directory as `remote_bin` ensures `mv` stays on one filesystem
/// and can rename over a still-running executable without ETXTBSY.
fn remote_upload_tmp_path(remote_bin: &str, unique: &str) -> String {
    format!("{remote_bin}.tmp.{unique}")
}

/// Builds the single-line script that installs the binary streamed on stdin.
///
/// In-place `cat > remote_bin` fails with ETXTBSY ("text file busy") when a previous
/// agent is still executing that binary, so the script writes a sibling temp file and
/// renames it over the final path; the old inode stays mapped until that process exits.
/// It creates the parent directory for a fresh host and marks the file executable
/// because `cat` does not carry mode bits. `set -e` stops at the first failing step and
/// the EXIT trap removes the temp file on that failure.
fn upload_script(remote_bin: &str, remote_tmp: &str) -> String {
    format!(
        "{{ set -e; bin={bin}; tmp={tmp}; mkdir -p \"$(dirname \"$bin\")\"; \
         trap 'rm -f \"$tmp\"' EXIT; cat > \"$tmp\"; chmod +x \"$tmp\"; mv -f \"$tmp\" \"$bin\"; \
         trap - EXIT; }}",
        bin = path_word(remote_bin),
        tmp = path_word(remote_tmp)
    )
}

/// Uploads the binary in one SSH session with [`upload_script`].
async fn upload_binary(
    host: &SshHost,
    local_path: &Path,
    remote_bin: &str,
    status: &ProvisioningStatusSink,
) -> Result<(), ProvisionError> {
    let unique = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let remote_tmp = remote_upload_tmp_path(remote_bin, &unique);
    // The script removes the temp file itself on failure; the guard covers a cancelled
    // provisioning future or a dropped connection, where the script may not finish.
    let mut remote_tmp_guard = RemoteTempGuard::new(host, remote_tmp.clone());

    status.report(format!("Uploading the binary to {remote_bin}"));
    log!(
        Level::Info,
        "Uploading binary to remote host: target={}, local_path={}, remote_bin={}, remote_tmp={}",
        host.target(),
        local_path.display(),
        remote_bin,
        remote_tmp
    );

    if let Err(error) = host
        .upload(
            local_path,
            &upload_script(remote_bin, &remote_tmp),
            "binary upload",
        )
        .await
    {
        remote_tmp_guard.cleanup().await;
        return Err(ProvisionError::Remote(format!(
            "failed to upload the binary to {remote_bin}: {error}"
        )));
    }
    remote_tmp_guard.disarm();

    log!(
        Level::Info,
        "Binary upload complete: target={}, remote_bin={}",
        host.target(),
        remote_bin
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        BinaryExpectation, LAUNCH_NEEDS_INSTALL, LAUNCH_READY, PROBE_REPORT_LINES, RemoteSniff,
        debug_binary_candidate, debug_remote_bin, default_remote_bin, file_sha1sum,
        remote_sniff_script, remote_upload_tmp_path, upload_script,
    };
    use crate::ssh::shell::sh_command;
    use std::path::{Path, PathBuf};
    use tokio::io::AsyncWriteExt;

    /// Runs a remote command line through a local `/bin/sh` the way sshd runs it through
    /// the login shell, with `stdin` as its input and a fake home directory.
    async fn run_remote(command: &str, stdin: &[u8], home: &Path) -> std::process::Output {
        let mut child = tokio::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(command)
            .env_remove("XDG_DATA_HOME")
            .env("HOME", home)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut input = child.stdin.take().unwrap();
        input.write_all(stdin).await.unwrap();
        drop(input);
        child.wait_with_output().await.unwrap()
    }

    /// Writes an executable fake redoor that prints `version` and its arguments.
    async fn fake_binary(path: &Path, version: &str) {
        tokio::fs::create_dir_all(path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(
            path,
            format!(
                r#"#!/bin/sh
if [ "$1" = --version ]; then echo '{version}'; echo 'second line, with comma'; exit 0; fi
printf 'agent:%s|' "$@"
"#
            ),
        )
        .await
        .unwrap();
        let mut permissions = tokio::fs::metadata(path).await.unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
        tokio::fs::set_permissions(path, permissions).await.unwrap();
    }

    /// Splits stdout into lines the way the report reader does.
    fn lines(output: &std::process::Output) -> Vec<String> {
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// Verifies local SHA-1 matches the well-known digest for a fixed payload so
    /// remote `sha1sum` comparisons cannot drift from a buggy hasher.
    #[tokio::test]
    async fn file_sha1sum_matches_known_digest() {
        let directory = redoor_core::test_support::TempDir::create();
        let path = directory.path().join("payload.bin");
        tokio::fs::write(&path, b"redoor").await.unwrap();

        // echo -n redoor | sha1sum
        let digest = file_sha1sum(&path).await.unwrap();
        assert_eq!(digest, "5cd57297d6ccaa26976cb250ba018adbc98d5907");
    }

    /// Streams the generated probe to a POSIX shell just as SSH does, proving
    /// the script remains executable without login-shell features.
    #[tokio::test]
    async fn remote_sniff_script_executes_successfully() {
        let home = redoor_core::test_support::TempDir::create();
        let script =
            remote_sniff_script("${XDG_DATA_HOME:-$HOME/.local/share}/redoor-test-missing-binary");
        // The outer group prevents any probe command from running before the full script arrives.
        assert!(
            script.starts_with("{ ") && script.ends_with("}\n"),
            "the complete probe should be protected by one brace group"
        );

        let output = run_remote("exec sh -s", script.as_bytes(), home.path()).await;
        // Success proves the streamed script uses syntax supported by the local POSIX shell.
        assert!(
            output.status.success(),
            "probe failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report = lines(&output);
        // The parser needs every field even when the binary is missing.
        assert_eq!(report.len(), PROBE_REPORT_LINES, "report: {report:?}");
        let sniff = RemoteSniff::parse("test", &report).unwrap();
        // Returning the shell-expanded path lets logs identify the actual remote file.
        assert_eq!(
            sniff.remote_bin,
            format!(
                "{}/.local/share/redoor-test-missing-binary",
                home.path().display()
            )
        );
        // A missing binary has neither a version nor a checksum, so it gets installed.
        assert_eq!(sniff.version_output, "");
        assert_eq!(sniff.sha1sum, "");
    }

    /// A path with spaces and shell syntax is probed literally, and only the first version
    /// line is kept so commas or extra output cannot shift the other fields.
    #[tokio::test]
    async fn probe_reports_odd_paths_and_versions_literally() {
        let home = redoor_core::test_support::TempDir::create();
        let remote_bin = "~/my bin/$(touch pwned)/redoor";
        let path = home.path().join("my bin/$(touch pwned)/redoor");
        fake_binary(&path, "redoor 1.2.3").await;

        let output = run_remote(
            "exec sh -s",
            remote_sniff_script(remote_bin).as_bytes(),
            home.path(),
        )
        .await;
        let sniff = RemoteSniff::parse("test", &lines(&output)).unwrap();
        // The path is used literally after the expanded home prefix.
        assert_eq!(sniff.remote_bin, path.display().to_string());
        // Only the first `--version` line is reported.
        assert_eq!(sniff.version_output, "redoor 1.2.3");
        // The checksum covers the file the probe ran.
        assert_eq!(sniff.sha1sum, file_sha1sum(&path).await.unwrap());
        // The path's command substitution never ran.
        assert!(!home.path().join("pwned").exists());
    }

    /// The launch script starts a matching binary with its arguments after the report, and
    /// starts nothing when the binary does not match.
    #[tokio::test]
    async fn launch_script_starts_only_a_matching_binary() {
        let home = redoor_core::test_support::TempDir::create();
        let path = home.path().join("bin/redoor");
        let expected = redoor_core::identity::version_line();
        fake_binary(&path, &expected).await;
        let local_sha1 = file_sha1sum(&path).await.unwrap();
        let cases = [
            (BinaryExpectation::this_version(), true),
            (BinaryExpectation::exact(local_sha1.clone()), true),
            (BinaryExpectation::exact("0".repeat(40)), false),
        ];
        for (expect, starts) in cases {
            let command = sh_command(&expect.launch_script("~/bin/redoor"), &["agent", "a b'c"]);
            let output = run_remote(&command, b"", home.path()).await;
            let report = lines(&output);
            // The report comes first in both outcomes, so the server can show the sniff results.
            let sniff = RemoteSniff::parse("test", &report[..PROBE_REPORT_LINES]).unwrap();
            // The Rust-side check must agree with the script.
            assert_eq!(expect.matches(&sniff), starts, "expect {expect:?}");
            if starts {
                // The decision line is followed by the binary's own output with every argument intact.
                assert_eq!(report[PROBE_REPORT_LINES], LAUNCH_READY);
                assert_eq!(report[PROBE_REPORT_LINES + 1], "agent:agent|agent:a b'c|");
            } else {
                // Nothing runs after the decision line when the binary has to be installed.
                assert_eq!(
                    report[PROBE_REPORT_LINES..],
                    [LAUNCH_NEEDS_INSTALL.to_string()]
                );
            }
        }
    }

    /// The upload script installs the streamed bytes as an executable in one session, and
    /// leaves no temp file behind when a step fails.
    #[tokio::test]
    async fn upload_script_installs_streamed_binary() {
        let home = redoor_core::test_support::TempDir::create();
        let remote_bin = "~/data dir/redoor";
        let remote_tmp = remote_upload_tmp_path(remote_bin, "1-2");
        let command = sh_command(&upload_script(remote_bin, &remote_tmp), &[]);

        let binary = b"#!/bin/sh
echo uploaded
";
        let output = run_remote(&command, binary, home.path()).await;
        // The directory is created and the binary moved into place.
        assert!(
            output.status.success(),
            "upload failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let installed = home.path().join("data dir/redoor");
        let ran = tokio::process::Command::new(&installed)
            .output()
            .await
            .unwrap();
        // The installed file has the streamed bytes and is executable.
        assert_eq!(String::from_utf8_lossy(&ran.stdout).trim_end(), "uploaded");
        // The temp file was renamed, not copied.
        assert!(!home.path().join("data dir/redoor.tmp.1-2").exists());

        // A read-only directory makes `cat` fail after the trap is set up.
        let blocked = home.path().join("blocked");
        tokio::fs::create_dir(&blocked).await.unwrap();
        let mut permissions = tokio::fs::metadata(&blocked).await.unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o555);
        tokio::fs::set_permissions(&blocked, permissions)
            .await
            .unwrap();
        let blocked_bin = "~/blocked/redoor";
        let command = sh_command(
            &upload_script(blocked_bin, &remote_upload_tmp_path(blocked_bin, "3-4")),
            &[],
        );
        let output = run_remote(&command, b"data", home.path()).await;
        // The failure is reported through the exit status.
        assert!(!output.status.success());
        // Nothing is left in the directory.
        let mut entries = tokio::fs::read_dir(&blocked).await.unwrap();
        assert!(entries.next_entry().await.unwrap().is_none());
    }

    /// Verifies checksum-less hosts can reuse a binary only when its executable
    /// still reports the exact expected version.
    #[test]
    fn expectation_falls_back_to_version_without_sha1() {
        let version = redoor_core::identity::version_line();
        let sniff = |version: &str, sha1sum: &str| RemoteSniff {
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
            version_output: version.to_string(),
            sha1sum: sha1sum.to_string(),
            remote_bin: "/bin/redoor".to_string(),
        };
        let exact = BinaryExpectation::exact("local-digest".to_string());
        // A matching version is the only available identity signal on a minimal host.
        assert!(exact.matches(&sniff(&version, "")));
        // A stale version must still trigger an upload when no digest can be calculated.
        assert!(!exact.matches(&sniff("redoor 0.0.0", "")));
        // A present digest must match even if the version text matches.
        assert!(!exact.matches(&sniff(&version, "different-digest")));
        assert!(exact.matches(&sniff(&version, "local-digest")));
        // A release install is matched by version alone.
        assert!(BinaryExpectation::this_version().matches(&sniff(&version, "any-digest")));
    }

    /// A local debug build is expected to report its own version, not this server's, so a
    /// build that is newer than the running server still verifies after its upload.
    #[tokio::test]
    async fn local_build_expects_the_build_version() {
        let home = redoor_core::test_support::TempDir::create();
        let path = home.path().join("target/debug/redoor");
        fake_binary(&path, "redoor 999.0.0").await;
        let expect = BinaryExpectation::local_build(&path).await.unwrap();
        // The version comes from the build's first `--version` line.
        assert_eq!(expect.version, "redoor 999.0.0");
        // The bytes must still match exactly, since debug rebuilds keep the version.
        assert_eq!(expect.sha1, Some(file_sha1sum(&path).await.unwrap()));
        let sniff = RemoteSniff {
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
            version_output: "redoor 999.0.0".to_string(),
            sha1sum: file_sha1sum(&path).await.unwrap(),
            remote_bin: "/bin/redoor".to_string(),
        };
        // An identical upload verifies even though this server reports another version.
        assert!(expect.matches(&sniff));
    }

    /// Ensures the upload temp path stays beside the final binary so remote
    /// `mv` is atomic and does not cross filesystems.
    #[test]
    fn remote_upload_tmp_path_stays_in_same_directory() {
        assert_eq!(
            remote_upload_tmp_path(
                "${XDG_DATA_HOME:-$HOME/.local/share}/redoor/binaries/debug/redoor",
                "1-2"
            ),
            "${XDG_DATA_HOME:-$HOME/.local/share}/redoor/binaries/debug/redoor.tmp.1-2"
        );
    }

    /// Verifies remote releases mirror the local versioned binary hierarchy
    /// while using the persistent XDG data location on the target host.
    #[test]
    fn default_remote_bin_uses_versioned_data_hierarchy() {
        assert_eq!(
            default_remote_bin("redoor-dev"),
            format!(
                "${{XDG_DATA_HOME:-$HOME/.local/share}}/redoor-dev/binaries/{}/redoor",
                env!("CARGO_PKG_VERSION")
            ),
            "the remote install should live under the application data hierarchy"
        );
    }

    /// Verifies debug uploads target a dedicated install path rather than the
    /// versioned release layout, so both can coexist on one remote host.
    #[test]
    fn debug_remote_bin_uses_dedicated_debug_version() {
        assert_eq!(
            debug_remote_bin("redoor-dev"),
            "${XDG_DATA_HOME:-$HOME/.local/share}/redoor-dev/binaries/debug/redoor",
            "the debug install path should use the effective application namespace"
        );
    }

    /// Verifies a debug server can provision its exact local build to a host
    /// that can execute the same operating-system and CPU artifact.
    #[test]
    fn selects_debug_binary_for_matching_platform() {
        let path = debug_binary_candidate(
            true,
            "linux",
            "x86_64",
            "linux",
            "x86_64",
            Path::new("/workspace/redoor"),
        );

        // A matching debug build should bypass GitHub and use workspace output.
        assert_eq!(
            path,
            Some(PathBuf::from("/workspace/redoor/target/debug/redoor"))
        );
    }

    /// Verifies the debug binary lookup points at the workspace build output that
    /// `cargo build` produces, not at a per-crate target directory that never exists.
    #[test]
    fn debug_binary_resolves_from_workspace_root() {
        let root = super::workspace_root();
        let manifest = std::fs::read_to_string(root.join("Cargo.toml"))
            .expect("workspace root should contain Cargo.toml");
        // Only the virtual workspace manifest declares `[workspace]`.
        assert!(
            manifest.contains("[workspace]"),
            "{} is not the workspace root",
            root.display()
        );
        let path = debug_binary_candidate(
            true,
            std::env::consts::OS,
            std::env::consts::ARCH,
            std::env::consts::OS,
            std::env::consts::ARCH,
            &root,
        );
        // The candidate must be `<workspace>/target/debug/redoor`, where the binary is built.
        assert_eq!(path, Some(root.join("target/debug/redoor")));
    }

    /// Verifies release servers keep using release provisioning even when a
    /// stale debug binary happens to remain in the workspace target directory.
    #[test]
    fn ignores_debug_binary_for_release_build() {
        let path = debug_binary_candidate(
            false,
            "linux",
            "x86_64",
            "linux",
            "x86_64",
            Path::new("/workspace/redoor"),
        );

        // Build mode must gate local debug deployment independently of the path.
        assert_eq!(path, None);
    }

    /// Verifies a debug binary is never uploaded to a remote CPU that cannot
    /// execute it, leaving release download selection to choose the right arch.
    #[test]
    fn ignores_debug_binary_for_different_architecture() {
        let path = debug_binary_candidate(
            true,
            "linux",
            "x86_64",
            "linux",
            "aarch64",
            Path::new("/workspace/redoor"),
        );

        // Architecture mismatches must fall back rather than fail after upload.
        assert_eq!(path, None);
    }

    /// Verifies matching CPU names alone are insufficient when the remote host
    /// uses another executable format and system ABI.
    #[test]
    fn ignores_debug_binary_for_different_operating_system() {
        let path = debug_binary_candidate(
            true,
            "linux",
            "aarch64",
            "macos",
            "aarch64",
            Path::new("/workspace/redoor"),
        );

        // OS mismatches must use the platform-specific GitHub artifact instead.
        assert_eq!(path, None);
    }
}
