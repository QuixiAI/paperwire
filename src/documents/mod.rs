//! Document import (design.md §12). A supporting capability: this stage
//! takes inline text or Markdown. PDF and the other formats come later.
//!
//! Documents belong to the client that imported them; a document id alone
//! gives no access (§12.5).

use rmcp::schemars::{self, JsonSchema};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::core::{Timestamp, clock};
use crate::store::{Store, StoreError};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DocumentFormat {
    #[default]
    Text,
    Markdown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document {
    pub id: String,
    /// The client that imported it.
    pub principal_id: String,
    pub title: Option<String>,
    pub format: DocumentFormat,
    pub content: Vec<u8>,
    /// Hash of the content exactly as imported (§12.4).
    pub sha256: String,
    pub page_count: u32,
    /// True when the count comes from a layout rule rather than from
    /// rendered pages.
    pub page_count_estimated: bool,
    pub created_at: Timestamp,
}

#[derive(Debug, thiserror::Error)]
pub enum DocumentError {
    #[error("the document is empty")]
    Empty,
    #[error("the document is {0} bytes; the limit is {max}", max = MAX_BYTES)]
    TooLarge(usize),
    #[error("the document is about {0} pages; the limit is {max}", max = MAX_PAGES)]
    TooManyPages(u32),
    #[error(transparent)]
    Store(#[from] StoreError),
}

pub const MAX_BYTES: usize = 1024 * 1024;
pub const MAX_PAGES: u32 = 100;

/// The layout rule behind estimated page counts: a US Letter page with
/// one-inch margins, in 12-point monospaced text. That is 10 characters and
/// 6 lines to the inch: 65 columns by 54 lines.
const COLUMNS: usize = 65;
const LINES_PER_PAGE: usize = 54;

/// Pages needed for `text` under the layout rule. Long lines wrap.
pub fn estimate_pages(text: &str) -> u32 {
    let lines: usize = text
        .lines()
        .map(|line| line.chars().count().div_ceil(COLUMNS).max(1))
        .sum();
    u32::try_from(lines.div_ceil(LINES_PER_PAGE).max(1)).unwrap_or(u32::MAX)
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Import a document for `principal`.
pub fn prepare(
    store: &Store,
    principal: &str,
    title: Option<String>,
    format: DocumentFormat,
    content: &str,
) -> Result<Document, DocumentError> {
    if content.trim().is_empty() {
        return Err(DocumentError::Empty);
    }
    if content.len() > MAX_BYTES {
        return Err(DocumentError::TooLarge(content.len()));
    }
    let page_count = estimate_pages(content);
    if page_count > MAX_PAGES {
        return Err(DocumentError::TooManyPages(page_count));
    }
    let document = Document {
        id: crate::core::new_id("doc"),
        principal_id: principal.to_owned(),
        title: title.map(|t| t.trim().to_owned()).filter(|t| !t.is_empty()),
        format,
        content: content.as_bytes().to_vec(),
        sha256: sha256_hex(content.as_bytes()),
        page_count,
        page_count_estimated: true,
        created_at: clock::now(),
    };
    store.insert_document(&document)?;
    Ok(document)
}

/// The document, if it exists and belongs to `principal`. A document of
/// another client reads as missing.
pub fn get(store: &Store, principal: &str, id: &str) -> Result<Option<Document>, StoreError> {
    Ok(store.document(id)?.filter(|d| d.principal_id == principal))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_estimate_counts_wrapped_lines() {
        assert_eq!(estimate_pages("hello"), 1);
        assert_eq!(estimate_pages(&"line\n".repeat(54)), 1);
        assert_eq!(estimate_pages(&"line\n".repeat(55)), 2);
        // One very long line wraps onto 55 lines.
        assert_eq!(estimate_pages(&"x".repeat(65 * 54 + 1)), 2);
    }

    #[test]
    fn prepare_hashes_and_scopes_to_the_client() {
        let store = Store::open_in_memory().unwrap();
        let doc = prepare(
            &store,
            "assistant",
            Some(" Hi ".into()),
            DocumentFormat::Text,
            "abc",
        )
        .unwrap();
        assert_eq!(
            doc.sha256,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(doc.title.as_deref(), Some("Hi"));
        assert!(doc.page_count_estimated);
        assert_eq!(
            get(&store, "assistant", &doc.id).unwrap(),
            Some(doc.clone())
        );
        assert_eq!(get(&store, "other", &doc.id).unwrap(), None);
    }

    #[test]
    fn prepare_enforces_limits() {
        let store = Store::open_in_memory().unwrap();
        let try_prepare = |content: &str| prepare(&store, "a", None, DocumentFormat::Text, content);
        assert!(matches!(try_prepare("  \n"), Err(DocumentError::Empty)));
        assert!(matches!(
            try_prepare(&"x".repeat(MAX_BYTES + 1)),
            Err(DocumentError::TooLarge(_))
        ));
        assert!(matches!(
            try_prepare(&"x\n".repeat(54 * 100 + 1)),
            Err(DocumentError::TooManyPages(101))
        ));
    }
}
