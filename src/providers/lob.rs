//! Lob (https://docs.lob.com): letters by US mail.
//!
//! Built from Lob's published API and tested against recorded-shape
//! responses, not yet against Lob itself. Choices that follow from Lob:
//!
//! - **No price.** Lob has no quote endpoint, so [`quote`] reports the price
//!   as unavailable and the user's own price table bounds the cost (§13.4).
//! - **Idempotency.** Creation sends `Idempotency-Key`, which Lob honors for
//!   24 hours, so the retry wrapper may resend a create (§19.1).
//! - **Reconciliation.** Every letter carries the operation id as metadata,
//!   and `lookup` lists letters by it. An empty list is reported as
//!   *unknown*, not *not found*: how soon a new letter appears in Lob's
//!   list has not been verified.
//! - **Layout.** The address goes on an inserted first page, so the
//!   document's own pages print exactly as counted. That page is billed.
//!
//! [`quote`]: ProviderAdapter::quote

use serde_json::{Value, json};

use super::{
    Account, AuthMethod, BoxFuture, Channel, DeliveryStage, Environment, Lookup, ProviderAdapter,
    ProviderError, ProviderInfo, Submission, SubmitError, Tracking,
};
use crate::core::{Capability, Secret};
use crate::delegation::AuthorizedOperation;
use crate::documents::{Document, DocumentFormat};
use crate::http::{HttpError, Method, Request, Response, Transport};
use crate::operations::{Address, MailItem, Price, PriceKind};

const BASE_URL: &str = "https://api.lob.com";
const API_VERSION: &str = "2020-02-11";
const IDEMPOTENCY_HEADER: &str = "Idempotency-Key";

/// Lob accepts a letter's content inline as HTML only up to this length.
pub const MAX_INLINE_HTML: usize = 10_000;

pub struct LobAdapter<T> {
    transport: T,
    base_url: String,
}

impl<T: Transport> LobAdapter<T> {
    pub fn new(transport: T) -> Self {
        Self::with_base_url(transport, BASE_URL)
    }

    /// For tests: talk to a stand-in server.
    pub fn with_base_url(transport: T, base_url: impl Into<String>) -> Self {
        Self {
            transport,
            base_url: base_url.into(),
        }
    }

    fn request(
        &self,
        method: Method,
        path: &str,
        environment: Environment,
        credential: Option<&Secret>,
    ) -> Result<Request, ProviderError> {
        let key = credential.ok_or(ProviderError::CredentialRequired)?;
        check_key_environment(key, environment)?;
        Ok(Request::new(method, format!("{}{path}", self.base_url))
            // Lob: the API key is the basic-auth username, with no password.
            .basic_auth(key.expose(), None)
            .header("Lob-Version", API_VERSION))
    }
}

/// Lob keys say which environment they belong to. A live key must never be
/// used through a connection the user set up as a test connection.
fn check_key_environment(key: &Secret, environment: Environment) -> Result<(), ProviderError> {
    let (expected, name) = match environment {
        Environment::Test => ("test_", "test"),
        Environment::Live => ("live_", "live"),
    };
    if key.expose().starts_with(expected) {
        Ok(())
    } else {
        Err(ProviderError::Other(format!(
            "this is not a Lob {name} API key (those start with \"{expected}\")"
        )))
    }
}

/// Lob's error message, without echoing anything else from the body.
fn error_message(response: &Response) -> String {
    let message = response
        .json::<Value>()
        .ok()
        .and_then(|body| body["error"]["message"].as_str().map(str::to_owned));
    match message {
        Some(message) => format!("Lob ({}): {message}", response.status),
        None => format!("Lob returned status {}", response.status),
    }
}

fn address(a: &Address) -> Value {
    json!({
        "name": a.name,
        "address_line1": a.line1,
        "address_line2": a.line2,
        "address_city": a.city,
        "address_state": a.state,
        "address_zip": a.postal_code,
        "address_country": a.country,
    })
}

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The documents as one HTML letter, laid out to match the page estimate:
/// US Letter, one-inch margins, 12-point monospaced text at six lines to
/// the inch. Document text is escaped, so it cannot inject markup or load
/// anything (§12.4).
pub fn render_html(documents: &[Document]) -> Result<String, String> {
    let mut body = String::new();
    for (i, document) in documents.iter().enumerate() {
        if document.format != DocumentFormat::Text {
            return Err("Lob letters support plain-text documents only for now".into());
        }
        let text = std::str::from_utf8(&document.content)
            .map_err(|_| "a document is not valid UTF-8 text".to_owned())?;
        let class = if i == 0 { "doc" } else { "doc next" };
        body.push_str(&format!(
            "<div class=\"{class}\">{}</div>",
            escape_html(text)
        ));
    }
    let html = format!(
        "<html><head><meta charset=\"utf-8\"><style>\
         @page{{size:8.5in 11in;margin:1in}}\
         body{{margin:0;font-family:Courier,monospace;font-size:12pt;line-height:12pt}}\
         .doc{{white-space:pre-wrap;overflow-wrap:anywhere}}\
         .next{{page-break-before:always}}\
         </style></head><body>{body}</body></html>"
    );
    if html.len() > MAX_INLINE_HTML {
        return Err(format!(
            "the letter is too long to send to Lob as text ({} characters of HTML; \
             the limit is {MAX_INLINE_HTML})",
            html.len()
        ));
    }
    Ok(html)
}

/// What can be checked about an item before any money is involved.
fn validate(item: &MailItem) -> Result<(), ProviderError> {
    if item.product != "letter" {
        return Err(ProviderError::InvalidItem(format!(
            "Lob product {:?} is not supported",
            item.product
        )));
    }
    if item.from.is_none() {
        return Err(ProviderError::InvalidItem(
            "Lob requires a return address: supply `from`".into(),
        ));
    }
    Ok(())
}

fn stage(letter: &Value) -> DeliveryStage {
    if letter["deleted"] == true {
        return DeliveryStage::Cancelled;
    }
    let last = letter["tracking_events"]
        .as_array()
        .and_then(|events| events.last())
        .and_then(|event| event["name"].as_str());
    match last {
        None => DeliveryStage::Processing,
        Some("Mailed") => DeliveryStage::Mailed,
        Some("In Transit" | "In Local Area" | "Processed for Delivery" | "Re-Routed") => {
            DeliveryStage::InTransit
        }
        Some("Delivered") => DeliveryStage::Delivered,
        Some("Returned to Sender") => DeliveryStage::Returned,
        Some(_) => DeliveryStage::Unknown,
    }
}

impl<T: Transport + 'static> ProviderAdapter for LobAdapter<T> {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: "lob".into(),
            name: "Lob".into(),
            channels: vec![Channel::Postal],
            capabilities: vec![Capability::PostalSendLetter, Capability::PostalReadStatus],
            auth: AuthMethod::ApiKey,
            environments: vec![Environment::Test, Environment::Live],
        }
    }

    fn verify<'a>(
        &'a self,
        environment: Environment,
        credential: Option<&'a Secret>,
    ) -> BoxFuture<'a, Result<Vec<Capability>, ProviderError>> {
        Box::pin(async move {
            // The cheapest authenticated call: list at most one letter.
            let request =
                self.request(Method::Get, "/v1/letters?limit=1", environment, credential)?;
            let response = self.transport.send(request).await?;
            match response.status {
                200 => Ok(self.info().capabilities),
                401 | 403 => Err(ProviderError::Unauthorized),
                _ => Err(ProviderError::Other(error_message(&response))),
            }
        })
    }

    fn quote<'a>(&'a self, item: &'a MailItem) -> BoxFuture<'a, Result<Price, ProviderError>> {
        Box::pin(async move {
            validate(item)?;
            Ok(Price {
                kind: PriceKind::Unavailable,
                amount: None,
            })
        })
    }

    /// The inserted address page is printed and billed.
    fn billable_pages(&self, item: &MailItem) -> u32 {
        item.page_count.saturating_add(1)
    }

    fn submit<'a>(
        &'a self,
        authorized: &'a AuthorizedOperation,
        account: Account<'a>,
        item: &'a MailItem,
        documents: &'a [Document],
    ) -> BoxFuture<'a, Result<Submission, SubmitError>> {
        Box::pin(async move {
            // Everything up to the send is local: a failure here means
            // nothing reached Lob.
            let rejected = |e: ProviderError| SubmitError::Rejected(e.to_string());
            validate(item).map_err(rejected)?;
            let html = render_html(documents).map_err(SubmitError::Rejected)?;
            let operation_id = authorized.operation_id();
            let request = self
                .request(
                    Method::Post,
                    "/v1/letters",
                    account.environment,
                    account.credential,
                )
                .map_err(rejected)?
                .idempotency_key(IDEMPOTENCY_HEADER, operation_id)
                .json(json!({
                    "description": format!("postal-mcp {operation_id}"),
                    "to": address(&item.to),
                    "from": item.from.as_ref().map(address),
                    "file": html,
                    "color": false,
                    "double_sided": true,
                    "address_placement": "insert_blank_page",
                    "mail_type": "usps_first_class",
                    "use_type": "operational",
                    "metadata": {"operation_id": operation_id},
                }));

            let response = match self.transport.send(request).await {
                Ok(response) => response,
                Err(e @ (HttpError::NotSent(_) | HttpError::InvalidRequest(_))) => {
                    return Err(SubmitError::Rejected(e.to_string()));
                }
                Err(e @ (HttpError::Ambiguous(_) | HttpError::ResponseTooLarge { .. })) => {
                    return Err(SubmitError::Uncertain(e.to_string()));
                }
            };
            match response.status {
                200..=299 => match response.json::<Value>() {
                    Ok(letter) if letter["id"].is_string() => Ok(Submission {
                        reference: letter["id"].as_str().unwrap_or_default().to_owned(),
                        // Lob does not report the charge on the letter.
                        charged: None,
                    }),
                    _ => Err(SubmitError::Uncertain(
                        "Lob accepted the letter but its reply could not be read".into(),
                    )),
                },
                // A conflict means the same key is already being processed.
                409 => Err(SubmitError::Uncertain(error_message(&response))),
                // Lob refused the request, so it created nothing.
                400..=499 => Err(SubmitError::Rejected(error_message(&response))),
                // A server error can follow a letter that was created.
                _ => Err(SubmitError::Uncertain(error_message(&response))),
            }
        })
    }

    fn lookup<'a>(
        &'a self,
        operation_id: &'a str,
        account: Account<'a>,
    ) -> BoxFuture<'a, Result<Lookup, ProviderError>> {
        Box::pin(async move {
            // Operation ids are [a-z0-9_], so they need no escaping.
            let path = format!("/v1/letters?limit=1&metadata%5Boperation_id%5D={operation_id}");
            let request =
                self.request(Method::Get, &path, account.environment, account.credential)?;
            let response = self.transport.send(request).await?;
            if response.status != 200 {
                return Err(ProviderError::Other(error_message(&response)));
            }
            let list: Value = response
                .json()
                .map_err(|_| ProviderError::Other("Lob's reply could not be read".into()))?;
            Ok(match list["data"][0]["id"].as_str() {
                Some(id) => Lookup::Found(Submission {
                    reference: id.to_owned(),
                    charged: None,
                }),
                None => Lookup::Unknown,
            })
        })
    }

    fn track<'a>(
        &'a self,
        reference: &'a str,
        account: Account<'a>,
    ) -> BoxFuture<'a, Result<Tracking, ProviderError>> {
        Box::pin(async move {
            if !reference.starts_with("ltr_")
                || !reference
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_')
            {
                return Err(ProviderError::Other("not a Lob letter id".into()));
            }
            let request = self.request(
                Method::Get,
                &format!("/v1/letters/{reference}"),
                account.environment,
                account.credential,
            )?;
            let response = self.transport.send(request).await?;
            if response.status != 200 {
                return Err(ProviderError::Other(error_message(&response)));
            }
            let letter: Value = response
                .json()
                .map_err(|_| ProviderError::Other("Lob's reply could not be read".into()))?;
            let text = |field: &str| letter[field].as_str().map(str::to_owned);
            Ok(Tracking {
                stage: stage(&letter),
                detail: letter["tracking_events"]
                    .as_array()
                    .and_then(|events| events.last())
                    .and_then(|event| event["name"].as_str())
                    .map(str::to_owned),
                expected_delivery_date: text("expected_delivery_date"),
            })
        })
    }
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
    fn html_escapes_document_text() {
        let text = "Dear <b>Jane</b> & co,\n<script src=\"http://evil.example/x.js\"></script>";
        let html = render_html(&[document(DocumentFormat::Text, text)]).unwrap();
        assert!(html.contains("Dear &lt;b&gt;Jane&lt;/b&gt; &amp; co,"));
        assert!(!html.contains("<script"));
        assert!(!html.contains("<b>"));
    }

    #[test]
    fn html_separates_documents_and_enforces_lob_limit() {
        let one = document(DocumentFormat::Text, "first");
        let html = render_html(&[one.clone(), one.clone()]).unwrap();
        assert_eq!(html.matches("class=\"doc\"").count(), 1);
        assert_eq!(html.matches("class=\"doc next\"").count(), 1);

        let long = document(DocumentFormat::Text, &"x".repeat(MAX_INLINE_HTML));
        assert!(render_html(&[long]).unwrap_err().contains("too long"));
        let markdown = document(DocumentFormat::Markdown, "# Hi");
        assert!(render_html(&[markdown]).unwrap_err().contains("plain-text"));
    }

    #[test]
    fn key_must_match_the_connection_environment() {
        let test = Secret::new("test_abc");
        let live = Secret::new("live_abc");
        assert!(check_key_environment(&test, Environment::Test).is_ok());
        assert!(check_key_environment(&live, Environment::Live).is_ok());
        assert!(check_key_environment(&live, Environment::Test).is_err());
        assert!(check_key_environment(&test, Environment::Live).is_err());
        let error = check_key_environment(&live, Environment::Test).unwrap_err();
        assert!(!error.to_string().contains("live_abc"));
    }

    #[test]
    fn tracking_events_map_to_stages() {
        let with = |name: &str| json!({"tracking_events": [{"name": "Mailed"}, {"name": name}]});
        assert_eq!(
            stage(&json!({"tracking_events": []})),
            DeliveryStage::Processing
        );
        assert_eq!(stage(&json!({})), DeliveryStage::Processing);
        assert_eq!(stage(&with("Mailed")), DeliveryStage::Mailed);
        assert_eq!(stage(&with("In Local Area")), DeliveryStage::InTransit);
        assert_eq!(stage(&with("Delivered")), DeliveryStage::Delivered);
        assert_eq!(stage(&with("Returned to Sender")), DeliveryStage::Returned);
        assert_eq!(stage(&with("Something New")), DeliveryStage::Unknown);
        assert_eq!(stage(&json!({"deleted": true})), DeliveryStage::Cancelled);
    }
}
