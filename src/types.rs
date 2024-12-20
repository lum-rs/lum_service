use std::fmt::{self, Display};

use lum_event::event_repeater::{AttachError, DetachError};
use lum_libs::{thiserror::Error, uuid::Uuid};

#[derive(Debug, Clone)]
pub enum Status {
    Started,
    Stopped,
    Starting,
    Stopping,
    FailedToStart(String),
    FailedToStop(String),
    RuntimeError(String),
}

impl Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Status::Started => write!(f, "Started"),
            Status::Stopped => write!(f, "Stopped"),
            Status::Starting => write!(f, "Starting"),
            Status::Stopping => write!(f, "Stopping"),
            Status::FailedToStart(error) => write!(f, "Failed to start: {}", error),
            Status::FailedToStop(error) => write!(f, "Failed to stop: {}", error),
            Status::RuntimeError(error) => write!(f, "Runtime error: {}", error),
        }
    }
}

impl PartialEq for Status {
    fn eq(&self, other: &Self) -> bool {
        matches!(
            (self, other),
            (Status::Started, Status::Started)
                | (Status::Stopped, Status::Stopped)
                | (Status::Starting, Status::Starting)
                | (Status::Stopping, Status::Stopping)
                | (Status::FailedToStart(_), Status::FailedToStart(_))
                | (Status::FailedToStop(_), Status::FailedToStop(_))
                | (Status::RuntimeError(_), Status::RuntimeError(_))
        )
    }
}

impl Eq for Status {}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Clone, Copy)]
pub enum OverallStatus {
    Healthy,
    Unhealthy,
}

impl Display for OverallStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OverallStatus::Healthy => write!(f, "Healthy"),
            OverallStatus::Unhealthy => write!(f, "Unhealthy"),
        }
    }
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Clone, Copy)]
pub enum Priority {
    Essential,
    Optional,
}

impl Display for Priority {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Priority::Essential => write!(f, "Essential"),
            Priority::Optional => write!(f, "Optional"),
        }
    }
}

#[derive(Debug, Error)]
pub enum StartupError {
    #[error("Service {0} ({1}) is not managed by this Service Manager")]
    ServiceNotManaged(String, Uuid),

    #[error("Service {0} ({1}) is not stopped")]
    ServiceNotStopped(String, Uuid),

    #[error("Service {0} ({1}) already has a background task running")]
    BackgroundTaskAlreadyRunning(String, Uuid),

    #[error(
        "Failed to attach Service Manager's status_change EventRepeater to {0} ({1})'s status_change Event: {2}"
    )]
    StatusAttachmentFailed(String, Uuid, AttachError),

    #[error("Service {0} ({1}) failed to start")]
    FailedToStartService(String, Uuid),
}

#[derive(Debug, Error)]
pub enum ShutdownError {
    #[error("Service {0} ({1}) is not managed by this Service Manager")]
    ServiceNotManaged(String, Uuid),

    #[error("Service {0} ({1}) is not started")]
    ServiceNotStarted(String, Uuid),

    #[error("Service {0} ({1}) failed to stop")]
    FailedToStopService(String, Uuid),

    #[error(
        "Failed to detach Service Manager's status_change EventRepeater from {0} ({1})'s status_change Event: {2}"
    )]
    StatusDetachmentFailed(String, Uuid, DetachError),
}

#[derive(Debug, Error)]
pub enum RunTaskError {
    #[error("Service {0} ({1}) is not managed by this Service Manager")]
    ServiceNotManaged(String, Uuid),
}
