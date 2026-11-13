//! Document import and rendering (design.md §12). A supporting capability:
//! plain text and Markdown are given inline, and PDFs are read from
//! directories the user has allowed.
//!
//! Documents belong to the client that imported them; a document id alone
//! gives no access (§12.5).

mod pdf;
mod render;

use std::path::{Path, PathBuf};

use rmcp::schemars::{self, JsonSchema};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::core::{Timestamp, clock, new_id};
use crate::store::{Store, StoreError};

pub use render::render_html;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DocumentFormat {
    #[default]
    Text,
    Markdown,
    Pdf,
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
    /// True when the count comes from a layout rule rather than from the
    /// pages themselves. A PDF's count is exact.
    pub page_count_estimated: bool,
    pub created_at: Timestamp,
}

#[derive(Debug, thiserror::Error)]
pub enum DocumentError {
    #[error("the document is empty")]
    Empty,
    #[error("the document is {size} bytes; the limit is {limit}")]
    TooLarge { size: u64, limit: u64 },
    #[error("the document has {0} pages; the limit is {max}", max = MAX_PAGES)]
    TooManyPages(u32),
    #[error("{0}")]
    Invalid(String),
    #[error(
        "this server is not allowed to read that file. PDFs can be imported only from \
         directories the user allowed with `serve --document-dir`"
    )]
    PathNotAllowed,
    #[error(transparent)]
    Store(#[from] StoreError),
}

pub const MAX_TEXT_BYTES: u64 = 1024 * 1024;
pub const MAX_PDF_BYTES: u64 = 10 * 1024 * 1024;
pub const MAX_PAGES: u32 = 100;

/// The layout rule behind estimated page counts: a US Letter page with
/// one-inch margins, in 12-point monospaced text. That is 10 characters and
/// 6 lines to the inch: 65 columns by 54 lines. [`render_html`] lays text
/// out the same way.
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

fn clean_title(title: Option<String>) -> Option<String> {
    title.map(|t| t.trim().to_owned()).filter(|t| !t.is_empty())
}

fn check_pages(page_count: u32) -> Result<u32, DocumentError> {
    if page_count > MAX_PAGES {
        Err(DocumentError::TooManyPages(page_count))
    } else {
        Ok(page_count)
    }
}

/// Import inline text or Markdown for `principal`.
pub fn prepare(
    store: &Store,
    principal: &str,
    title: Option<String>,
    format: DocumentFormat,
    content: &str,
) -> Result<Document, DocumentError> {
    if format == DocumentFormat::Pdf {
        return Err(DocumentError::Invalid(
            "a PDF is imported from a file: give `path`, not `content`".into(),
        ));
    }
    if content.trim().is_empty() {
        return Err(DocumentError::Empty);
    }
    let size = content.len() as u64;
    if size > MAX_TEXT_BYTES {
        return Err(DocumentError::TooLarge {
            size,
            limit: MAX_TEXT_BYTES,
        });
    }
    let document = Document {
        id: new_id("doc"),
        principal_id: principal.to_owned(),
        title: clean_title(title),
        format,
        content: content.as_bytes().to_vec(),
        sha256: sha256_hex(content.as_bytes()),
        page_count: check_pages(estimate_pages(content))?,
        page_count_estimated: true,
        created_at: clock::now(),
    };
    store.insert_document(&document)?;
    Ok(document)
}

/// The directories PDFs may be imported from. Empty by default: this
/// server reads no file the user has not put within its reach, so an AI
/// client cannot use it to read (and then mail) arbitrary files.
#[derive(Debug, Clone, Default)]
pub struct DocumentDirs(Vec<PathBuf>);

impl DocumentDirs {
    /// Fails if a directory does not exist.
    pub fn new(dirs: &[PathBuf]) -> std::io::Result<Self> {
        dirs.iter()
            .map(std::fs::canonicalize)
            .collect::<std::io::Result<_>>()
            .map(Self)
    }

    /// The real location of `path`, if it is inside an allowed directory.
    /// Symlinks are resolved first, so a link cannot lead outside.
    fn resolve(&self, path: &Path) -> Result<PathBuf, DocumentError> {
        let real = std::fs::canonicalize(path).map_err(|_| DocumentError::PathNotAllowed)?;
        if self.0.iter().any(|dir| real.starts_with(dir)) {
            Ok(real)
        } else {
            Err(DocumentError::PathNotAllowed)
        }
    }
}

/// Import a PDF file for `principal`. The file is stored unchanged; its
/// page count is read from the file.
pub fn import_pdf(
    store: &Store,
    principal: &str,
    title: Option<String>,
    path: &Path,
    allowed: &DocumentDirs,
) -> Result<Document, DocumentError> {
    let real = allowed.resolve(path)?;
    let unreadable = |_| DocumentError::Invalid("the file could not be read".into());
    let metadata = std::fs::metadata(&real).map_err(unreadable)?;
    if !metadata.is_file() {
        return Err(DocumentError::Invalid("that is not a file".into()));
    }
    if metadata.len() > MAX_PDF_BYTES {
        return Err(DocumentError::TooLarge {
            size: metadata.len(),
            limit: MAX_PDF_BYTES,
        });
    }
    let content = std::fs::read(&real).map_err(unreadable)?;
    let page_count = check_pages(pdf::inspect(&content).map_err(DocumentError::Invalid)?)?;

    let document = Document {
        id: new_id("doc"),
        principal_id: principal.to_owned(),
        title: clean_title(title).or_else(|| {
            real.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        }),
        format: DocumentFormat::Pdf,
        sha256: sha256_hex(&content),
        content,
        page_count,
        page_count_estimated: false,
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
        let try_prepare = |format, content: &str| prepare(&store, "a", None, format, content);
        let text = DocumentFormat::Text;
        assert!(matches!(
            try_prepare(text, "  \n"),
            Err(DocumentError::Empty)
        ));
        assert!(matches!(
            try_prepare(text, &"x".repeat(MAX_TEXT_BYTES as usize + 1)),
            Err(DocumentError::TooLarge { .. })
        ));
        assert!(matches!(
            try_prepare(text, &"x\n".repeat(54 * 100 + 1)),
            Err(DocumentError::TooManyPages(101))
        ));
        assert!(matches!(
            try_prepare(DocumentFormat::Pdf, "%PDF-1.4"),
            Err(DocumentError::Invalid(_))
        ));
    }

    #[test]
    fn pdf_import_reads_the_real_page_count() {
        let store = Store::open_in_memory().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("letter.pdf");
        let bytes = pdf::tests::sample(3, pdf::tests::LETTER);
        std::fs::write(&path, &bytes).unwrap();
        let allowed = DocumentDirs::new(&[dir.path().to_owned()]).unwrap();

        let doc = import_pdf(&store, "assistant", None, &path, &allowed).unwrap();
        assert_eq!(doc.format, DocumentFormat::Pdf);
        assert_eq!(doc.page_count, 3);
        assert!(!doc.page_count_estimated);
        assert_eq!(doc.content, bytes);
        assert_eq!(doc.sha256, sha256_hex(&bytes));
        assert_eq!(doc.title.as_deref(), Some("letter.pdf"));
        assert_eq!(get(&store, "assistant", &doc.id).unwrap(), Some(doc));
    }

    #[test]
    fn pdf_import_reads_only_allowed_directories() {
        let store = Store::open_in_memory().unwrap();
        let allowed_dir = tempfile::tempdir().unwrap();
        let other_dir = tempfile::tempdir().unwrap();
        let secret = other_dir.path().join("secret.pdf");
        std::fs::write(&secret, pdf::tests::sample(1, pdf::tests::LETTER)).unwrap();
        let import =
            |path: &Path, dirs: &DocumentDirs| import_pdf(&store, "assistant", None, path, dirs);

        // Nothing is allowed by default.
        let none = DocumentDirs::default();
        assert!(matches!(
            import(&secret, &none),
            Err(DocumentError::PathNotAllowed)
        ));

        let allowed = DocumentDirs::new(&[allowed_dir.path().to_owned()]).unwrap();
        assert!(matches!(
            import(&secret, &allowed),
            Err(DocumentError::PathNotAllowed)
        ));
        // Neither `..` nor a symlink leads out of the allowed directory.
        let dotted = allowed_dir
            .path()
            .join("..")
            .join(other_dir.path().file_name().unwrap())
            .join("secret.pdf");
        assert!(matches!(
            import(&dotted, &allowed),
            Err(DocumentError::PathNotAllowed)
        ));
        #[cfg(unix)]
        {
            let link = allowed_dir.path().join("link.pdf");
            std::os::unix::fs::symlink(&secret, &link).unwrap();
            assert!(matches!(
                import(&link, &allowed),
                Err(DocumentError::PathNotAllowed)
            ));
        }
        let missing = allowed_dir.path().join("missing.pdf");
        assert!(matches!(
            import(&missing, &allowed),
            Err(DocumentError::PathNotAllowed)
        ));
        assert!(store.document("doc_any").unwrap().is_none());
    }

    #[test]
    fn pdf_import_rejects_what_is_not_a_printable_pdf() {
        let store = Store::open_in_memory().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let allowed = DocumentDirs::new(&[dir.path().to_owned()]).unwrap();
        let import = |name: &str, bytes: &[u8]| {
            let path = dir.path().join(name);
            std::fs::write(&path, bytes).unwrap();
            import_pdf(&store, "assistant", None, &path, &allowed)
        };

        let invalid = |result: Result<Document, DocumentError>| match result {
            Err(DocumentError::Invalid(message)) => message,
            other => panic!("expected a refusal, got {other:?}"),
        };
        let message = invalid(import("notes.txt", b"just some text"));
        assert!(message.contains("not a PDF"), "{message}");
        let message = invalid(import("broken.pdf", b"%PDF-1.4\nnot really"));
        assert!(message.contains("could not be read"), "{message}");
        let message = invalid(import("a4.pdf", &pdf::tests::sample(1, pdf::tests::A4)));
        assert!(message.contains("US Letter"), "{message}");
        assert!(matches!(
            import("long.pdf", &pdf::tests::sample(101, pdf::tests::LETTER)),
            Err(DocumentError::TooManyPages(101))
        ));
        assert!(matches!(
            import_pdf(&store, "assistant", None, dir.path(), &allowed),
            Err(DocumentError::Invalid(_))
        ));
    }
}
