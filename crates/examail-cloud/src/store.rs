//! What the service keeps, in Firestore. Every document is keyed by the Google account's `sub`
//! (accounts, grants) or by a hash of a secret (devices), so nothing is looked up by email.

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::firestore::{Firestore, Precondition};
use crate::jobs::{self, Job};

const ACCOUNTS: &str = "accounts";
const GRANTS: &str = "grants";
const DEVICES: &str = "devices";
const CONNECTS: &str = "connects";

/// A Google account that connected Examail Cloud.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Account {
    pub email: String,
    /// `active`, or `revoked` once Google refuses its grant.
    pub status: String,
    pub connected_at: String,
}

/// The service's own Google grant for an account: its refresh token, sealed with KMS.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Grant {
    pub sealed_refresh_token: String,
    pub scopes: String,
}

/// A Mac (or other device) of an account's, known by the hash of the token it was given.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Device {
    pub sub: String,
    pub name: String,
    pub created_at: String,
}

/// A consent in progress: who started it and its PKCE verifier, until Google redirects back.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Connect {
    pub sub: String,
    pub verifier: String,
    pub expires_at: String,
}

#[derive(Clone)]
pub struct Store {
    db: Firestore,
}

impl Store {
    pub fn new(db: Firestore) -> Self {
        Self { db }
    }

    /// The database itself, for what's queried rather than looked up ([`crate::jobs`]).
    pub fn db(&self) -> &Firestore {
        &self.db
    }

    pub async fn job(&self, id: &str) -> Result<Option<Job>> {
        self.db.get(jobs::COLLECTION, id).await
    }

    pub async fn put_job(&self, id: &str, job: &Job) -> Result<()> {
        Ok(self.db.set(jobs::COLLECTION, id, job, Precondition::None).await?)
    }

    pub async fn delete_job(&self, id: &str) -> Result<()> {
        self.db.delete(jobs::COLLECTION, id).await
    }

    /// `sub`'s jobs, finished or not.
    pub async fn jobs(&self, sub: &str) -> Result<Vec<Job>> {
        Ok(self
            .db
            .where_eq(jobs::COLLECTION, "sub", &serde_json::Value::from(sub))
            .await?
            .into_iter()
            .map(|(_, job)| job)
            .collect())
    }

    pub async fn account(&self, sub: &str) -> Result<Option<Account>> {
        self.db.get(ACCOUNTS, sub).await
    }

    pub async fn put_account(&self, sub: &str, account: &Account) -> Result<()> {
        Ok(self.db.set(ACCOUNTS, sub, account, Precondition::None).await?)
    }

    pub async fn grant(&self, sub: &str) -> Result<Option<Grant>> {
        self.db.get(GRANTS, sub).await
    }

    pub async fn put_grant(&self, sub: &str, grant: &Grant) -> Result<()> {
        Ok(self.db.set(GRANTS, sub, grant, Precondition::None).await?)
    }

    pub async fn device(&self, token_hash: &str) -> Result<Option<Device>> {
        self.db.get(DEVICES, token_hash).await
    }

    pub async fn put_device(&self, token_hash: &str, device: &Device) -> Result<()> {
        Ok(self.db.set(DEVICES, token_hash, device, Precondition::Missing).await?)
    }

    pub async fn delete_device(&self, token_hash: &str) -> Result<()> {
        self.db.delete(DEVICES, token_hash).await
    }

    pub async fn put_connect(&self, state: &str, connect: &Connect) -> Result<()> {
        Ok(self.db.set(CONNECTS, state, connect, Precondition::Missing).await?)
    }

    /// The consent `state` named, taken so it can only be used once.
    pub async fn take_connect(&self, state: &str) -> Result<Option<Connect>> {
        let connect = self.db.get(CONNECTS, state).await?;
        self.db.delete(CONNECTS, state).await?;
        Ok(connect)
    }

    /// Forgets everything about `sub`: its account, grant, devices and jobs.
    pub async fn forget(&self, sub: &str) -> Result<()> {
        for job in self
            .db
            .ids_where(jobs::COLLECTION, "sub", &serde_json::Value::from(sub))
            .await?
        {
            self.db.delete(jobs::COLLECTION, &job).await?;
        }
        for device in self
            .db
            .ids_where(DEVICES, "sub", &serde_json::Value::from(sub))
            .await?
        {
            self.db.delete(DEVICES, &device).await?;
        }
        self.db.delete(GRANTS, sub).await?;
        self.db.delete(ACCOUNTS, sub).await
    }
}
