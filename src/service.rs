use std::{cmp::Ordering, sync::Weak};

use lum_boxtypes::{BoxedError, PinnedBoxedFuture};
use lum_event::Observable;
use lum_libs::{
    async_trait::async_trait,
    downcast_rs::{DowncastSync, impl_downcast},
    uuid::Uuid,
};

use super::{
    service_manager::ServiceManager,
    types::{Priority, Status},
};

#[derive(Debug)]
pub struct ServiceInfo {
    pub uuid: Uuid,
    pub name: String,
    pub priority: Priority,

    pub status: Observable<Status>,
}

impl ServiceInfo {
    pub fn new(name: impl Into<String>, priority: Priority) -> Self {
        let uuid = Uuid::new_v4();
        Self {
            uuid,
            name: name.into(),
            priority,
            status: Observable::new(Status::Stopped, format!("{uuid}::status_change")),
        }
    }
}

impl PartialEq for ServiceInfo {
    fn eq(&self, other: &Self) -> bool {
        self.uuid == other.uuid
    }
}

impl Eq for ServiceInfo {}

impl Ord for ServiceInfo {
    fn cmp(&self, other: &Self) -> Ordering {
        self.name.cmp(&other.name)
    }
}

impl PartialOrd for ServiceInfo {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

//TODO: When async fn is allowed in public traits, use dynosaur here instead of async_trait
#[async_trait]
pub trait Service: DowncastSync {
    fn info(&self) -> &ServiceInfo;
    fn info_mut(&mut self) -> &mut ServiceInfo;

    async fn start(&mut self, service_manager: Weak<ServiceManager>) -> Result<(), BoxedError>;
    async fn stop(&mut self) -> Result<(), BoxedError>;

    //Can't rely on async_trait here, as it returns a non-Sync Future.
    fn fail(&mut self, _message: &str) -> PinnedBoxedFuture<()> {
        Box::pin(async move {})
    }

    fn is_available(&self) -> bool {
        self.info().status.get() == Status::Started
    }
}

impl_downcast!(sync Service);

impl Eq for dyn Service {}

impl PartialEq for dyn Service {
    fn eq(&self, other: &Self) -> bool {
        self.info() == other.info()
    }
}

impl Ord for dyn Service {
    fn cmp(&self, other: &Self) -> Ordering {
        self.info().cmp(other.info())
    }
}

impl PartialOrd for dyn Service {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
