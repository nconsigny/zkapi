//! The operator's indexer. Since vault revision 4 only requests read it: an
//! exit is bound to the note's stored leaf and needs no path. Its answers
//! are untrusted; a request path from it is used only once the vault's
//! `currentRoot()` confirms its root (`crate::request_path`).
//!
//! Every call is bounded (design §6.4 E0): request preparation reads it
//! while holding the wallet's locks, which every exit route takes too. A
//! call that connects and never answers, or drips its body, fails after
//! `CALL_TIMEOUT` instead of holding those locks.

use std::time::Duration;

use serde::Deserialize;
use zkapi_types::Felt252;

use crate::error::AuthError;

/// E0's `CALL_TIMEOUT`, connect and full body included (shortened in tests,
/// like the exit routes' own bound).
#[cfg(not(test))]
pub(crate) const CALL_TIMEOUT: Duration =
    Duration::from_secs(zkapi_client::exit_policy::CALL_TIMEOUT_SECONDS);
#[cfg(test)]
pub(crate) const CALL_TIMEOUT: Duration = Duration::from_millis(1_500);

#[derive(Debug, Clone)]
pub struct IndexerClient {
    base_url: String,
    http: reqwest::Client,
}

#[derive(Debug, Deserialize)]
struct TreeRootResponse {
    root: Felt252,
}

/// `/v1/tree/notes/{id}/path`: one view of the indexer's tree, with the
/// block it was read at (the indexer of vault revision 4 names its hash).
#[derive(Debug, Deserialize)]
struct TreePathResponse {
    siblings: Vec<Felt252>,
    #[serde(default)]
    leaf: Option<Felt252>,
    #[serde(default)]
    root: Option<Felt252>,
    #[serde(default)]
    block_number: Option<u64>,
    #[serde(default)]
    block_hash: Option<String>,
}

/// A note's path as the indexer serves it, unverified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexerPath {
    pub leaf: Option<Felt252>,
    pub siblings: Vec<Felt252>,
    pub root: Felt252,
    /// The block the indexer's view is at, when it names one.
    pub block: Option<(u64, [u8; 32])>,
}

#[derive(Debug, Deserialize)]
struct NextNoteIdResponse {
    next_note_id: u32,
}

fn block_hash(text: &str) -> Result<[u8; 32], AuthError> {
    text.strip_prefix("0x")
        .and_then(|digits| hex::decode(digits).ok())
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| AuthError::Indexer("the indexer's block hash is not 32 bytes".into()))
}

impl IndexerClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self::with_timeout(base_url, CALL_TIMEOUT)
    }

    /// An indexer client whose every call (connect and full body) is bounded
    /// by `timeout`.
    pub fn with_timeout(base_url: impl Into<String>, timeout: Duration) -> Self {
        let http = reqwest::Client::builder()
            .connect_timeout(timeout)
            .timeout(timeout)
            .build()
            .unwrap_or_default();
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            http,
        }
    }

    pub async fn root(&self) -> Result<Felt252, AuthError> {
        let payload: TreeRootResponse = self.get(format!("{}/v1/tree/root", self.base_url)).await?;
        Ok(payload.root)
    }

    /// `note_id`'s path, leaf and root from one view of the indexer's tree,
    /// within one `CALL_TIMEOUT` (E0). An indexer that omits the root in
    /// the path answer is asked for it separately.
    pub async fn path_view(&self, note_id: u32) -> Result<IndexerPath, AuthError> {
        tokio::time::timeout(CALL_TIMEOUT, async {
            let payload: TreePathResponse = self
                .get(format!("{}/v1/tree/notes/{note_id}/path", self.base_url))
                .await?;
            let root = match payload.root {
                Some(root) => root,
                None => self.root().await?,
            };
            let block = match (payload.block_number, payload.block_hash.as_deref()) {
                (Some(number), Some(hash)) => Some((number, block_hash(hash)?)),
                _ => None,
            };
            Ok::<_, AuthError>(IndexerPath {
                leaf: payload.leaf,
                siblings: payload.siblings,
                root,
                block,
            })
        })
        .await
        .map_err(|_| AuthError::Indexer("the indexer did not answer in time".into()))?
    }

    pub async fn next_note_id(&self) -> Result<u32, AuthError> {
        let payload: NextNoteIdResponse = self
            .get(format!("{}/v1/tree/next-note-id", self.base_url))
            .await?;
        Ok(payload.next_note_id)
    }

    /// The leaf the indexer's tree holds at `note_id`, from the same view as
    /// its path; zero where no note is.
    pub async fn note_leaf(&self, note_id: u32) -> Result<Felt252, AuthError> {
        let payload: TreePathResponse = self
            .get(format!("{}/v1/tree/notes/{note_id}/path", self.base_url))
            .await?;
        payload
            .leaf
            .ok_or_else(|| AuthError::Indexer("the indexer's note path carries no leaf".into()))
    }

    async fn get<T: serde::de::DeserializeOwned>(&self, url: String) -> Result<T, AuthError> {
        let response = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|err| AuthError::Indexer(err.to_string()))?;
        let response = response
            .error_for_status()
            .map_err(|err| AuthError::Indexer(err.to_string()))?;
        response
            .json()
            .await
            .map_err(|err| AuthError::Indexer(err.to_string()))
    }
}
