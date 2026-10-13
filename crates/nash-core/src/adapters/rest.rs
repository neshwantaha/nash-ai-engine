//! A generic, non-LLM REST client mode.
//!
//! Some custom endpoints are ordinary JSON APIs, not model inference servers.
//! This client lets the engine call them safely (same SSRF policy, same
//! redaction) *without* pretending they are chat models.

use super::common::read_body_limited;
use crate::config::{apply_auth, AuthConfig};
use crate::error::Result;
use crate::http::{HttpClient, PreparedRequest};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::time::Instant;
use tokio_util::sync::CancellationToken;
use url::Url;

/// A REST call description.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RestRequest {
    pub method: String,
    /// Absolute URL or a path joined onto a base URL supplied separately.
    pub url: String,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub query: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<Value>,
}

/// The response of a REST call.
#[derive(Debug, Clone)]
pub struct RestResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub text: String,
    pub json: Option<Value>,
    pub elapsed_ms: u64,
}

/// Stateless generic REST client.
#[derive(Debug, Clone)]
pub struct RestClient {
    http: HttpClient,
}

impl RestClient {
    pub fn new(http: HttpClient) -> Self {
        Self { http }
    }

    /// Execute a REST request. `auth`/`secret` are optional.
    pub async fn request(
        &self,
        req: RestRequest,
        auth: Option<&AuthConfig>,
        secret: Option<&str>,
        cancel: &CancellationToken,
    ) -> Result<RestResponse> {
        let mut url = Url::parse(&req.url)
            .map_err(|e| crate::error::EngineError::Config(format!("invalid REST url: {e}")))?;
        for (k, v) in &req.query {
            url.query_pairs_mut().append_pair(k, v);
        }
        let method = reqwest::Method::from_bytes(req.method.to_uppercase().as_bytes())
            .map_err(|_| crate::error::EngineError::Config(format!("invalid method '{}'", req.method)))?;
        let mut prepared = PreparedRequest::new(method, url, "rest.request");
        for (k, v) in &req.headers {
            prepared = prepared.header(k, v);
        }
        if let (Some(auth), Some(secret)) = (auth, secret) {
            prepared = apply_auth(prepared, auth, Some(secret))?;
        }
        if let Some(body) = &req.body {
            prepared = prepared.json_body(body)?;
        }

        let started = Instant::now();
        let resp = self.http.execute(prepared, cancel).await?;
        let headers: Vec<(String, String)> = resp
            .headers()
            .iter()
            .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("").to_string()))
            .collect();
        let (status2, _, text) = read_body_limited(resp, 4 * 1024 * 1024).await;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        let json = serde_json::from_str::<Value>(&text).ok();
        Ok(RestResponse {
            status: status2,
            headers,
            text,
            json,
            elapsed_ms,
        })
    }
}
