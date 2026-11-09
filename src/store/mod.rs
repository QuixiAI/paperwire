//! SQLite persistence (design.md §18): connections, grants, usage and audit
//! events. Credentials are not stored here.
//!
//! The server and the admin CLI are separate processes on the same file, so
//! nothing is cached: every call reads current state.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use rusqlite::{OptionalExtension, Row, params};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::connections::{Connection, ConnectionStatus, CredentialRef};
use crate::core::{Money, Timestamp, clock};
use crate::delegation::{Grant, Usage};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("cannot create data directory: {0}")]
    Io(#[from] std::io::Error),
    #[error("database holds an invalid {0}")]
    Corrupt(&'static str),
    #[error("database schema version {0} is newer than this program supports")]
    TooNew(i64),
}

/// A security-relevant event (design.md §6.5).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuditEvent {
    /// E.g. `grant.created`, `authorization.checked`.
    pub kind: String,
    pub principal_id: Option<String>,
    pub connection_id: Option<String>,
    pub grant_id: Option<String>,
    pub action: Option<String>,
    pub decision: Option<String>,
    pub reason: Option<String>,
    pub policy_version: Option<u32>,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditRecord {
    pub id: i64,
    pub at: Timestamp,
    pub event: AuditEvent,
}

/// Where state lives when `--data-dir` is not given:
/// `$XDG_DATA_HOME/postal-mcp`, else `~/.local/share/postal-mcp`.
pub fn default_data_dir() -> Option<PathBuf> {
    let non_empty = |name| std::env::var_os(name).filter(|v| !v.is_empty());
    let base = match non_empty("XDG_DATA_HOME") {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from(non_empty("HOME")?).join(".local/share"),
    };
    Some(base.join("postal-mcp"))
}

const SCHEMA_VERSION: i64 = 1;

const SCHEMA_V1: &str = "
CREATE TABLE provider_connections (
    id           TEXT PRIMARY KEY,
    provider     TEXT NOT NULL,
    environment  TEXT NOT NULL,
    status       TEXT NOT NULL,
    capabilities TEXT NOT NULL,
    credential   TEXT NOT NULL,
    created_at   INTEGER NOT NULL
);
CREATE TABLE delegation_grants (
    id             TEXT PRIMARY KEY,
    principal_id   TEXT NOT NULL,
    connection_ids TEXT NOT NULL,
    actions        TEXT NOT NULL,
    constraints    TEXT NOT NULL,
    policy_version INTEGER NOT NULL,
    created_at     INTEGER NOT NULL,
    expires_at     INTEGER,
    revoked_at     INTEGER
);
CREATE INDEX delegation_grants_principal ON delegation_grants (principal_id);
CREATE TABLE usage_ledger (
    id           INTEGER PRIMARY KEY,
    grant_id     TEXT NOT NULL REFERENCES delegation_grants (id),
    operation_id TEXT NOT NULL,
    currency     TEXT,
    amount_minor INTEGER NOT NULL,
    items        INTEGER NOT NULL,
    occurred_at  INTEGER NOT NULL
);
CREATE INDEX usage_ledger_grant ON usage_ledger (grant_id, occurred_at);
CREATE TABLE audit_events (
    id             INTEGER PRIMARY KEY,
    at             INTEGER NOT NULL,
    kind           TEXT NOT NULL,
    principal_id   TEXT,
    connection_id  TEXT,
    grant_id       TEXT,
    action         TEXT,
    decision       TEXT,
    reason         TEXT,
    policy_version INTEGER,
    detail         TEXT
);
";

/// Calls are short, synchronous and serialized by a mutex. That is enough
/// for a local server with one client.
#[derive(Clone)]
pub struct Store {
    db: Arc<Mutex<rusqlite::Connection>>,
}

impl Store {
    pub const FILE_NAME: &'static str = "postal-mcp.db";

    /// Open (creating if needed) the database in `data_dir`.
    pub fn open(data_dir: &Path) -> Result<Self, StoreError> {
        std::fs::create_dir_all(data_dir)?;
        let db = rusqlite::Connection::open(data_dir.join(Self::FILE_NAME))?;
        // The admin CLI may write while the server reads.
        db.pragma_update(None, "journal_mode", "WAL")?;
        db.busy_timeout(Duration::from_secs(5))?;
        Self::init(db)
    }

    pub fn open_in_memory() -> Result<Self, StoreError> {
        Self::init(rusqlite::Connection::open_in_memory()?)
    }

    fn init(mut db: rusqlite::Connection) -> Result<Self, StoreError> {
        db.pragma_update(None, "foreign_keys", true)?;
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let version: i64 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
        match version {
            0 => {
                tx.execute_batch(SCHEMA_V1)?;
                tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            }
            SCHEMA_VERSION => {}
            newer => return Err(StoreError::TooNew(newer)),
        }
        tx.commit()?;
        Ok(Self {
            db: Arc::new(Mutex::new(db)),
        })
    }

    fn db(&self) -> MutexGuard<'_, rusqlite::Connection> {
        // A panic while holding the lock cannot leave SQLite half-written.
        self.db.lock().unwrap_or_else(|e| e.into_inner())
    }

    // --- connections -----------------------------------------------------

    pub fn insert_connection(&self, c: &Connection) -> Result<(), StoreError> {
        self.db().execute(
            "INSERT INTO provider_connections
             (id, provider, environment, status, capabilities, credential, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                c.id,
                c.provider,
                enum_text(&c.environment),
                enum_text(&c.status),
                to_json(&c.capabilities),
                c.credential.encode(),
                c.created_at.unix_timestamp(),
            ],
        )?;
        Ok(())
    }

    pub fn connection(&self, id: &str) -> Result<Option<Connection>, StoreError> {
        self.db()
            .query_row(
                &format!("{CONNECTION_SELECT} WHERE id = ?1"),
                [id],
                connection_row,
            )
            .optional()?
            .transpose()
    }

    pub fn connections(&self) -> Result<Vec<Connection>, StoreError> {
        let db = self.db();
        let mut statement = db.prepare(&format!("{CONNECTION_SELECT} ORDER BY created_at, id"))?;
        let rows = statement.query_map([], connection_row)?;
        rows.map(|row| row?).collect()
    }

    /// Returns whether the connection exists.
    pub fn set_connection_status(
        &self,
        id: &str,
        status: ConnectionStatus,
    ) -> Result<bool, StoreError> {
        let changed = self.db().execute(
            "UPDATE provider_connections SET status = ?2 WHERE id = ?1",
            params![id, enum_text(&status)],
        )?;
        Ok(changed == 1)
    }

    // --- grants ----------------------------------------------------------

    pub fn insert_grant(&self, g: &Grant) -> Result<(), StoreError> {
        self.db().execute(
            "INSERT INTO delegation_grants
             (id, principal_id, connection_ids, actions, constraints, policy_version,
              created_at, expires_at, revoked_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                g.id,
                g.principal_id,
                to_json(&g.connection_ids),
                to_json(&g.actions),
                to_json(&g.constraints),
                g.policy_version,
                g.created_at.unix_timestamp(),
                g.expires_at.map(Timestamp::unix_timestamp),
                g.revoked_at.map(Timestamp::unix_timestamp),
            ],
        )?;
        Ok(())
    }

    pub fn grant(&self, id: &str) -> Result<Option<Grant>, StoreError> {
        self.db()
            .query_row(&format!("{GRANT_SELECT} WHERE id = ?1"), [id], grant_row)
            .optional()?
            .transpose()
    }

    /// Every grant, revoked and expired ones included, optionally for one
    /// principal.
    pub fn grants(&self, principal: Option<&str>) -> Result<Vec<Grant>, StoreError> {
        let db = self.db();
        let mut statement = db.prepare(&format!(
            "{GRANT_SELECT} WHERE ?1 IS NULL OR principal_id = ?1 ORDER BY created_at, id"
        ))?;
        let rows = statement.query_map([principal], grant_row)?;
        rows.map(|row| row?).collect()
    }

    /// Returns whether a grant was revoked by this call (false if it does
    /// not exist or was already revoked).
    pub fn revoke_grant(&self, id: &str, at: Timestamp) -> Result<bool, StoreError> {
        let changed = self.db().execute(
            "UPDATE delegation_grants SET revoked_at = ?2
             WHERE id = ?1 AND revoked_at IS NULL",
            params![id, at.unix_timestamp()],
        )?;
        Ok(changed == 1)
    }

    // --- usage -----------------------------------------------------------

    pub fn record_usage(
        &self,
        grant_id: &str,
        operation_id: &str,
        cost: Option<&Money>,
        items: u32,
        at: Timestamp,
    ) -> Result<(), StoreError> {
        self.db().execute(
            "INSERT INTO usage_ledger
             (grant_id, operation_id, currency, amount_minor, items, occurred_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                grant_id,
                operation_id,
                cost.map(|c| c.currency.as_str()),
                cost.map_or(0, |c| c.amount_minor),
                items,
                at.unix_timestamp(),
            ],
        )?;
        Ok(())
    }

    /// What `grant_id` has used: spend in `currency` since the start of the
    /// UTC month, and items since the start of the UTC day.
    pub fn usage(
        &self,
        grant_id: &str,
        currency: Option<&str>,
        now: Timestamp,
    ) -> Result<Usage, StoreError> {
        let db = self.db();
        let spent_this_month_minor = db.query_row(
            "SELECT COALESCE(SUM(amount_minor), 0) FROM usage_ledger
             WHERE grant_id = ?1 AND currency = ?2 AND occurred_at >= ?3",
            params![
                grant_id,
                currency,
                clock::start_of_month(now).unix_timestamp()
            ],
            |r| r.get(0),
        )?;
        let items_today = db.query_row(
            "SELECT COALESCE(SUM(items), 0) FROM usage_ledger
             WHERE grant_id = ?1 AND occurred_at >= ?2",
            params![grant_id, clock::start_of_day(now).unix_timestamp()],
            |r| r.get(0),
        )?;
        Ok(Usage {
            spent_this_month_minor,
            items_today,
        })
    }

    // --- audit -----------------------------------------------------------

    pub fn audit(&self, e: &AuditEvent, at: Timestamp) -> Result<(), StoreError> {
        self.db().execute(
            "INSERT INTO audit_events
             (at, kind, principal_id, connection_id, grant_id, action, decision, reason,
              policy_version, detail)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                at.unix_timestamp(),
                e.kind,
                e.principal_id,
                e.connection_id,
                e.grant_id,
                e.action,
                e.decision,
                e.reason,
                e.policy_version,
                e.detail,
            ],
        )?;
        Ok(())
    }

    /// The most recent events, newest first.
    pub fn audit_events(&self, limit: u32) -> Result<Vec<AuditRecord>, StoreError> {
        let db = self.db();
        let mut statement = db.prepare(
            "SELECT id, at, kind, principal_id, connection_id, grant_id, action, decision,
                    reason, policy_version, detail
             FROM audit_events ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = statement.query_map([limit], audit_row)?;
        rows.map(|row| row?).collect()
    }
}

const CONNECTION_SELECT: &str = "SELECT id, provider, environment, status, capabilities, \
     credential, created_at FROM provider_connections";

const GRANT_SELECT: &str = "SELECT id, principal_id, connection_ids, actions, constraints, \
     policy_version, created_at, expires_at, revoked_at FROM delegation_grants";

type Decoded<T> = rusqlite::Result<Result<T, StoreError>>;

fn connection_row(r: &Row<'_>) -> Decoded<Connection> {
    let (environment, status, capabilities, credential, created_at): (
        String,
        String,
        String,
        String,
        i64,
    ) = (r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?);
    let (id, provider) = (r.get(0)?, r.get(1)?);
    Ok((|| {
        Ok(Connection {
            id,
            provider,
            environment: enum_from_text(&environment, "environment")?,
            status: enum_from_text(&status, "connection status")?,
            capabilities: from_json(&capabilities, "capability list")?,
            credential: CredentialRef::decode(&credential)
                .ok_or(StoreError::Corrupt("credential reference"))?,
            created_at: timestamp(created_at)?,
        })
    })())
}

fn grant_row(r: &Row<'_>) -> Decoded<Grant> {
    let (connection_ids, actions, constraints): (String, String, String) =
        (r.get(2)?, r.get(3)?, r.get(4)?);
    let (created_at, expires_at, revoked_at): (i64, Option<i64>, Option<i64>) =
        (r.get(6)?, r.get(7)?, r.get(8)?);
    let (id, principal_id, policy_version) = (r.get(0)?, r.get(1)?, r.get(5)?);
    Ok((|| {
        Ok(Grant {
            id,
            principal_id,
            connection_ids: from_json(&connection_ids, "connection list")?,
            actions: from_json(&actions, "action list")?,
            constraints: from_json(&constraints, "constraint set")?,
            policy_version,
            created_at: timestamp(created_at)?,
            expires_at: expires_at.map(timestamp).transpose()?,
            revoked_at: revoked_at.map(timestamp).transpose()?,
        })
    })())
}

fn audit_row(r: &Row<'_>) -> Decoded<AuditRecord> {
    let (id, at): (i64, i64) = (r.get(0)?, r.get(1)?);
    let event = AuditEvent {
        kind: r.get(2)?,
        principal_id: r.get(3)?,
        connection_id: r.get(4)?,
        grant_id: r.get(5)?,
        action: r.get(6)?,
        decision: r.get(7)?,
        reason: r.get(8)?,
        policy_version: r.get(9)?,
        detail: r.get(10)?,
    };
    Ok(timestamp(at).map(|at| AuditRecord { id, at, event }))
}

fn timestamp(seconds: i64) -> Result<Timestamp, StoreError> {
    clock::from_unix(seconds).ok_or(StoreError::Corrupt("timestamp"))
}

fn to_json<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).expect("stored types serialize to JSON")
}

fn from_json<T: DeserializeOwned>(text: &str, what: &'static str) -> Result<T, StoreError> {
    serde_json::from_str(text).map_err(|_| StoreError::Corrupt(what))
}

/// The serde name of a unit enum variant, e.g. `connected`.
pub(crate) fn enum_text<T: Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(s)) => s,
        _ => unreachable!("only unit enum variants are stored as text"),
    }
}

fn enum_from_text<T: DeserializeOwned>(text: &str, what: &'static str) -> Result<T, StoreError> {
    serde_json::from_value(serde_json::Value::String(text.to_owned()))
        .map_err(|_| StoreError::Corrupt(what))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Capability;
    use crate::core::clock::parse_rfc3339;
    use crate::delegation::Constraints;
    use crate::providers::Environment;

    fn at(s: &str) -> Timestamp {
        parse_rfc3339(s).unwrap()
    }

    fn connection() -> Connection {
        Connection {
            id: "conn_lob_1".into(),
            provider: "lob".into(),
            environment: Environment::Test,
            status: ConnectionStatus::Connected,
            capabilities: vec![Capability::PostalSendLetter, Capability::PostalReadStatus],
            credential: CredentialRef::Env("LOB_TEST_API_KEY".into()),
            created_at: at("2026-11-09T12:00:00Z"),
        }
    }

    fn grant(id: &str, principal: &str) -> Grant {
        Grant {
            id: id.into(),
            principal_id: principal.into(),
            connection_ids: vec!["conn_lob_1".into()],
            actions: vec![Capability::PostalSendLetter],
            constraints: Constraints {
                max_cost_per_item: Some(Money::new("USD", 300)),
                destination_countries: Some(vec!["US".into()]),
                ..Constraints::default()
            },
            policy_version: 1,
            created_at: at("2026-11-09T12:00:00Z"),
            expires_at: Some(at("2027-01-01T00:00:00Z")),
            revoked_at: None,
        }
    }

    #[test]
    fn connection_round_trip_and_status_change() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(store.connection("conn_lob_1").unwrap(), None);
        store.insert_connection(&connection()).unwrap();
        assert_eq!(store.connection("conn_lob_1").unwrap(), Some(connection()));
        assert_eq!(store.connections().unwrap(), [connection()]);

        assert!(
            store
                .set_connection_status("conn_lob_1", ConnectionStatus::Disconnected)
                .unwrap()
        );
        let stored = store.connection("conn_lob_1").unwrap().unwrap();
        assert_eq!(stored.status, ConnectionStatus::Disconnected);
        assert!(
            !store
                .set_connection_status("nope", ConnectionStatus::Disconnected)
                .unwrap()
        );
    }

    #[test]
    fn grants_round_trip_filter_by_principal_and_revoke_once() {
        let store = Store::open_in_memory().unwrap();
        store.insert_grant(&grant("grant_a", "assistant")).unwrap();
        store.insert_grant(&grant("grant_b", "other")).unwrap();

        assert_eq!(
            store.grant("grant_a").unwrap(),
            Some(grant("grant_a", "assistant"))
        );
        assert_eq!(store.grants(None).unwrap().len(), 2);
        assert_eq!(
            store.grants(Some("assistant")).unwrap(),
            [grant("grant_a", "assistant")]
        );

        let when = at("2026-11-10T00:00:00Z");
        assert!(store.revoke_grant("grant_a", when).unwrap());
        assert!(!store.revoke_grant("grant_a", when).unwrap());
        assert!(!store.revoke_grant("nope", when).unwrap());
        assert_eq!(
            store.grant("grant_a").unwrap().unwrap().revoked_at,
            Some(when)
        );
    }

    #[test]
    fn usage_sums_this_month_in_one_currency_and_today() {
        let store = Store::open_in_memory().unwrap();
        store.insert_grant(&grant("grant_a", "assistant")).unwrap();
        let usd = |n| Money::new("USD", n);
        let record = |op: &str, cost: Option<&Money>, when: &str| {
            store
                .record_usage("grant_a", op, cost, 1, at(when))
                .unwrap()
        };
        record("op_1", Some(&usd(100)), "2026-10-31T23:59:59Z"); // last month
        record("op_2", Some(&usd(200)), "2026-11-01T00:00:00Z");
        record("op_3", Some(&usd(50)), "2026-11-09T08:00:00Z");
        record(
            "op_4",
            Some(&Money::new("EUR", 999)),
            "2026-11-09T09:00:00Z",
        );
        record("op_5", None, "2026-11-09T10:00:00Z");

        let now = at("2026-11-09T12:00:00Z");
        assert_eq!(
            store.usage("grant_a", Some("USD"), now).unwrap(),
            Usage {
                spent_this_month_minor: 250,
                items_today: 3,
            }
        );
        // Without a budget currency there is no spend to compare.
        let no_budget = store.usage("grant_a", None, now).unwrap();
        assert_eq!(no_budget.spent_this_month_minor, 0);
        assert_eq!(
            store.usage("grant_b", Some("USD"), now).unwrap(),
            Usage::default()
        );
    }

    #[test]
    fn audit_events_come_back_newest_first() {
        let store = Store::open_in_memory().unwrap();
        for kind in ["grant.created", "grant.revoked"] {
            let event = AuditEvent {
                kind: kind.into(),
                grant_id: Some("grant_a".into()),
                ..AuditEvent::default()
            };
            store.audit(&event, at("2026-11-09T12:00:00Z")).unwrap();
        }
        let events = store.audit_events(10).unwrap();
        let kinds: Vec<_> = events.iter().map(|r| r.event.kind.as_str()).collect();
        assert_eq!(kinds, ["grant.revoked", "grant.created"]);
        assert_eq!(store.audit_events(1).unwrap().len(), 1);
    }

    #[test]
    fn reopening_a_file_keeps_data_and_rejects_newer_schemas() {
        let dir = tempfile::tempdir().unwrap();
        Store::open(dir.path())
            .unwrap()
            .insert_connection(&connection())
            .unwrap();
        let reopened = Store::open(dir.path()).unwrap();
        assert_eq!(reopened.connections().unwrap(), [connection()]);

        reopened
            .db()
            .pragma_update(None, "user_version", 99)
            .unwrap();
        drop(reopened);
        assert!(matches!(
            Store::open(dir.path()),
            Err(StoreError::TooNew(99))
        ));
    }
}
