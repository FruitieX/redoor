//! `redoor remote mkdir`.
//!
//! The agent always creates missing parents and accepts an existing directory, so plain
//! `mkdir` semantics (fail on an existing path or a missing parent) are checked here first.

use std::path::Path;

use anyhow::{Result, bail};
use clap::Args;
use redoor_protocol::api::{files::CreateDirectoryResponse, transfers::CopyEndpoint};
use reqwest::Method;

use super::{client::RemoteClient, copy::Location, credentials::CredentialStore};

/// Arguments of `mkdir`.
#[derive(Args)]
pub(crate) struct MkdirArgs {
    /// Create missing parent directories and accept an existing directory.
    #[arg(short = 'p', long)]
    parents: bool,
    /// Print the API response as JSON.
    #[arg(long)]
    json: bool,
    /// Remote directory as DEVICE:/absolute/path.
    path: String,
}

/// Creates one remote directory.
pub(crate) async fn run(store: CredentialStore, args: MkdirArgs) -> Result<()> {
    let endpoint = Location::remote(&args.path).await?;
    let client = RemoteClient::load(&store).await?;
    client.ensure_agent_running(endpoint.agent.as_str()).await?;
    if !args.parents {
        require_plain_mkdir(&client, &endpoint).await?;
    }
    let response = client
        .send(
            client
                .request(Method::POST, &client.agent_path(&endpoint, "mkdir")?)?
                .timeout(std::time::Duration::from_secs(30)),
        )
        .await?;
    let created: CreateDirectoryResponse = serde_json::from_slice(&response.bytes().await?)?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&created)?);
    }
    Ok(())
}

/// Without `-p` the path must not exist and its parent must be an existing directory.
async fn require_plain_mkdir(client: &RemoteClient, endpoint: &CopyEndpoint) -> Result<()> {
    let path = endpoint.path.escape_debug().to_string();
    if client.metadata(endpoint).await?.is_some() {
        bail!("{path} already exists");
    }
    let parent = Path::new(&endpoint.path)
        .parent()
        .map(|parent| parent.to_string_lossy().into_owned())
        .unwrap_or_else(|| "/".to_owned());
    let parent = CopyEndpoint {
        agent: endpoint.agent.clone(),
        path: parent,
    };
    if !client
        .metadata(&parent)
        .await?
        .is_some_and(|metadata| metadata.is_dir)
    {
        bail!("Parent directory of {path} does not exist; use -p to create it");
    }
    Ok(())
}
