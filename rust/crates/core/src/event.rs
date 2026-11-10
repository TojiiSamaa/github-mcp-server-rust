//! Événement d'activité GitHub normalisé + bus partagé (webhook, poller, règles, relevé).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::broadcast;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Event {
    /// Identifiant stable (delivery id du webhook, id de notification…) pour dédoublonner.
    pub id: String,
    pub account: String,
    /// `owner/repo`, vide si sans objet (invitation d'organisation…).
    pub repo: String,
    /// workflow_run, pull_request, issue_comment, invitation, notification, security_alert, package, release, deployment_status…
    pub kind: String,
    pub action: String,
    pub actor: String,
    pub title: String,
    pub url: String,
    pub at: DateTime<Utc>,
    pub payload: Value,
}

pub type Bus = broadcast::Sender<Event>;

/// Un bus par processus : créé par le serveur, cloné partout.
pub fn bus() -> Bus {
    broadcast::channel(1024).0
}
