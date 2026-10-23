//! Authenticated HTTP transport without response buffering or transfer-wide timeouts.

use anyhow::{Context, Result, ensure};
use redoor_protocol::api::transfers::CopyEndpoint;
use reqwest::{Method, RequestBuilder, Response, StatusCode, Url, header};
use serde::{Serialize, de::DeserializeOwned};

use super::credentials::{CredentialStore, Credentials, LOGIN_REQUIRED};

/// Retains HTTP status so callers can tell missing paths from permission failures.
#[derive(Debug, thiserror::Error)]
#[error("Remote API returned {status}: {detail}")]
pub struct RemoteApiError {
    pub status: StatusCode,
    pub detail: String,
}

/// Cloneable transport for independent streaming and control operations.
#[derive(Clone)]
pub struct RemoteClient {
    http: reqwest::Client,
    server: Url,
    token: String,
}

impl RemoteClient {
    /// Validates the selected server before credentials can be put on the wire.
    pub fn parse_server(server: &str) -> Result<Url> {
        let mut url =
            Url::parse(server).context("SERVER_URL must be an absolute HTTP or HTTPS URL")?;
        ensure!(
            matches!(url.scheme(), "http" | "https") && url.host_str().is_some(),
            "SERVER_URL must use HTTP or HTTPS"
        );
        ensure!(
            url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none(),
            "SERVER_URL must not contain credentials, a query, or a fragment"
        );
        if !url.path().ends_with('/') {
            url.set_path(&format!("{}/", url.path()));
        }
        Ok(url)
    }

    /// Disables redirects so credentials never follow an API redirect to a different origin.
    pub(crate) fn transport() -> Result<reqwest::Client> {
        Ok(reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(std::time::Duration::from_secs(15))
            .build()?)
    }

    /// Uses `REDOOR_REMOTE_SERVER`/`REDOOR_REMOTE_TOKEN` when set, otherwise the saved login.
    pub async fn load(store: &CredentialStore) -> Result<Self> {
        let credentials = match Credentials::from_environment(
            std::env::var("REDOOR_REMOTE_SERVER").ok(),
            std::env::var("REDOOR_REMOTE_TOKEN").ok(),
        )? {
            Some(credentials) => credentials,
            None => store.load().await?.context(LOGIN_REQUIRED)?,
        };
        Self::with_token(&credentials.server_url, credentials.token)
    }

    /// Builds a client for an explicit server and token, e.g. to revoke a replaced login.
    pub(crate) fn with_token(server: &str, token: String) -> Result<Self> {
        Ok(Self {
            http: Self::transport()?,
            server: Self::parse_server(server)?,
            token,
        })
    }

    /// The validated server base URL, which prefixes every endpoint.
    pub fn server(&self) -> &Url {
        &self.server
    }

    /// Resolves API paths relative to a deployment prefix while forbidding origin/path escapes.
    pub fn endpoint(&self, path: &str) -> Result<Url> {
        let url = self.server.join(path.trim_start_matches('/'))?;
        ensure!(
            url.origin() == self.server.origin() && url.path().starts_with(self.server.path()),
            "Remote request must target the selected server"
        );
        Ok(url)
    }

    /// Builds `api/v1/agents/{agent}/{operation}/{path}`, percent-encoding each component so
    /// spaces, colons, `#` and `?` remain filesystem data.
    pub fn agent_path(&self, endpoint: &CopyEndpoint, operation: &str) -> Result<String> {
        let mut url = Url::parse("http://localhost/")?;
        {
            let mut segments = url
                .path_segments_mut()
                .map_err(|_| anyhow::anyhow!("Invalid API URL"))?;
            let agent = endpoint.agent.to_string();
            segments.extend(["api", "v1", "agents", &agent]);
            segments.extend(operation.split('/'));
            for part in endpoint.path.trim_start_matches('/').split('/') {
                segments.push(part);
            }
        }
        Ok(url.path().trim_start_matches('/').to_owned())
    }

    /// Lets transfers attach streaming bodies/query options without bypassing authenticated send.
    pub fn request(&self, method: Method, path: &str) -> Result<RequestBuilder> {
        Ok(self.http.request(method, self.endpoint(path)?))
    }

    /// Adds the bearer token and checks the status before exposing the streaming body.
    pub async fn send(&self, builder: RequestBuilder) -> Result<Response> {
        let mut request = builder.build()?;
        ensure!(
            request.url().origin() == self.server.origin()
                && request.url().path().starts_with(self.server.path()),
            "Remote request must target the selected server"
        );
        request.headers_mut().insert(
            header::AUTHORIZATION,
            format!("Bearer {}", self.token)
                .parse()
                .context("Access token contains invalid characters")?,
        );
        let response = self
            .http
            .execute(request)
            .await
            .context("Remote request failed")?;
        if response.status() == StatusCode::UNAUTHORIZED {
            anyhow::bail!(LOGIN_REQUIRED);
        }
        Self::check_response(response).await
    }

    /// Decodes typed control responses while send remains usable for bounded-memory transfers.
    pub async fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let response = self
            .send(
                self.request(Method::GET, path)?
                    .timeout(std::time::Duration::from_secs(30)),
            )
            .await?;
        serde_json::from_slice(&response.bytes().await?).context("Invalid remote API response")
    }

    /// Uses existing API payload types without requiring reqwest's optional JSON feature.
    pub fn json_request<T: Serialize>(
        &self,
        method: Method,
        path: &str,
        body: &T,
    ) -> Result<RequestBuilder> {
        Ok(self
            .request(method, path)?
            .header(header::CONTENT_TYPE, "application/json")
            .body(serde_json::to_vec(body)?))
    }

    /// Turns failures into `RemoteApiError` with the server's message (for example which
    /// permission a token lacks), reading at most 8 KiB of error body.
    pub(crate) async fn check_response(mut response: Response) -> Result<Response> {
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        let mut bytes = Vec::new();
        while bytes.len() < 8192 {
            let Some(chunk) = response.chunk().await? else {
                break;
            };
            bytes.extend_from_slice(&chunk[..chunk.len().min(8192 - bytes.len())]);
        }
        let detail = serde_json::from_slice::<redoor_protocol::api::ErrorResponse>(&bytes)
            .map(|body| body.error)
            .unwrap_or_else(|_| String::from_utf8_lossy(&bytes).into_owned());
        Err(RemoteApiError { status, detail }.into())
    }
}
