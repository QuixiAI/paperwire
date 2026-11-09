use std::fmt;
use std::str::FromStr;

use rmcp::schemars::{self, JsonSchema};
use serde::{Deserialize, Serialize};

/// Normalized capability identifiers (design.md §3.5). These are
/// application-level names, not MCP protocol scopes.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
pub enum Capability {
    #[serde(rename = "postal.send.letter")]
    PostalSendLetter,
    #[serde(rename = "postal.send.postcard")]
    PostalSendPostcard,
    #[serde(rename = "postal.send.certified")]
    PostalSendCertified,
    #[serde(rename = "postal.read.status")]
    PostalReadStatus,
    #[serde(rename = "postal.read.evidence")]
    PostalReadEvidence,

    #[serde(rename = "mailbox.read.items")]
    MailboxReadItems,
    #[serde(rename = "mailbox.read.envelopes")]
    MailboxReadEnvelopes,
    #[serde(rename = "mailbox.read.scans")]
    MailboxReadScans,
    #[serde(rename = "mailbox.request.scan")]
    MailboxRequestScan,
    #[serde(rename = "mailbox.request.forward")]
    MailboxRequestForward,
    #[serde(rename = "mailbox.request.shred")]
    MailboxRequestShred,

    #[serde(rename = "fax.send")]
    FaxSend,
    #[serde(rename = "fax.read.inbox")]
    FaxReadInbox,
    #[serde(rename = "fax.read.document")]
    FaxReadDocument,
    #[serde(rename = "fax.read.status")]
    FaxReadStatus,
    #[serde(rename = "fax.read.confirmation")]
    FaxReadConfirmation,

    #[serde(rename = "endpoint.read")]
    EndpointRead,
    #[serde(rename = "endpoint.provision")]
    EndpointProvision,
    #[serde(rename = "endpoint.release")]
    EndpointRelease,
}

impl Capability {
    pub const ALL: [Capability; 19] = [
        Self::PostalSendLetter,
        Self::PostalSendPostcard,
        Self::PostalSendCertified,
        Self::PostalReadStatus,
        Self::PostalReadEvidence,
        Self::MailboxReadItems,
        Self::MailboxReadEnvelopes,
        Self::MailboxReadScans,
        Self::MailboxRequestScan,
        Self::MailboxRequestForward,
        Self::MailboxRequestShred,
        Self::FaxSend,
        Self::FaxReadInbox,
        Self::FaxReadDocument,
        Self::FaxReadStatus,
        Self::FaxReadConfirmation,
        Self::EndpointRead,
        Self::EndpointProvision,
        Self::EndpointRelease,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::PostalSendLetter => "postal.send.letter",
            Self::PostalSendPostcard => "postal.send.postcard",
            Self::PostalSendCertified => "postal.send.certified",
            Self::PostalReadStatus => "postal.read.status",
            Self::PostalReadEvidence => "postal.read.evidence",
            Self::MailboxReadItems => "mailbox.read.items",
            Self::MailboxReadEnvelopes => "mailbox.read.envelopes",
            Self::MailboxReadScans => "mailbox.read.scans",
            Self::MailboxRequestScan => "mailbox.request.scan",
            Self::MailboxRequestForward => "mailbox.request.forward",
            Self::MailboxRequestShred => "mailbox.request.shred",
            Self::FaxSend => "fax.send",
            Self::FaxReadInbox => "fax.read.inbox",
            Self::FaxReadDocument => "fax.read.document",
            Self::FaxReadStatus => "fax.read.status",
            Self::FaxReadConfirmation => "fax.read.confirmation",
            Self::EndpointRead => "endpoint.read",
            Self::EndpointProvision => "endpoint.provision",
            Self::EndpointRelease => "endpoint.release",
        }
    }
}

impl Capability {
    /// Read capabilities have no real-world effect and cost nothing, so a
    /// grant's sending constraints (§5.3) do not apply to them.
    pub fn is_read(self) -> bool {
        self.as_str().contains(".read")
    }

    /// Sends a physical mail piece.
    pub fn is_postal_send(self) -> bool {
        matches!(
            self,
            Self::PostalSendLetter | Self::PostalSendPostcard | Self::PostalSendCertified
        )
    }
}

impl fmt::Display for Capability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, thiserror::Error)]
#[error("unknown capability: {0}")]
pub struct UnknownCapability(pub String);

impl FromStr for Capability {
    type Err = UnknownCapability;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|c| c.as_str() == s)
            .ok_or_else(|| UnknownCapability(s.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_form_round_trips_through_serde_and_from_str() {
        for cap in Capability::ALL {
            let json = serde_json::to_string(&cap).unwrap();
            assert_eq!(json, format!("\"{}\"", cap.as_str()));
            assert_eq!(serde_json::from_str::<Capability>(&json).unwrap(), cap);
            assert_eq!(cap.as_str().parse::<Capability>().unwrap(), cap);
        }
    }

    #[test]
    fn unknown_capability_is_rejected() {
        assert!("postal.send.everything".parse::<Capability>().is_err());
    }
}
