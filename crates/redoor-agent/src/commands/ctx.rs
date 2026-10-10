//! Execution context handed to every command and the reply it returns to the dispatcher.

use crate::errors::AgentCommandError;
use crate::runtime::cancel::Registration;
use crate::runtime::responder::Responder;
use crate::transfers::{
    copy::CopyContext,
    stream::{OutputCredits, StreamWriter},
    upload::UploadRoutes,
};
use crate::trash::Trash;
use redoor_protocol::{
    ids::RequestId,
    wire::{
        command::{CommandErrorKind, CommandResult},
        stream::StreamPayloadKind,
    },
};
use std::sync::Arc;
use tokio::sync::{Semaphore, mpsc};
use tokio_tungstenite::tungstenite::protocol::Message as WsMessage;
use tokio_util::sync::CancellationToken;

/// Upper bound on concurrent content greps so independent searches cannot saturate the CPU.
const CONTENT_GREP_SLOTS: usize = 4;

/// Long-lived agent facilities shared by every command of every connection.
#[derive(Clone)]
pub(crate) struct AgentServices {
    /// Platform trash provider selected once at startup.
    pub(crate) trash: Trash,
    /// Bounds concurrent content greps without imposing latest-wins cancellation.
    pub(crate) grep_slots: Arc<Semaphore>,
    /// Routes inbound upload chunks and publication decisions to their workers.
    pub(crate) upload_routes: UploadRoutes,
    /// Routes server credit grants to the output stream writers waiting for them.
    pub(crate) output_credits: OutputCredits,
    /// TOML file this process loaded, reported by `GetAgentDetails`; empty for CLI/env-only runs.
    pub(crate) loaded_config_path: Arc<str>,
}

impl AgentServices {
    /// Bundles the startup-selected trash provider and config origin with fresh shared state.
    pub(crate) fn new(trash: Trash, loaded_config_path: Option<&std::path::Path>) -> Self {
        Self {
            trash,
            grep_slots: Arc::new(Semaphore::new(CONTENT_GREP_SLOTS)),
            upload_routes: UploadRoutes::default(),
            output_credits: OutputCredits::default(),
            loaded_config_path: loaded_config_path
                .map(|path| path.display().to_string())
                .unwrap_or_default()
                .into(),
        }
    }
}

/// Everything one command may use while it runs.
///
/// Built once by the dispatcher from the command's policy so commands never reach into runtime
/// state or process globals for their connection resources.
pub(crate) struct CommandCtx {
    /// Control-lane responder for progress, readiness and publication requests.
    pub(crate) responder: Responder,
    /// Cooperative cancellation: the request registration for request-scoped commands,
    /// otherwise a child of the control connection.
    pub(crate) cancel: CancellationToken,
    /// Request-id registration; dropping it unregisters the request from `cancel_transfer`.
    pub(crate) registration: Option<Registration<RequestId>>,
    /// Transfer socket writer for commands whose lane needs one.
    pub(crate) transfer: Option<mpsc::Sender<WsMessage>>,
    /// Shared long-lived facilities.
    pub(crate) services: AgentServices,
}

impl CommandCtx {
    /// Borrows the cancellation and progress plumbing used by copy, move and restore.
    pub(crate) fn copy_context(&self) -> CopyContext<'_> {
        CopyContext {
            responder: &self.responder,
            cancel: &self.cancel,
        }
    }

    /// Splits the context into what an output stream needs.
    ///
    /// Admission guarantees both the transfer writer and the registration for output commands;
    /// a missing one is reported instead of panicking so a policy mistake cannot crash the agent.
    pub(crate) fn into_output(
        self,
        payload_kind: StreamPayloadKind,
    ) -> Result<OutputLane, AgentCommandError> {
        let request_id = self.responder.request_id();
        match (self.transfer, self.registration) {
            (Some(transfer), Some(registration)) => {
                let stream = StreamWriter::new(
                    transfer,
                    request_id,
                    payload_kind,
                    self.services.output_credits.register(request_id),
                );
                Ok(OutputLane {
                    stream,
                    registration,
                    responder: self.responder,
                })
            }
            _ => Err(AgentCommandError::new(
                CommandErrorKind::Internal,
                "Output command was dispatched without a transfer connection",
            )),
        }
    }

    /// Builds a context without a connection for unit tests; frames land in the receiver.
    #[cfg(test)]
    pub(crate) fn detached(
        request_id: RequestId,
    ) -> (
        Self,
        mpsc::Receiver<crate::runtime::responder::OutboundFrame>,
    ) {
        let (responder, frames) = Responder::recording(request_id, 16);
        (
            Self {
                responder,
                cancel: CancellationToken::new(),
                registration: None,
                transfer: None,
                services: AgentServices::new(Trash::unsupported(), None),
            },
            frames,
        )
    }
}

/// Resources of a command that streams output over the transfer socket.
pub(crate) struct OutputLane {
    pub(crate) stream: StreamWriter,
    pub(crate) registration: Registration<RequestId>,
    pub(crate) responder: Responder,
}

/// How a finished command reports its outcome.
pub(crate) enum Reply {
    /// Send this terminal result on the control lane.
    Result(CommandResult),
    /// The outcome already travelled in-band as the final or error frame of the output stream.
    Streamed,
    /// In-band delivery failed: send this result on the control lane, but give up if the
    /// connection generation that would carry it is torn down first.
    Fallback {
        result: CommandResult,
        abandon: CancellationToken,
    },
}

impl From<CommandResult> for Reply {
    /// Most commands simply answer with their result.
    fn from(result: CommandResult) -> Self {
        Self::Result(result)
    }
}
