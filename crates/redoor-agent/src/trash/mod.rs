//! Platform trash behind one provider trait selected once at startup.
//!
//! Previously every operation repeated a triple `cfg` and Android (whose `target_os` is not
//! `linux`) silently lost freedesktop trash even though Termux provides `$HOME` and a normal
//! filesystem. Providers now implement only what their platform supports and inherit
//! "unsupported" answers for the rest.

#[cfg(any(target_os = "linux", target_os = "android"))]
mod freedesktop;
#[cfg(target_os = "macos")]
mod macos;

use crate::transfers::copy::CopyContext;
use futures_util::future::BoxFuture;
use redoor_core::log;
use redoor_protocol::wire::request::{CommandError, EmptyTrashResponse, RestoreTrashResponse};
use redoor_protocol::{
    api::trash::TrashListResponse,
    wire::{command::CommandErrorKind, log::Level},
};
use std::{path::PathBuf, sync::Arc};
use thiserror::Error;

/// Keeps provider failures mapped to stable command error categories.
#[derive(Debug, Error)]
#[error("{message}")]
pub(crate) struct TrashError {
    pub(crate) kind: CommandErrorKind,
    message: String,
}

impl TrashError {
    /// Creates an error when provider validation determines the public category directly.
    pub(crate) fn new(kind: CommandErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// Adds operation context while preserving the OS error's stable category.
    #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
    pub(crate) fn io(context: &str, error: std::io::Error) -> Self {
        Self::new(
            CommandErrorKind::from_io_error(&error),
            format!("{context}: {error}"),
        )
    }

    /// Answer for operations the selected provider does not implement.
    fn unsupported() -> Self {
        Self::new(
            CommandErrorKind::InvalidInput,
            "Trash is unsupported on this platform",
        )
    }
}

impl From<TrashError> for CommandError {
    /// Trash failures cross the protocol boundary with their stable kind.
    fn from(error: TrashError) -> Self {
        CommandError::new(error.kind.clone(), error.to_string())
    }
}

/// Builds an already-resolved future for operations a provider does not support.
fn unsupported<'a, T: Send + 'a>() -> BoxFuture<'a, Result<T, TrashError>> {
    Box::pin(std::future::ready(Err(TrashError::unsupported())))
}

/// One platform's trash implementation.
pub(crate) trait TrashProvider: Send + Sync {
    /// Whether entries can be moved into this platform's trash.
    fn supports_move(&self) -> bool {
        false
    }

    /// Whether Redoor can list, purge and restore this platform's trash.
    fn supports_inventory(&self) -> bool {
        false
    }

    /// Moves one entry to the provider-selected same-device trash location.
    fn trash(&self, _path: PathBuf) -> BoxFuture<'_, Result<(), TrashError>> {
        unsupported()
    }

    /// Discovers and orders a fresh inventory without recursively scanning payloads.
    fn list(&self) -> BoxFuture<'_, Result<TrashListResponse, TrashError>> {
        unsupported()
    }

    /// Permanently removes all payloads and metadata while preserving provider roots.
    fn empty(&self) -> BoxFuture<'_, Result<u64, TrashError>> {
        unsupported()
    }

    /// Restores one inventory item to an explicit destination with move semantics.
    fn restore<'a>(
        &'a self,
        _location_id: &'a str,
        _item_id: &'a str,
        _destination: PathBuf,
        _copy: CopyContext<'a>,
    ) -> BoxFuture<'a, Result<PathBuf, TrashError>> {
        unsupported()
    }
}

/// Provider for platforms without a supported trash; every operation is rejected.
#[cfg(any(
    test,
    not(any(target_os = "linux", target_os = "android", target_os = "macos"))
))]
struct UnsupportedTrash;

#[cfg(any(
    test,
    not(any(target_os = "linux", target_os = "android", target_os = "macos"))
))]
impl TrashProvider for UnsupportedTrash {}

/// Cloneable handle to the provider selected at startup, translating results for commands.
#[derive(Clone)]
pub(crate) struct Trash {
    provider: Arc<dyn TrashProvider>,
    /// Advertised at registration so servers do not offer an impossible action.
    pub(crate) supports_move: bool,
    /// Advertised at registration so servers only show inventories that can be read.
    pub(crate) supports_inventory: bool,
}

impl Trash {
    /// Selects and validates the platform provider once, before the agent registers.
    pub(crate) async fn select(forced_root: Option<PathBuf>) -> Result<Self, TrashError> {
        #[cfg(any(target_os = "linux", target_os = "android"))]
        let provider: Arc<dyn TrashProvider> =
            Arc::new(freedesktop::FreedesktopTrash::initialize(forced_root).await?);
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        let provider: Arc<dyn TrashProvider> = {
            if forced_root.is_some() {
                return Err(TrashError::new(
                    CommandErrorKind::InvalidInput,
                    "Trash directory overrides are supported only with freedesktop trash (Linux and Android)",
                ));
            }
            #[cfg(target_os = "macos")]
            let provider = Arc::new(macos::MacTrash);
            #[cfg(not(target_os = "macos"))]
            let provider = Arc::new(UnsupportedTrash);
            provider
        };
        Ok(Self::from_provider(provider))
    }

    /// Captures the provider's capabilities once so registration never re-queries them.
    fn from_provider(provider: Arc<dyn TrashProvider>) -> Self {
        Self {
            supports_move: provider.supports_move(),
            supports_inventory: provider.supports_inventory(),
            provider,
        }
    }

    /// Supplies a provider that rejects every operation, for tests that never touch trash.
    #[cfg(test)]
    pub(crate) fn unsupported() -> Self {
        Self::from_provider(Arc::new(UnsupportedTrash))
    }

    /// Runs the `Trash` command, logging the request and outcome for operators and tests.
    pub(crate) async fn move_to_trash(&self, path: String) -> Result<(), CommandError> {
        log!(Level::Debug, "Trash operation requested: path={}", path);
        match self.provider.trash(PathBuf::from(&path)).await {
            Ok(()) => {
                log!(Level::Debug, "Trash operation completed: path={}", path);
                Ok(())
            }
            Err(error) => {
                log!(
                    Level::Debug,
                    "Trash operation failed: path={}, kind={:?}, error={}",
                    path,
                    error.kind,
                    error
                );
                Err(error.into())
            }
        }
    }

    /// Runs the `ListTrash` command.
    pub(crate) async fn list(&self) -> Result<TrashListResponse, CommandError> {
        Ok(self.provider.list().await?)
    }

    /// Runs the `EmptyTrash` command.
    pub(crate) async fn empty(&self) -> Result<EmptyTrashResponse, CommandError> {
        let deleted_items = self.provider.empty().await?;
        Ok(EmptyTrashResponse { deleted_items })
    }

    /// Runs the `RestoreTrash` command and reports the restored path.
    pub(crate) async fn restore(
        &self,
        location_id: &str,
        item_id: &str,
        destination_path: String,
        copy: CopyContext<'_>,
    ) -> Result<RestoreTrashResponse, CommandError> {
        match self
            .provider
            .restore(location_id, item_id, PathBuf::from(destination_path), copy)
            .await
        {
            Ok(path) => match path.into_os_string().into_string() {
                Ok(path) => Ok(RestoreTrashResponse { path }),
                Err(_) => Err(CommandError::new(
                    CommandErrorKind::InvalidInput,
                    "Restored path is not valid UTF-8",
                )),
            },
            Err(error) => Err(error.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies the unsupported provider rejects every operation without advertising support.
    #[tokio::test]
    async fn unsupported_provider_rejects_every_operation() {
        let trash = Trash::unsupported();

        // Registration must not advertise capabilities the provider lacks.
        assert!(!trash.supports_move && !trash.supports_inventory);
        // Mutations fail with the stable invalid-input kind instead of panicking.
        assert!(matches!(
            trash.move_to_trash("/tmp/x".to_string()).await,
            Err(CommandError {
                kind: CommandErrorKind::InvalidInput,
                ..
            })
        ));
        // Inventory reads are equally rejected.
        assert!(matches!(trash.list().await, Err(CommandError { .. })));
        assert!(matches!(trash.empty().await, Err(CommandError { .. })));
    }
}
