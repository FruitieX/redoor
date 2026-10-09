//! The error type every command failure is converted through before it crosses the wire.

use crate::transfers::{copy::LocalCopyError, r#move::LocalMoveError};
use redoor_protocol::wire::command::{CommandErrorKind, CommandResult};
use thiserror::Error;

/// Wraps subsystem-specific command failures behind one protocol boundary type.
#[derive(Debug, Error)]
pub(crate) enum AgentCommandError {
    #[error(transparent)]
    LocalCopy(#[from] LocalCopyError),
    #[error(transparent)]
    LocalMove(#[from] LocalMoveError),
    /// Failure that already knows its stable protocol kind.
    #[error("{message}")]
    Failed {
        kind: CommandErrorKind,
        message: String,
    },
}

impl AgentCommandError {
    /// Builds a failure whose protocol kind is known at the failure site.
    pub(crate) fn new(kind: CommandErrorKind, message: impl Into<String>) -> Self {
        Self::Failed {
            kind,
            message: message.into(),
        }
    }

    /// Returns the protocol-stable kind that the server maps into an HTTP status.
    pub(crate) fn kind(&self) -> CommandErrorKind {
        match self {
            Self::LocalCopy(error) => error.kind(),
            Self::LocalMove(error) => error.kind(),
            Self::Failed { kind, .. } => kind.clone(),
        }
    }
}

impl From<AgentCommandError> for CommandResult {
    /// Converts a failure into the wire error result with its stable kind.
    fn from(error: AgentCommandError) -> Self {
        CommandResult::error(error.kind(), error.to_string())
    }
}
