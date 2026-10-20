//! `redoor remote rm`: moves to the trash by default, like the UI, because a permanent delete
//! cannot be undone.

use anyhow::{Context, Result, bail};
use clap::Args;
use redoor_protocol::api::files::RawDeleteResponse;
use reqwest::Method;

use super::{client::RemoteClient, copy::Location, credentials::CredentialStore};

/// Arguments of `rm`.
#[derive(Args)]
pub(crate) struct RmArgs {
    /// Remove directories and their contents.
    #[arg(short = 'r', long)]
    recursive: bool,
    /// Delete permanently instead of moving to the trash.
    #[arg(long)]
    permanent: bool,
    /// Print the API response as JSON.
    #[arg(long)]
    json: bool,
    /// Remote path as DEVICE:/absolute/path.
    path: String,
}

/// Removes one remote path after the same checks POSIX `rm` makes.
pub(crate) async fn run(store: CredentialStore, args: RmArgs) -> Result<()> {
    let endpoint = Location::remote(&args.path).await?;
    let label = endpoint.path.escape_debug().to_string();
    let client = RemoteClient::load(&store).await?;
    client.ensure_agent_running(&endpoint.agent.0).await?;
    let metadata = client
        .metadata(&endpoint)
        .await?
        .with_context(|| format!("{label} does not exist"))?;
    if metadata.is_dir && !args.recursive {
        bail!("{label} is a directory; use -r to remove it");
    }
    let mut path = client.agent_path(&endpoint, "raw")?;
    if !args.permanent {
        let device = client.agent_snapshot(&endpoint.agent.0).await?;
        if !device.supports_move_to_trash {
            bail!(
                "Device {} has no trash; use --permanent to delete {label} permanently",
                endpoint.agent.to_string().escape_debug()
            );
        }
        path.push_str("?trash=true");
    }
    let response = client
        .send(
            client
                .request(Method::DELETE, &path)?
                .timeout(std::time::Duration::from_secs(300)),
        )
        .await?;
    let deleted: RawDeleteResponse = serde_json::from_slice(&response.bytes().await?)?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&deleted)?);
    }
    Ok(())
}
