//! Stable (https://docs.usestable.com): a virtual mailbox. Mail sent to the
//! account's address is received and scanned by Stable.
//!
//! Built from Stable's published API and tested against a local stand-in,
//! not yet against Stable itself. Read-only: this adapter lists mailbox
//! addresses and received mail. It requests no scans, forwards or shreds.
//! Choices that follow from Stable:
//!
//! - **Two hosts.** A test connection talks to Stable's sandbox and a live
//!   one to production. Sandbox keys start with `stbl_dev_`, and a key is
//!   only ever sent to the host it belongs to.
//! - **Temporary links.** The envelope image and the scanned document are
//!   links Stable signs for a limited time. They are passed on as given,
//!   never stored; reading the item again gives fresh ones.
//! - **Lenient reading.** Only an item's `id` is required. Stable's
//!   reference lists `scanDetails` as always present, so an item counts as
//!   scanned only when its scan status says so.
//! - **Received time.** Stable filters by `createdAt` but its reference
//!   does not list that field on a mail item. It is read when present.

use serde_json::Value;

use super::{
    Account, AuthMethod, BoxFuture, Channel, Environment, Lookup, ProviderAdapter, ProviderError,
    ProviderInfo, Submission, SubmitError, Tracking,
};
use crate::core::{Capability, Secret};
use crate::delegation::AuthorizedOperation;
use crate::documents::Document;
use crate::http::{Method, Request, Response, Transport};
use crate::inbound::{
    Endpoint, EndpointKind, EndpointStatus, InboundItem, InboxPage, InboxQuery, MailboxAddress,
    Scan, ScanStatus,
};
use crate::operations::{MailItem, Price};

const LIVE_URL: &str = "https://api.usestable.com";
const SANDBOX_URL: &str = "https://api.dev.usestable.com";
const KEY_HEADER: &str = "x-api-key";
const SANDBOX_KEY_PREFIX: &str = "stbl_dev_";
/// Stable's mailboxes are in the United States.
const COUNTRY: &str = "US";
/// Locations fetched in one call. An account has a handful.
const LOCATIONS_PAGE: u32 = 100;

const RECEIVES_ONLY: &str = "Stable receives mail; it does not send it";

pub struct StableAdapter<T> {
    transport: T,
    /// Set only by tests. Otherwise the host follows the environment.
    base_url: Option<String>,
}

impl<T: Transport> StableAdapter<T> {
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            base_url: None,
        }
    }

    /// For tests: talk to a stand-in server.
    pub fn with_base_url(transport: T, base_url: impl Into<String>) -> Self {
        Self {
            transport,
            base_url: Some(base_url.into()),
        }
    }

    /// A GET of `path_and_query`. Everything this adapter does is a GET.
    async fn get(
        &self,
        path_and_query: &str,
        environment: Environment,
        credential: Option<&Secret>,
    ) -> Result<Response, ProviderError> {
        let key = credential.ok_or(ProviderError::CredentialRequired)?;
        check_key_environment(key, environment)?;
        let base = match (&self.base_url, environment) {
            (Some(base), _) => base.as_str(),
            (None, Environment::Test) => SANDBOX_URL,
            (None, Environment::Live) => LIVE_URL,
        };
        let request = Request::new(Method::Get, format!("{base}{path_and_query}"))
            .header(KEY_HEADER, key.expose());
        Ok(self.transport.send(request).await?)
    }
}

/// A sandbox key works only on the sandbox, and a test connection must
/// never read a real mailbox.
fn check_key_environment(key: &Secret, environment: Environment) -> Result<(), ProviderError> {
    let sandbox_key = key.expose().starts_with(SANDBOX_KEY_PREFIX);
    match (environment, sandbox_key) {
        (Environment::Test, true) | (Environment::Live, false) => Ok(()),
        (Environment::Test, false) => Err(ProviderError::Other(format!(
            "this is not a Stable sandbox API key (those start with \
             \"{SANDBOX_KEY_PREFIX}\"); connect a production key with the live environment"
        ))),
        (Environment::Live, true) => Err(ProviderError::Other(
            "this is a Stable sandbox API key; connect it with the test environment".into(),
        )),
    }
}

/// Stable's error code and message, without echoing anything else.
fn failure(response: &Response) -> ProviderError {
    let body = response.json::<Value>().ok();
    let field = |name: &str| {
        body.as_ref()
            .and_then(|body| body[name].as_str())
            .map(str::to_owned)
    };
    let code = field("code");
    // Stable answers a bad key with 400 INVALID_API_KEY, not 401.
    if matches!(response.status, 401 | 403) || code.as_deref() == Some("INVALID_API_KEY") {
        return ProviderError::Unauthorized;
    }
    ProviderError::Other(match field("message") {
        Some(message) => format!("Stable ({}): {message}", response.status),
        None => format!("Stable returned status {}", response.status),
    })
}

fn body(response: &Response) -> Result<Value, ProviderError> {
    response
        .json::<Value>()
        .map_err(|_| ProviderError::Other("Stable sent a reply this server could not read".into()))
}

/// A query-string value, percent-encoded.
fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// A non-empty string field.
fn text(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// The nodes of one page of a Stable list, and the cursor for the next.
fn page(list: &Value) -> Result<(Vec<&Value>, Option<String>), ProviderError> {
    let edges = list["edges"].as_array().ok_or_else(|| {
        ProviderError::Other("Stable sent a list this server could not read".into())
    })?;
    let nodes = edges.iter().map(|edge| &edge["node"]).collect();
    let info = &list["pageInfo"];
    let next = if info["hasNextPage"] == true {
        text(&info["endCursor"])
    } else {
        None
    };
    Ok((nodes, next))
}

fn endpoint(location: &Value) -> Option<Endpoint> {
    let endpoint_id = text(&location["id"])?;
    let a = &location["address"];
    let address = match (text(&a["line1"]), text(&a["city"]), text(&a["postalCode"])) {
        (Some(line1), Some(city), Some(postal_code)) => Some(MailboxAddress {
            line1,
            line2: text(&a["line2"]),
            city,
            state: text(&a["state"]),
            postal_code,
            country: COUNTRY.into(),
        }),
        _ => None,
    };
    let onboarding = location["onboarding"]["status"].as_str();
    // Stable receives mail at a location once its paperwork is complete.
    let status = match (location["status"].as_str(), onboarding) {
        (Some("active"), Some("complete") | None) => EndpointStatus::Active,
        (Some("active" | "inactive"), _) => EndpointStatus::Inactive,
        _ => EndpointStatus::Unknown,
    };
    let detail = onboarding
        .filter(|step| *step != "complete")
        .map(|step| format!("Stable onboarding is at the \"{step}\" step"));
    Some(Endpoint {
        endpoint_id,
        kind: EndpointKind::PostalMailbox,
        status,
        address,
        detail,
    })
}

fn item(mail: &Value) -> Option<InboundItem> {
    let item_id = text(&mail["id"])?;
    let recipients = &mail["recipients"];
    let recipient = match (
        text(&recipients["line1"]["text"]),
        text(&recipients["line2"]["text"]),
    ) {
        (Some(line1), Some(line2)) => Some(format!("{line1}, {line2}")),
        (line1, line2) => line1.or(line2),
    };
    let details = &mail["scanDetails"];
    let scan_status = match details["status"].as_str() {
        Some("completed") => ScanStatus::Completed,
        Some("processing") => ScanStatus::Processing,
        _ => ScanStatus::None,
    };
    let scan = (scan_status == ScanStatus::Completed).then(|| Scan {
        summary: text(&details["summary"]),
        document_url: text(&details["imageUrl"]),
    });
    Some(InboundItem {
        item_id,
        endpoint_id: text(&mail["location"]["id"]),
        sender: text(&mail["from"]),
        recipient,
        received_at: text(&mail["createdAt"]),
        returned_to_sender: mail["isReturnedToSender"] == true,
        scan_status,
        envelope_image_url: text(&mail["imageUrl"]),
        scan,
    })
}

impl<T: Transport + 'static> ProviderAdapter for StableAdapter<T> {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: "stable".into(),
            name: "Stable".into(),
            channels: vec![Channel::Mailbox],
            capabilities: vec![
                Capability::MailboxReadItems,
                Capability::MailboxReadEnvelopes,
                Capability::MailboxReadScans,
                Capability::EndpointRead,
            ],
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
            // The cheapest authenticated call: list at most one location.
            let response = self
                .get("/v1/locations?first=1", environment, credential)
                .await?;
            match response.status {
                200 => Ok(self.info().capabilities),
                _ => Err(failure(&response)),
            }
        })
    }

    fn credential_environment(&self, credential: &Secret) -> Option<Environment> {
        // Stable documents a prefix for sandbox keys only.
        credential
            .expose()
            .starts_with(SANDBOX_KEY_PREFIX)
            .then_some(Environment::Test)
    }

    fn quote<'a>(&'a self, _: &'a MailItem) -> BoxFuture<'a, Result<Price, ProviderError>> {
        Box::pin(async { Err(ProviderError::InvalidItem(RECEIVES_ONLY.into())) })
    }

    fn submit<'a>(
        &'a self,
        _: &'a AuthorizedOperation,
        _: Account<'a>,
        _: &'a MailItem,
        _: &'a [Document],
    ) -> BoxFuture<'a, Result<Submission, SubmitError>> {
        // Nothing is sent, so this is a definite rejection.
        Box::pin(async { Err(SubmitError::Rejected(RECEIVES_ONLY.into())) })
    }

    fn lookup<'a>(
        &'a self,
        _: &'a str,
        _: Account<'a>,
    ) -> BoxFuture<'a, Result<Lookup, ProviderError>> {
        // This adapter never submits, so there is no order to find.
        Box::pin(async { Ok(Lookup::NotFound) })
    }

    fn track<'a>(
        &'a self,
        _: &'a str,
        _: Account<'a>,
    ) -> BoxFuture<'a, Result<Tracking, ProviderError>> {
        Box::pin(async { Err(ProviderError::Other(RECEIVES_ONLY.into())) })
    }

    fn endpoints<'a>(
        &'a self,
        account: Account<'a>,
    ) -> BoxFuture<'a, Result<Vec<Endpoint>, ProviderError>> {
        Box::pin(async move {
            let path = format!("/v1/locations?first={LOCATIONS_PAGE}");
            let response = self
                .get(&path, account.environment, account.credential)
                .await?;
            if response.status != 200 {
                return Err(failure(&response));
            }
            let list = body(&response)?;
            let (locations, _) = page(&list)?;
            Ok(locations.into_iter().filter_map(endpoint).collect())
        })
    }

    fn inbox<'a>(
        &'a self,
        account: Account<'a>,
        query: &'a InboxQuery,
    ) -> BoxFuture<'a, Result<InboxPage, ProviderError>> {
        Box::pin(async move {
            let mut path = format!("/v1/mail-items?first={}", query.limit);
            let filters = [
                ("locationId", &query.endpoint_id),
                ("createdAt_gte", &query.received_after),
                ("after", &query.cursor),
            ];
            for (name, value) in filters {
                if let Some(value) = value {
                    path.push_str(&format!("&{name}={}", encode(value)));
                }
            }
            let response = self
                .get(&path, account.environment, account.credential)
                .await?;
            if response.status != 200 {
                return Err(failure(&response));
            }
            let list = body(&response)?;
            let (mail, next_cursor) = page(&list)?;
            Ok(InboxPage {
                items: mail.into_iter().filter_map(item).collect(),
                next_cursor,
            })
        })
    }

    fn inbound_item<'a>(
        &'a self,
        account: Account<'a>,
        item_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<InboundItem>, ProviderError>> {
        Box::pin(async move {
            // The caller has checked that the id is plain. Encoding it as
            // well means it can never change the path.
            let path = format!("/v1/mail-items/{}", encode(item_id));
            let response = self
                .get(&path, account.environment, account.credential)
                .await?;
            match response.status {
                200 => item(&body(&response)?).map(Some).ok_or_else(|| {
                    ProviderError::Other("Stable sent an item this server could not read".into())
                }),
                404 => Ok(None),
                _ => Err(failure(&response)),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn query_values_are_percent_encoded() {
        assert_eq!(encode("abc-_.~123"), "abc-_.~123");
        assert_eq!(encode("a b&c=d/e"), "a%20b%26c%3Dd%2Fe");
        assert_eq!(encode("2026-01-31T00:00:00Z"), "2026-01-31T00%3A00%3A00Z");
    }

    #[test]
    fn a_key_is_only_used_in_its_own_environment() {
        let sandbox = Secret::new("stbl_dev_abc");
        let production = Secret::new("abc");
        assert!(check_key_environment(&sandbox, Environment::Test).is_ok());
        assert!(check_key_environment(&production, Environment::Live).is_ok());
        assert!(check_key_environment(&sandbox, Environment::Live).is_err());
        assert!(check_key_environment(&production, Environment::Test).is_err());
    }

    #[test]
    fn an_item_is_scanned_only_when_its_scan_is_complete() {
        let unopened = item(&json!({
            "id": "mi_1",
            "imageUrl": "https://files.example/envelope",
            "scanDetails": {"imageUrl": "", "status": null},
        }))
        .unwrap();
        assert_eq!(unopened.scan_status, ScanStatus::None);
        assert_eq!(unopened.scan, None);

        let processing = item(&json!({
            "id": "mi_2",
            "scanDetails": {"imageUrl": "https://files.example/partial", "status": "processing"},
        }))
        .unwrap();
        assert_eq!(processing.scan_status, ScanStatus::Processing);
        assert_eq!(processing.scan, None);

        assert_eq!(item(&json!({"from": "no id"})), None);
    }

    #[test]
    fn a_location_receives_mail_only_once_onboarded() {
        let location = |status: &str, onboarding: &str| {
            endpoint(&json!({
                "id": "loc_1",
                "status": status,
                "onboarding": {"status": onboarding},
                "address": {"line1": "1 Main St", "city": "Austin", "state": "TX", "postalCode": "78701"},
            }))
            .unwrap()
        };
        assert_eq!(
            location("active", "complete").status,
            EndpointStatus::Active
        );
        assert_eq!(location("active", "complete").detail, None);
        let pending = location("active", "verify");
        assert_eq!(pending.status, EndpointStatus::Inactive);
        assert!(pending.detail.unwrap().contains("verify"));
        assert_eq!(
            location("inactive", "complete").status,
            EndpointStatus::Inactive
        );
        assert_eq!(
            location("closed", "complete").status,
            EndpointStatus::Unknown
        );
    }
}
