//! What Trackside remembers about each listener between sessions: the horses they follow,
//! their home state, and the day they last heard their stable report (so the next report can
//! open with what happened since).
//!
//! On Lambda each request can land on a fresh instance, so the profile lives in DynamoDB
//! (`TRACKSIDE_MEMORY_TABLE`, one item per signed-in user keyed by the token's subject).
//! Without a table it is held in memory, which is how the server runs locally and in tests.
//! A profile holds no racing data and no personal details beyond the subject; `forget_me`
//! deletes it.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use aws_sdk_dynamodb::types::AttributeValue;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    /// Followed horses, as named in the form guide, in the order they were added.
    pub horses: Vec<String>,
    /// VIC, NSW, ...: meetings in this state are read first.
    pub home_state: Option<String>,
    /// The last day (Melbourne time) the listener heard their stable report.
    pub last_checked: Option<NaiveDate>,
}

#[async_trait]
pub trait Memory: Send + Sync {
    /// The listener's profile, or an empty one for someone new.
    async fn load(&self, user: &str) -> Result<Profile>;
    async fn save(&self, user: &str, profile: &Profile) -> Result<()>;
    async fn forget(&self, user: &str) -> Result<()>;
    /// Whether profiles outlive this process, for answers that promise to remember.
    fn durable(&self) -> bool;
}

/// Profiles held in this process: lost on restart, shared by nothing else.
#[derive(Default)]
pub struct InMemory(RwLock<HashMap<String, Profile>>);

#[async_trait]
impl Memory for InMemory {
    async fn load(&self, user: &str) -> Result<Profile> {
        Ok(self.0.read().await.get(user).cloned().unwrap_or_default())
    }
    async fn save(&self, user: &str, profile: &Profile) -> Result<()> {
        self.0
            .write()
            .await
            .insert(user.to_string(), profile.clone());
        Ok(())
    }
    async fn forget(&self, user: &str) -> Result<()> {
        self.0.write().await.remove(user);
        Ok(())
    }
    fn durable(&self) -> bool {
        false
    }
}

/// Profiles in a DynamoDB table with string partition key `user`.
pub struct Dynamo {
    client: aws_sdk_dynamodb::Client,
    table: String,
}

impl Dynamo {
    pub async fn new(table: String) -> Self {
        let config = aws_config::load_from_env().await;
        Self {
            client: aws_sdk_dynamodb::Client::new(&config),
            table,
        }
    }
}

fn key(user: &str) -> HashMap<String, AttributeValue> {
    HashMap::from([("user".to_string(), AttributeValue::S(user.to_string()))])
}

#[async_trait]
impl Memory for Dynamo {
    async fn load(&self, user: &str) -> Result<Profile> {
        let out = self
            .client
            .get_item()
            .table_name(&self.table)
            .set_key(Some(key(user)))
            .consistent_read(true)
            .send()
            .await
            .context("reading the listener's profile")?;
        let Some(item) = out.item else {
            return Ok(Profile::default());
        };
        let s = |name: &str| item.get(name).and_then(|v| v.as_s().ok()).cloned();
        Ok(Profile {
            horses: item
                .get("horses")
                .and_then(|v| v.as_l().ok())
                .map(|l| l.iter().filter_map(|h| h.as_s().ok().cloned()).collect())
                .unwrap_or_default(),
            home_state: s("home_state"),
            last_checked: s("last_checked").and_then(|d| d.parse().ok()),
        })
    }

    async fn save(&self, user: &str, profile: &Profile) -> Result<()> {
        let mut item = key(user);
        item.insert(
            "horses".into(),
            AttributeValue::L(
                profile
                    .horses
                    .iter()
                    .map(|h| AttributeValue::S(h.clone()))
                    .collect(),
            ),
        );
        if let Some(state) = &profile.home_state {
            item.insert("home_state".into(), AttributeValue::S(state.clone()));
        }
        if let Some(day) = profile.last_checked {
            item.insert("last_checked".into(), AttributeValue::S(day.to_string()));
        }
        item.insert(
            "updated_at".into(),
            AttributeValue::S(chrono::Utc::now().to_rfc3339()),
        );
        self.client
            .put_item()
            .table_name(&self.table)
            .set_item(Some(item))
            .send()
            .await
            .context("saving the listener's profile")?;
        Ok(())
    }

    async fn forget(&self, user: &str) -> Result<()> {
        self.client
            .delete_item()
            .table_name(&self.table)
            .set_key(Some(key(user)))
            .send()
            .await
            .context("deleting the listener's profile")?;
        Ok(())
    }

    fn durable(&self) -> bool {
        true
    }
}

/// DynamoDB when `TRACKSIDE_MEMORY_TABLE` names a table, otherwise this process's memory.
pub async fn from_env() -> Arc<dyn Memory> {
    match std::env::var("TRACKSIDE_MEMORY_TABLE") {
        Ok(table) if !table.is_empty() => {
            tracing::info!(%table, "listener profiles in DynamoDB");
            Arc::new(Dynamo::new(table).await)
        }
        _ => Arc::new(InMemory::default()),
    }
}
