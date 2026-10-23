//! `redoor remote ls` and `redoor remote stat`: read-only views of one remote path.

use anyhow::Result;
use clap::Args;
use jiff::{Timestamp, tz::TimeZone};
use redoor_protocol::api::files::{LsDirectoryResponse, LsEntry, LsFileResponse, MetadataResponse};
use serde::{Deserialize, Serialize};

use super::{client::RemoteClient, copy::Location, credentials::CredentialStore};

/// Arguments of `ls` and `stat`: one remote path and the output format.
#[derive(Args)]
pub(crate) struct PathArgs {
    /// Remote path as DEVICE:/absolute/path.
    path: String,
    /// Print the API response as JSON.
    #[arg(long)]
    json: bool,
}

/// The ls endpoint answers with a directory listing or, for a file path, the file itself.
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum Listing {
    Directory(LsDirectoryResponse),
    File(LsFileResponse),
}

/// Lists a directory like `ls -l`, or describes a single file.
pub(crate) async fn ls(store: CredentialStore, args: PathArgs) -> Result<()> {
    let endpoint = Location::remote(&args.path).await?;
    let client = RemoteClient::load(&store).await?;
    client.ensure_agent_running(&endpoint.agent.0).await?;
    let listing: Listing = client
        .get_json(&client.agent_path(&endpoint, "ls")?)
        .await?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&listing)?);
        return Ok(());
    }
    match listing {
        Listing::Directory(directory) => {
            let mut entries = directory.files;
            entries.sort_by(|left, right| left.name.cmp(&right.name));
            for entry in &entries {
                println!("{}", format_entry(entry));
            }
        }
        Listing::File(file) => {
            println!(
                "{:<9} {:>12}  {}",
                "file",
                file.size,
                file.path.escape_debug()
            );
        }
    }
    Ok(())
}

/// One `ls -l` style line; names are escaped so they cannot control the terminal.
fn format_entry(entry: &LsEntry) -> String {
    let modified = Timestamp::from_second(entry.modified_at.0)
        .map(|at| {
            at.to_zoned(TimeZone::system())
                .strftime("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|_| "-".to_owned());
    format!(
        "{:<9} {:>12}  {modified}  {}",
        entry.file_type.escape_debug(),
        entry.size,
        entry.name.escape_debug()
    )
}

/// Prints a path's metadata as key/value lines.
pub(crate) async fn stat(store: CredentialStore, args: PathArgs) -> Result<()> {
    let endpoint = Location::remote(&args.path).await?;
    let client = RemoteClient::load(&store).await?;
    client.ensure_agent_running(&endpoint.agent.0).await?;
    let metadata: MetadataResponse = client
        .get_json(&client.agent_path(&endpoint, "metadata")?)
        .await?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&metadata)?);
        return Ok(());
    }
    let kind = if metadata.is_dir {
        "directory"
    } else if metadata.is_file {
        "file"
    } else {
        "other"
    };
    println!("Path:      {}", metadata.path.escape_debug());
    println!("Type:      {kind}");
    println!("Size:      {}", metadata.file_size);
    println!("MIME type: {}", metadata.mime_type.escape_debug());
    println!("Editable:  {}", metadata.editable);
    Ok(())
}
