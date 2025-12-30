use lum_boxtypes::{BoxedError, LifetimedPinnedBoxedFutureResult};
use lum_event::EventRepeater;
use lum_libs::{
    dashmap::DashMap,
    tokio::{
        spawn,
        sync::{Mutex, MutexGuard},
        task::JoinHandle,
        time::timeout,
    },
};
use lum_log::{error, error_panic, error_unreachable, info, warn};

use crate::{service::ServiceInfo, taskchain::Taskchain, types::RunTaskError};

use super::{
    service::Service,
    types::{Health, Priority, ShutdownError, StartupError, Status},
};

use std::{
    any::TypeId,
    collections::HashMap,
    fmt::{self, Display},
    future::Future,
    sync::{Arc, OnceLock, Weak},
    time::Duration,
};

pub struct ServiceManager {
    pub services: HashMap<TypeId, Arc<Mutex<dyn Service>>>,
    pub on_status_change: Arc<EventRepeater<Status>>,

    weak: OnceLock<Weak<Self>>,
    background_tasks: DashMap<TypeId, Vec<JoinHandle<Result<(), BoxedError>>>>,
}

impl ServiceManager {
    pub async fn new(services: Vec<Arc<Mutex<dyn Service>>>) -> Arc<Self> {
        let mut services_map: HashMap<TypeId, Arc<Mutex<dyn Service>>> = HashMap::new(); //TODO: Drop type annotation

        //TODO: When Rust allows async closures, refactor this to use iterator methods instead of for loop
        for service in services.into_iter() {
            let service_lock = service.lock().await;
            let service_info = service_lock.info();

            let existing_service = services_map.get(&service_info.type_id);
            if let Some(existing_service) = existing_service {
                let existing_service_lock = existing_service.lock().await;
                let existing_service_info = existing_service_lock.info();

                warn!(
                    "ServiceManager::new() was given service {} ({}), which has the same TypeId as service {} ({}). This is not allowed. The service {} ({}) will be ignored.",
                    service_info.name,
                    service_info.type_name,
                    existing_service_info.name,
                    existing_service_info.type_name,
                    service_info.name,
                    service_info.type_name
                );
                continue;
            }

            services_map.insert(service_info.type_id, service.clone());
        }

        let service_manager = ServiceManager {
            weak: OnceLock::new(),
            services: services_map,
            background_tasks: DashMap::new(),
            on_status_change: Arc::new(EventRepeater::new("ServiceManager::on_status_change")),
        };

        let arc = Arc::new(service_manager);
        let weak = Arc::downgrade(&Arc::clone(&arc));

        let result = arc.weak.set(weak);
        if result.is_err() {
            error_unreachable!(
                "Failed to set ServiceManager's Weak self-reference because it was already set. This should never happen. Panicking to prevent further undefined behavior."
            );
        }

        arc
    }

    pub fn get_weak(&self) -> Weak<Self> {
        match self.weak.get() {
            Some(weak) => weak.clone(),
            None => {
                error_panic!(
                    "ServiceManager's Weak self-reference was None when trying to access it. This should never happen. Panicking to prevent further undefined behavior."
                );
            }
        }
    }

    pub async fn start_service(
        &self,
        service: Arc<Mutex<dyn Service>>,
    ) -> Result<(), StartupError> {
        let mut service_lock = service.lock().await;

        let service_info = service_lock.info();
        if !self.manages_service_by_type_id(&service_info.type_id) {
            return Err(StartupError::ServiceNotManaged(
                service_info.name.to_string(),
                service_info.type_name.to_string(),
            ));
        }

        if service_info.status.get() != Status::Stopped {
            return Err(StartupError::ServiceNotStopped(
                service_info.name.to_string(),
                service_info.type_name.to_string(),
            ));
        }

        if self.has_background_tasks_by_type_id(&service_info.type_id) {
            return Err(StartupError::BackgroundTaskAlreadyRunning(
                service_info.name.to_string(),
                service_info.type_name.to_string(),
            ));
        }

        let service_status_event = service_info.status.on_change.clone();
        let attachment_result = self.on_status_change.attach(service_status_event, 10, true);
        if let Err(err) = attachment_result {
            return Err(StartupError::StatusAttachmentFailed(
                service_info.name.to_string(),
                service_info.type_name.to_string(),
                err,
            ));
        }

        self.init_service(&mut service_lock).await?;
        info!("Started service {}", service_lock.info().name); // Reacquiring to allow above mutable borrow

        Ok(())
    }

    pub async fn stop_service(
        &self,
        service: Arc<Mutex<dyn Service>>,
    ) -> Result<(), ShutdownError> {
        let mut service_lock = service.lock().await;

        let service_info = service_lock.info();
        if !(self.manages_service_by_type_id(&service_info.type_id)) {
            return Err(ShutdownError::ServiceNotManaged(
                service_info.name.to_string(),
                service_info.type_name.to_string(),
            ));
        }

        if service_info.status.get() != Status::Started {
            return Err(ShutdownError::ServiceNotStarted(
                service_info.name.to_string(),
                service_info.type_name.to_string(),
            ));
        }

        self.shutdown_service(&mut service_lock).await?;

        //TODO: Find better way to handle this
        // Reacquiring to allow above mutable borrow
        let service_info = service_lock.info();

        let service_status_event = service_info.status.on_change.as_ref();
        let detach_result = self.on_status_change.detach(service_status_event);
        if let Err(err) = detach_result {
            return Err(ShutdownError::StatusDetachmentFailed(
                service_info.name.to_string(),
                service_info.type_name.to_string(),
                err,
            ));
        }

        info!("Stopped service {}", service_info.name);

        Ok(())
    }

    pub async fn start_services(&self) -> Vec<Result<(), StartupError>> {
        let mut results = Vec::new();
        for pair in &self.services {
            let service = pair.1.clone();
            let result = self.start_service(service).await;

            results.push(result);
        }

        results
    }

    pub async fn stop_services(&self) -> Vec<Result<(), ShutdownError>> {
        let mut results = Vec::new();
        for pair in &self.services {
            let service = pair.1.clone();
            let result = self.stop_service(service).await;

            results.push(result);
        }

        results
    }

    pub async fn get_service_by_type<T: Service>(&self) -> Option<Arc<Mutex<dyn Service>>> {
        for service in self.services.values() {
            let lock = service.lock().await;
            if lock.downcast_ref::<T>().is_some() {
                return Some(Arc::clone(service));
            }
        }
        None
    }

    pub async fn with_service<T: Service + 'static, R>(
        &self,
        f: impl FnOnce(&mut T) -> R,
    ) -> Option<R> {
        match self.get_service_by_type::<T>().await {
            Some(service) => {
                let mut lock = service.lock().await;
                let service_ref = lock.downcast_mut::<T>().unwrap();

                Some(f(service_ref))
            }
            None => None,
        }
    }

    pub async fn with_service_async<T: Service + 'static, R, F: Future<Output = R>>(
        &self,
        f: impl FnOnce(&mut T) -> F,
    ) -> Option<R> {
        match self.get_service_by_type::<T>().await {
            Some(service) => {
                let mut lock = service.lock().await;
                let service_ref = lock.downcast_mut::<T>().unwrap();

                Some(f(service_ref).await)
            }
            None => None,
        }
    }

    //TODO: ServiceHandle type
    pub fn get_service(&self, type_id: &TypeId) -> Option<Arc<Mutex<dyn Service>>> {
        self.services.get(type_id).map(Arc::clone)
    }

    pub fn manages_service_by_type_id(&self, type_id: &TypeId) -> bool {
        self.get_service(type_id).is_some()
    }

    pub async fn manages_service(&self, service: &Arc<Mutex<dyn Service>>) -> bool {
        let type_id = service.lock().await.info().type_id;
        self.manages_service_by_type_id(&type_id)
    }

    pub fn has_background_tasks_by_type_id(&self, type_id: &TypeId) -> bool {
        self.background_tasks.contains_key(&type_id)
    }

    pub fn has_background_tasks_by_mutex_guard(
        &self,
        service: &MutexGuard<'_, dyn Service>,
    ) -> bool {
        let type_id = service.info().type_id;
        self.has_background_tasks_by_type_id(&type_id)
    }

    pub async fn has_background_tasks(&self, service: &Arc<Mutex<dyn Service>>) -> bool {
        let type_id = service.lock().await.info().type_id;
        self.has_background_tasks_by_type_id(&type_id)
    }

    //TODO: When Rust allows async closures, refactor this to use iterator methods instead of for loop
    pub async fn health(&self) -> Health {
        for pair in self.services.iter() {
            let service = pair.1;
            let service_lock = service.lock().await;
            let service_info = service_lock.info();

            if service_info.priority != Priority::Essential {
                continue;
            }

            let status = service_info.status.get();
            if status != Status::Started {
                return Health::Unhealthy;
            }
        }

        Health::Healthy
    }

    //TODO: Remove?
    //TODO: When Rust allows async closures, refactor this to use iterator methods instead of for loop
    pub async fn status_overview(&self) -> String {
        let mut text_buffer = String::new();

        let mut failed_essentials = Vec::new();
        let mut failed_optionals = Vec::new();
        let mut non_failed_essentials = Vec::new();
        let mut non_failed_optionals = Vec::new();
        let mut others = Vec::new();

        for pair in self.services.iter() {
            let service = pair.1;
            let lock = service.lock().await;
            let info = lock.info();
            let status = info.status.get();
            let priority = info.priority;
            let name = info.name.as_str();

            match status {
                Status::Started | Status::Stopped => match priority {
                    Priority::Essential => {
                        non_failed_essentials.push(format!(" - {name}: {status}"));
                    }
                    Priority::Optional => {
                        non_failed_optionals.push(format!(" - {name}: {status}"));
                    }
                },
                Status::FailedToStart(_) | Status::FailedToStop(_) | Status::RuntimeError(_) => {
                    match priority {
                        Priority::Essential => {
                            failed_essentials.push(format!(" - {name}: {status}"));
                        }
                        Priority::Optional => {
                            failed_optionals.push(format!(" - {name}: {status}"));
                        }
                    }
                }
                _ => {
                    others.push(format!(" - {name}: {status}"));
                }
            }
        }

        if !failed_essentials.is_empty() {
            text_buffer.push_str("Failed essential services:\n");
            text_buffer.push_str(failed_essentials.join("\n").as_str());
        }

        if !failed_optionals.is_empty() {
            text_buffer.push_str("Failed optional services:\n");
            text_buffer.push_str(failed_optionals.join("\n").as_str());
        }

        if !non_failed_essentials.is_empty() {
            text_buffer.push_str("Essential services:\n");
            text_buffer.push_str(non_failed_essentials.join("\n").as_str());
        }

        if !non_failed_optionals.is_empty() {
            text_buffer.push_str("Optional services:\n");
            text_buffer.push_str(non_failed_optionals.join("\n").as_str());
        }

        if !others.is_empty() {
            text_buffer.push_str("Other services:\n");
            text_buffer.push_str(others.join("\n").as_str());
        }

        let longest_width = text_buffer
            .lines()
            .map(|line| line.len())
            .max()
            .unwrap_or(0);

        let mut headline = String::from("Status overview\n");
        headline.push_str("─".repeat(longest_width).as_str());
        headline.push('\n');
        text_buffer.insert_str(0, &headline);

        text_buffer
    }

    async fn init_service(
        &self,
        service: &mut MutexGuard<'_, dyn Service>,
    ) -> Result<(), StartupError> {
        let service_manager = self.get_weak();

        service.info_mut().status.set(Status::Starting).await;
        let start = service.start(service_manager);
        let timeout_result = timeout(Duration::from_secs(10), start).await; //TODO: Add to config instead of hardcoding duration

        //TODO: Merge all cases into enum with variants "Ok", "Err", and "Timeout"
        let service_info = service.info_mut();
        match timeout_result {
            Ok(start_result) => match start_result {
                Ok(()) => {
                    service_info.status.set(Status::Started).await;
                }
                Err(error) => {
                    service_info
                        .status
                        .set(Status::FailedToStart(error.to_string()))
                        .await;

                    return Err(StartupError::FailedToStartService(
                        service_info.name.clone(),
                        service_info.type_name.to_string(),
                    ));
                }
            },
            Err(error) => {
                service_info
                    .status
                    .set(Status::FailedToStart(error.to_string()))
                    .await;

                return Err(StartupError::FailedToStartService(
                    service_info.name.clone(),
                    service_info.type_name.to_string(),
                ));
            }
        }

        Ok(())
    }

    async fn shutdown_service(
        &self,
        service: &mut MutexGuard<'_, dyn Service>,
    ) -> Result<(), ShutdownError> {
        service.info_mut().status.set(Status::Stopping).await;
        self.abort_background_tasks(service).await;
        let stop = service.stop();
        let timeout_result = timeout(Duration::from_secs(10), stop).await; //TODO: Add to config instead of hardcoding duration

        //TODO: Merge all cases into enum with variants "Ok", "Err", and "Timeout"
        let service_info = service.info_mut();
        match timeout_result {
            Ok(stop_result) => match stop_result {
                Ok(()) => {
                    service_info.status.set(Status::Stopped).await;
                }
                Err(error) => {
                    service_info
                        .status
                        .set(Status::FailedToStop(error.to_string()))
                        .await;

                    return Err(ShutdownError::FailedToStopService(
                        service_info.name.clone(),
                        service_info.type_name.to_string(),
                    ));
                }
            },
            Err(error) => {
                service_info
                    .status
                    .set(Status::FailedToStop(error.to_string()))
                    .await;

                return Err(ShutdownError::FailedToStopService(
                    service_info.name.clone(),
                    service_info.type_name.to_string(),
                ));
            }
        }

        Ok(())
    }

    async fn fail_service<IntoString: Into<String>>(
        &self,
        service: Arc<Mutex<dyn Service>>,
        message: IntoString,
    ) {
        let mut service_lock = service.lock().await;
        self.fail_service_by_mutex_guard(&mut service_lock, message)
            .await;
    }

    async fn fail_service_by_mutex_guard<IntoString: Into<String>>(
        &self,
        service: &mut MutexGuard<'_, dyn Service>,
        message: IntoString,
    ) {
        service.info_mut().status.set(Status::Failing).await;
        self.abort_background_tasks(service).await;

        let message = message.into();
        service.fail(&message).await;
        service
            .info_mut()
            .status
            .set(Status::RuntimeError(message))
            .await;
    }

    pub async fn run_task(
        &self,
        service_info: &ServiceInfo,
        task: LifetimedPinnedBoxedFutureResult<'static, ()>,
    ) -> Result<(), RunTaskError> {
        // We're cloning these values to move them into the task's closure
        // Otherwise, we would reference service_info and get lifetime issues
        let service_name = service_info.name.to_string();
        let service_type_id = service_info.type_id;
        let service_type_name = service_info.type_name; // this is static, so no need to clone
        let service_status = service_info.status.get();

        if service_status != Status::Starting && service_status != Status::Started {
            return Err(RunTaskError::ServiceNotStarted(
                service_name.to_string(),
                service_type_name.to_string(),
            ));
        }

        let service_manager_weak = self.get_weak();
        let mut taskchain = Taskchain::new(task);
        //TODO: When Rust allows async closures, refactor this to have the "async" keyword after the "move" keyword
        taskchain.append(move |result| async move {
            let service_manager_weak = service_manager_weak;
            let service_manager = match service_manager_weak.upgrade() {
                Some(arc) => arc,
                None => {
                    error_panic!(
                        "A task of a service {service_name} ({service_type_name}) unexpectedly ended, but cannot mark service as failed because its corresponding ServiceManager was already dropped. Panicking to prevent further undefined behavior."
                    );
                }
            };

            let service = match service_manager.get_service(&service_type_id) {
                Some(service) => service,
                None => {
                    error_panic!(
                        "A task of a service {service_name} ({service_type_name}) unexpectedly ended, but no service with that ID was registered in its corresponding ServiceManager. Was it removed while the task was running? Panicking to prevent further undefined behavior."
                    );
                }
            };

            match result {
                Ok(()) => {
                    error!(
                        "A task of service {service_name} ({service_type_name}) ended unexpectedly! Service will be marked as failed."
                    );

                    service_manager.fail_service(service, "Background task ended unexpectedly!").await;
                }

                Err(error) => {
                    error!(
                        "A task of service {service_name} ({service_type_name}) ended with error: {error}. Service will be marked as failed.",
                    );

                    service_manager.fail_service(service, error.to_string()).await;
                }
            }
            Ok(())
        });

        let join_handle = spawn(taskchain.run());

        self.background_tasks
            .entry(service_info.type_id)
            .or_default()
            .push(join_handle);

        Ok(())
    }

    async fn abort_background_tasks(&self, service_lock: &MutexGuard<'_, dyn Service>) {
        let service_type_id = service_lock.info().type_id;

        if !self.has_background_tasks_by_type_id(&service_type_id) {
            return;
        }

        let tasks = self.background_tasks.get_mut(&service_type_id).unwrap();
        for task in tasks.iter() {
            task.abort();
        }
        self.background_tasks.remove(&service_type_id);
    }
}

impl Display for ServiceManager {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Services: ")?;

        if self.services.is_empty() {
            write!(f, "None")?;
            return Ok(());
        }

        let mut services = self.services.iter().peekable();
        while let Some((_, service)) = services.next() {
            let service = service.blocking_lock();
            let service_info = service.info();

            write!(f, "{}", service_info.name,)?;
            if services.peek().is_some() {
                write!(f, ", ")?;
            }
        }
        Ok(())
    }
}
