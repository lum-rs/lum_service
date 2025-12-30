use std::{
    any::{self, TypeId},
    cmp::Ordering,
    sync::Weak,
};

use lum_boxtypes::{BoxedError, PinnedBoxedFuture};
use lum_event::Observable;
use lum_libs::{
    async_trait::async_trait,
    downcast_rs::{DowncastSync, impl_downcast},
};

use super::{
    service_manager::ServiceManager,
    types::{Priority, Status},
};

#[derive(Debug)]
pub struct ServiceInfo {
    pub type_id: TypeId,
    pub type_name: &'static str,
    pub name: String,
    pub priority: Priority,

    pub status: Observable<Status>,
}

impl ServiceInfo {
    pub fn new(service_type: TypeId, name: impl Into<String>, priority: Priority) -> Self {
        let type_id = service_type;
        let type_name = any::type_name_of_val(&type_id);
        let name = name.into();
        let status = Observable::new(Status::Stopped, format!("{type_name}::status_change"));

        Self {
            type_id,
            type_name,
            name,
            priority,
            status,
        }
    }
}

impl PartialEq for ServiceInfo {
    fn eq(&self, other: &Self) -> bool {
        self.type_id == other.type_id
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
