//! `redoor remote mv`: a server-side move between device paths, on one device or across two.
//!
//! The bytes never pass through the CLI; the server renames on one device or copies and then
//! deletes the source, and the CLI follows the transfer's progress like `cp` does.

use std::{path::Path, sync::atomic::Ordering};

use anyhow::{Context, Result};
use clap::Args;
use redoor_protocol::api::transfers::{MoveFileRequest, MoveFileResponse};
use reqwest::Method;

use super::{
    client::RemoteClient,
    copy::{ExistingArgs, Location},
    credentials::CredentialStore,
    progress::TransferCounters,
};

/// Arguments of `mv`.
#[derive(Args)]
pub(crate) struct MvArgs {
    #[command(flatten)]
    existing: ExistingArgs,
    /// Print one terminal JSON result.
    #[arg(long)]
    json: bool,
    /// Source as DEVICE:/absolute/path.
    source: String,
    /// Destination as DEVICE:/absolute/path; an existing directory receives the source.
    destination: String,
}

/// Moves one path and waits for the move to finish.
pub(crate) async fn run(store: CredentialStore, args: MvArgs) -> Result<()> {
    let counters = TransferCounters::default();
    let result = execute(&store, &args, &counters).await;
    if args.json {
        println!(
            "{}",
            serde_json::json!({
                "status": if result.is_ok() { "completed" } else { "failed" },
                "source": args.source,
                "destination": args.destination,
                "resolved_destination": result.as_ref().ok(),
                "bytes_transferred": counters.bytes.load(Ordering::Relaxed),
                "error": result.as_ref().err().map(|error| format!("{error:#}")),
            })
        );
    }
    result.map(|_| ())
}

/// Resolves the destination, admits the move and follows it to the end.
async fn execute(
    store: &CredentialStore,
    args: &MvArgs,
    counters: &TransferCounters,
) -> Result<String> {
    let source = Location::remote(&args.source).await?;
    let mut destination = Location::remote(&args.destination).await?;
    let client = RemoteClient::load(store).await?;
    client.ensure_agent_running(source.agent.as_str()).await?;
    if destination.agent != source.agent {
        client
            .ensure_agent_running(destination.agent.as_str())
            .await?;
    }
    client
        .metadata(&source)
        .await?
        .context("Source does not exist")?;
    let basename = Path::new(&source.path)
        .file_name()
        .and_then(|name| name.to_str())
        .context("Source must have a UTF-8 basename (filesystem root cannot be moved)")?
        .to_owned();
    client
        .resolve_destination(&mut destination, &basename)
        .await?;
    let request = MoveFileRequest {
        source,
        dest: destination.clone(),
        on_existing: args.existing.mode(),
        resume: Default::default(),
    };
    let response = client
        .send(
            client
                .json_request(Method::POST, "api/v1/move", &request)?
                .timeout(std::time::Duration::from_secs(30)),
        )
        .await?;
    let admitted: MoveFileResponse = serde_json::from_slice(&response.bytes().await?)?;
    client
        .wait_for_transfer(admitted.move_request_id, counters)
        .await??;
    Ok(format!("{}:{}", destination.agent, destination.path))
}
