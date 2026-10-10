//! Destination locks prevent discovery, hashing and writes from racing another transfer.

use redoor_core::log;
use redoor_core::partials::{PartialStore, SEGMENT_SIZE, hash_segment};
use redoor_protocol::wire::log::Level;
use redoor_protocol::wire::{
    command::{CommandErrorKind, MoveSourceIdentity},
    partials::{CancelReason, PartialRecord, PartialTransfer},
    request::{CommandError, HashSegments},
};
use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
};
use tokio::{
    fs::File,
    io::{AsyncSeekExt, AsyncWriteExt},
};
use tokio_util::sync::CancellationToken;

/// Only locks live in memory; all resume information remains on disk.
#[derive(Clone, Default)]
pub(crate) struct DestinationLocks {
    active: Arc<Mutex<HashSet<String>>>,
}

impl DestinationLocks {
    /// Rejects concurrent copies rather than allowing both to append to one partial.
    pub(crate) fn acquire(&self, path: &str) -> std::io::Result<DestinationGuard> {
        if !self
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(path.into())
        {
            return Err(std::io::Error::other("destination has an active transfer"));
        }
        Ok(DestinationGuard {
            path: path.into(),
            locks: self.clone(),
        })
    }
    /// Active partials cannot be offered for resume or discard.
    pub(crate) fn contains(&self, path: &str) -> bool {
        self.active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(path)
    }
}

/// RAII keeps every early failure from leaking the destination slot.
pub(crate) struct DestinationGuard {
    path: String,
    locks: DestinationLocks,
}
impl Drop for DestinationGuard {
    /// Releasing the worker makes its on-disk partial discoverable.
    fn drop(&mut self) {
        self.locks
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.path);
    }
}

/// Holds one durable partial; dropping on crash or task abort deliberately preserves it.
pub(crate) struct PartialOutput {
    pub(crate) manifest: redoor_protocol::wire::partials::PartialManifest,
    pub(crate) file: File,
    pub(crate) store: PartialStore,
    _guard: DestinationGuard,
}
impl PartialOutput {
    /// Reuses only matching authorization; stale source metadata starts a fresh partial.
    pub(crate) async fn prepare(
        store: PartialStore,
        guard: DestinationGuard,
        transfer: PartialTransfer,
    ) -> std::io::Result<Self> {
        let key = PartialStore::key(&transfer.signed.authorization.dest_path);
        let candidate = store.probe(&key).await?;
        if let Some(identity) = &transfer.expected_partial
            && !candidate.as_ref().is_some_and(|record| {
                record.manifest.partial_device == identity.device
                    && record.manifest.partial_inode == identity.inode
            })
        {
            return Err(std::io::Error::other(
                "Partial changed during resume verification",
            ));
        }
        let (manifest, mut file) = if transfer.resume
            == redoor_protocol::api::transfers::ResumeMode::Auto
            && let Some(record) = candidate
            && record.manifest.signed.authorization.source_identity
                == transfer.signed.authorization.source_identity
            && record.manifest.signed.authorization.source_path
                == transfer.signed.authorization.source_path
            && record.manifest.signed.authorization.source_agent_id
                == transfer.signed.authorization.source_agent_id
        {
            let file = PartialStore::open(&record.manifest).await?;
            let mut manifest = record.manifest;
            manifest.signed = transfer.signed;
            store.write(&manifest).await?;
            (manifest, file)
        } else {
            store.create(transfer.signed).await?
        };
        if transfer.offset > file.metadata().await?.len() {
            return Err(std::io::Error::other(
                "Partial changed during resume verification",
            ));
        }
        file.set_len(transfer.offset).await?;
        file.seek(std::io::SeekFrom::Start(transfer.offset)).await?;
        Ok(Self {
            manifest,
            file,
            store,
            _guard: guard,
        })
    }
    /// Pause acknowledgement follows both file and manifest durability.
    pub(crate) async fn stop(mut self, reason: CancelReason) -> std::io::Result<()> {
        let key = PartialStore::key(&self.manifest.signed.authorization.dest_path);
        if reason == CancelReason::Canceled {
            self.store.discard(&key).await
        } else {
            // Tokio writes may still be buffered when cancellation interrupts the copy loop.
            self.file.flush().await?;
            self.store.persist(&key).await
        }
    }
    /// Publication removes metadata only after the final rename succeeds.
    ///
    /// The file is already published, so a manifest that cannot be removed is only logged:
    /// listing and cleanup drop manifests whose partial no longer exists.
    pub(crate) async fn published(self) {
        let dest_path = &self.manifest.signed.authorization.dest_path;
        if let Err(error) = self.store.forget(&PartialStore::key(dest_path)).await {
            log!(
                Level::Warning,
                "Cannot remove manifest of a published partial: dest_path={}, error={}",
                dest_path,
                error
            );
        }
    }
}

/// Filters active destinations from agent-local inventory without consulting the server.
pub(crate) async fn list(
    store: &PartialStore,
    locks: &DestinationLocks,
) -> Result<Vec<PartialRecord>, CommandError> {
    Ok(store
        .list()
        .await
        .map_err(|error| CommandError::io("List partials", error))?
        .into_iter()
        .filter(|record| !locks.contains(&record.manifest.signed.authorization.dest_path))
        .collect())
}

/// Hashes one segment using the same bounded sequential reader on both agents.
pub(crate) async fn hash(
    request: HashSegments,
    store: &PartialStore,
    locks: &DestinationLocks,
    cancel: &CancellationToken,
) -> Result<String, CommandError> {
    if request.end < request.start || request.end - request.start > SEGMENT_SIZE {
        return Err(CommandError::new(
            CommandErrorKind::InvalidInput,
            "Invalid hash segment",
        ));
    }
    let mut file = if let Some(key) = request.partial_key {
        let manifest = store
            .read(&key)
            .await
            .map_err(|error| CommandError::io("Read partial", error))?;
        if locks.contains(&manifest.signed.authorization.dest_path) {
            return Err(CommandError::new(
                CommandErrorKind::InvalidInput,
                "destination has an active transfer",
            ));
        }
        PartialStore::open(&manifest)
            .await
            .map_err(|error| CommandError::io("Open partial", error))?
    } else {
        File::open(&request.path)
            .await
            .map_err(|error| CommandError::io("Open source", error))?
    };
    let source_verification = request.expected_identity.is_some();
    if let Some(identity) = request.expected_identity
        && MoveSourceIdentity::from_metadata(
            &file
                .metadata()
                .await
                .map_err(|error| CommandError::io("Read identity", error))?,
        ) != identity
    {
        return Err(CommandError::new(
            CommandErrorKind::SourceChanged,
            "Source changed while verifying",
        ));
    }
    hash_segment(
        &mut file,
        request.start,
        request.end - request.start,
        cancel,
    )
    .await
    .map_err(|error| {
        if source_verification && error.kind() == std::io::ErrorKind::UnexpectedEof {
            CommandError::new(CommandErrorKind::SourceChanged, error.to_string())
        } else {
            CommandError::io("Hash segment", error)
        }
    })
}
