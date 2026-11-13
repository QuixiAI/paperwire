//! Text and Markdown as print-ready HTML (design.md §12.2, §12.4).
//!
//! Nothing a document contains can become live markup: text is escaped,
//! raw HTML inside Markdown is printed as text, and images and links are
//! reduced to text, so the printed page loads nothing from anywhere.

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd, html};

use super::{Document, DocumentFormat};

/// The page layout the page estimate assumes: US Letter, one-inch margins,
/// 12-point monospaced text at six lines to the inch. Markdown keeps the
/// same grid (headings are bold, not larger), so its estimate holds too.
const STYLE: &str = "@page{size:8.5in 11in;margin:1in}\
body{margin:0;font-family:Courier,monospace;font-size:12pt;line-height:12pt}\
.doc{overflow-wrap:anywhere}\
.text{white-space:pre-wrap}\
.next{page-break-before:always}\
h1,h2,h3,h4,h5,h6{font-size:12pt;font-weight:bold;margin:12pt 0}\
p,ul,ol,pre,blockquote,table{margin:12pt 0}\
pre,code{font-family:inherit;white-space:pre-wrap}\
blockquote{margin-left:2em}";

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn markdown(source: &str) -> String {
    let options = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH;
    let mut link_targets: Vec<String> = Vec::new();
    let events = Parser::new_ext(source, options).filter_map(|event| {
        Some(match event {
            // Raw HTML is shown, not interpreted.
            Event::Html(raw) | Event::InlineHtml(raw) => Event::Text(raw),
            // An image would be fetched by the renderer: keep its alt text.
            Event::Start(Tag::Image { .. }) | Event::End(TagEnd::Image) => return None,
            // Paper has no links: print the text, then the address.
            Event::Start(Tag::Link { dest_url, .. }) => {
                link_targets.push(dest_url.into_string());
                return None;
            }
            Event::End(TagEnd::Link) => {
                let target = link_targets.pop().unwrap_or_default();
                Event::Text(format!(" ({target})").into())
            }
            other => other,
        })
    });
    let mut out = String::new();
    html::push_html(&mut out, events);
    out
}

/// The documents as one HTML letter, each starting on a new page. Fails
/// for a PDF, which is sent as it is.
pub fn render_html(documents: &[Document]) -> Result<String, String> {
    let mut body = String::new();
    for (i, document) in documents.iter().enumerate() {
        let text = std::str::from_utf8(&document.content)
            .map_err(|_| "a document is not valid UTF-8 text".to_owned())?;
        let (kind, content) = match document.format {
            DocumentFormat::Text => ("text", escape(text)),
            DocumentFormat::Markdown => ("markdown", markdown(text)),
            DocumentFormat::Pdf => return Err("a PDF cannot be combined with text".into()),
        };
        let next = if i == 0 { "" } else { " next" };
        body.push_str(&format!("<div class=\"doc {kind}{next}\">{content}</div>"));
    }
    Ok(format!(
        "<html><head><meta charset=\"utf-8\"><style>{STYLE}</style></head>\
         <body>{body}</body></html>"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::clock;

    fn document(format: DocumentFormat, text: &str) -> Document {
        Document {
            id: "doc_1".into(),
            principal_id: "assistant".into(),
            title: None,
            format,
            content: text.as_bytes().to_vec(),
            sha256: String::new(),
            page_count: 1,
            page_count_estimated: true,
            created_at: clock::now(),
        }
    }

    #[test]
    fn text_is_escaped() {
        let text = "Dear <b>Jane</b> & co,\n<script src=\"http://evil.example/x.js\"></script>";
        let html = render_html(&[document(DocumentFormat::Text, text)]).unwrap();
        assert!(html.contains("Dear &lt;b&gt;Jane&lt;/b&gt; &amp; co,"));
        assert!(!html.contains("<script"));
        assert!(!html.contains("<b>"));
    }

    #[test]
    fn markdown_is_formatted() {
        let source = "# Notice\n\nPlease **reply** by *Friday*.\n\n- one\n- two\n";
        let html = render_html(&[document(DocumentFormat::Markdown, source)]).unwrap();
        for expected in [
            "<h1>Notice</h1>",
            "<strong>reply</strong>",
            "<em>Friday</em>",
            "<li>one</li>",
        ] {
            assert!(html.contains(expected), "{expected} missing from {html}");
        }
    }

    #[test]
    fn markdown_cannot_inject_markup_or_load_anything() {
        let source = "Hi <script>alert(1)</script> there\n\n\
                      <iframe src=\"http://evil.example\"></iframe>\n\n\
                      ![tracking pixel](http://evil.example/p.png)\n\n\
                      See [our site](http://example.com/a?b=1&c=2) and \
                      [this](javascript:alert(1)).\n";
        let html = render_html(&[document(DocumentFormat::Markdown, source)]).unwrap();
        let body = html.split("<body>").nth(1).unwrap();
        // Escaped markup may remain as visible text; no live tag may.
        for forbidden in ["<script", "<iframe", "<img", "<a ", "href="] {
            assert!(!body.contains(forbidden), "{forbidden} found in {body}");
        }
        assert!(
            body.contains("&lt;script&gt;alert(1)&lt;/script&gt;"),
            "{body}"
        );
        assert!(body.contains("&lt;iframe src="), "{body}");
        assert!(body.contains("tracking pixel"), "{body}");
        assert!(
            body.contains("our site (http://example.com/a?b=1&amp;c=2)"),
            "{body}"
        );
    }

    #[test]
    fn documents_start_on_new_pages_and_pdf_is_refused() {
        let text = document(DocumentFormat::Text, "first");
        let md = document(DocumentFormat::Markdown, "second");
        let html = render_html(&[text.clone(), md]).unwrap();
        assert!(html.contains("<div class=\"doc text\">first</div>"));
        assert!(html.contains("<div class=\"doc markdown next\">"));
        let pdf = document(DocumentFormat::Pdf, "%PDF-1.4");
        assert!(render_html(&[text, pdf]).is_err());
    }
}
