use std::fmt::{self, Display};

use lum_event::event_repeater::{AttachError, DetachError};
use lum_libs::uuid::Uuid;
use thiserror::Error;

#[derive(Debug, Clone)]
pub enum Status {
    Starting,
    Started,
    Stopping,
    Stopped,
    FailedToStart(String),
    FailedToStop(String),
    Failing,
    RuntimeError(String),
}

impl Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Status::Starting => write!(f, "Starting"),
            Status::Started => write!(f, "Started"),
            Status::Stopping => write!(f, "Stopping"),
            Status::Stopped => write!(f, "Stopped"),
            Status::FailedToStart(error) => write!(f, "Failed to start: {}", error),
            Status::FailedToStop(error) => write!(f, "Failed to stop: {}", error),
            Status::Failing => write!(f, "Failing"),
            Status::RuntimeError(error) => write!(f, "Runtime error: {}", error),
        }
    }
}

impl PartialEq for Status {
    fn eq(&self, other: &Self) -> bool {
        matches!(
            (self, other),
            (Status::Starting, Status::Starting)
                | (Status::Started, Status::Started)
                | (Status::Stopping, Status::Stopping)
                | (Status::Stopped, Status::Stopped)
                | (Status::FailedToStart(_), Status::FailedToStart(_))
                | (Status::FailedToStop(_), Status::FailedToStop(_))
                | (Status::Failing, Status::Failing)
                | (Status::RuntimeError(_), Status::RuntimeError(_))
        )
    }
}

impl Eq for Status {}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Clone, Copy)]
pub enum Health {
    Healthy,
    Unhealthy,
}

impl Display for Health {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Health::Healthy => write!(f, "Healthy"),
            Health::Unhealthy => write!(f, "Unhealthy"),
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

    //TODO: BackgroundTaskRunning(String, Uuid, int32): Service {0} ({1}) has {2} background tasks running
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
    #[error("Service {0} ({1}) is not started or currently starting")]
    ServiceNotStarted(String, Uuid),

    #[error("Service {0} ({1}) is not managed by this Service Manager")]
    ServiceNotManaged(String, Uuid),
}
