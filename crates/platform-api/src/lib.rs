#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DataFlow {
    Render,
    Capture,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DeviceRole {
    Console,
    Multimedia,
    Communications,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemLifecycleEvent {
    SessionLocked,
    SessionUnlocked,
    SessionDisconnected,
    SessionConnected,
    SessionLoggedOff,
    Suspending,
    Resumed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EndpointDescriptor {
    pub platform: String,
    pub id: String,
    #[serde(default)]
    pub group_id: Option<String>,
    pub friendly_name: String,
    pub flow: DataFlow,
    pub default_roles: Vec<DeviceRole>,
}

#[derive(Debug, Error)]
pub enum PlatformError {
    #[error("platform operation failed: {0}")]
    Operation(String),
    #[error("platform worker stopped unexpectedly")]
    WorkerStopped,
}

pub trait AudioPlatform: Send + Sync {
    fn active_endpoints(&self) -> Result<Vec<EndpointDescriptor>, PlatformError>;
}
