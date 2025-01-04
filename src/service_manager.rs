use lum_boxtypes::{BoxedError, LifetimedPinnedBoxedFutureResult};
use lum_event::EventRepeater;
use lum_libs::{
    tokio::{
        spawn,
        sync::{Mutex, MutexGuard},
        task::JoinHandle,
        time::timeout,
    },
    uuid::Uuid,
};
use lum_log::{error, info};

use crate::{service::ServiceInfo, taskchain::Taskchain, types::RunTaskError};

use super::{
    service::Service,
    types::{Health, Priority, ShutdownError, StartupError, Status},
};

use std::{
    collections::HashMap,
    fmt::{self, Display},
    mem,
    sync::{Arc, OnceLock, Weak},
    time::Duration,
};

pub struct ServiceManager {
    pub services: Vec<Arc<Mutex<dyn Service>>>,
    pub on_status_change: Arc<EventRepeater<Status>>,

    weak: OnceLock<Weak<Self>>,
    background_tasks: Mutex<HashMap<Uuid, Vec<JoinHandle<Result<(), BoxedError>>>>>, //TODO: Types
}

impl ServiceManager {
    pub async fn new(services: Vec<Arc<Mutex<dyn Service>>>) -> Arc<Self> {
        let service_manager = ServiceManager {
            weak: OnceLock::new(),
            services,
            background_tasks: Mutex::new(HashMap::new()),
            on_status_change: EventRepeater::new("service_manager_on_status_change").await,
        };

        let arc = Arc::new(service_manager);
        let weak = Arc::downgrade(&Arc::clone(&arc));

        let result = arc.weak.set(weak);
        if result.is_err() {
            error!("Failed to set ServiceManager's Weak self-reference because it was already set. This should never happen. Panicking to prevent further undefined behavior.");
            unreachable!(
                "Failed to set ServiceManager's Weak self-reference because it was already set."
            );
        }

        arc
    }

    pub async fn start_service(
        &self,
        service: Arc<Mutex<dyn Service>>,
    ) -> Result<(), StartupError> {
        let mut service_lock = service.lock().await;

        let service_info = service_lock.info();
        let service_uuid = &service_info.uuid;
        let service_status = &service_info.status;


        if !self.manages_service_by_uuid(service_uuid).await {
            let service_id = service_info.id.clone();
            return Err(StartupError::ServiceNotManaged(service_id, *service_uuid));
        }

        if service_status.get() != Status::Stopped {
            let service_id = service_info.id.clone();
            return Err(StartupError::ServiceNotStopped(service_id, *service_uuid));
        }

        if self.has_background_tasks_by_uuid(service_uuid).await {
            let service_id = service_info.id.clone();
            return Err(StartupError::BackgroundTaskAlreadyRunning(
                service_id,
                *service_uuid,
            ));
        }

        let service_status_event = &service_status.on_change;
        let attachment_result = self.on_status_change.attach(service_status_event, 10).await;
        if let Err(err) = attachment_result {
            let service_id = service_info.id.clone();
            return Err(StartupError::StatusAttachmentFailed(
                service_id,
                *service_uuid,
                err,
            ));
        }

        self.init_service(&mut service_lock).await?;
        info!("Started service {}", service_lock.info().id); //Reacquire references to allow above mutable borrow

        Ok(())
    }

    pub async fn stop_service(
        &self,
        service: Arc<Mutex<dyn Service>>,
    ) -> Result<(), ShutdownError> {
        let mut service_lock = service.lock().await;

        let service_info = service_lock.info();
        let service_uuid = &service_info.uuid;
        let service_status = &service_info.status;

        if !(self.manages_service_by_uuid(service_uuid).await) {
            let service_id = service_info.id.clone();
            return Err(ShutdownError::ServiceNotManaged(service_id, *service_uuid));
        }

        if service_status.get() != Status::Started {
            let service_id = service_info.id.clone();
            return Err(ShutdownError::ServiceNotStarted(service_id, *service_uuid));
        }

        self.shutdown_service(&mut service_lock).await?;

        //Reacquire references to allow above mutable borrow
        let service_info = service_lock.info();
        let service_uuid = &service_info.uuid;
        let service_status = &service_info.status;

        let service_status_event = service_status.as_ref();
        let detach_result = self.on_status_change.detach(service_status_event).await;
        if let Err(err) = detach_result {
            let service_id = service_info.id.clone();
            return Err(ShutdownError::StatusDetachmentFailed(
                service_id,
                *service_uuid,
                err,
            ));
        }

        info!("Stopped service {}", service_info.id);

        Ok(())
    }

    pub async fn start_services(&self) -> Vec<Result<(), StartupError>> {
        let mut results = Vec::new();

        for service in &self.services {
            let service_arc_clone = Arc::clone(service);
            let result = self.start_service(service_arc_clone).await;

            results.push(result);
        }

        results
    }

    pub async fn stop_services(&self) -> Vec<Result<(), ShutdownError>> {
        let mut results = Vec::new();

        for service in &self.services {
            let service_arc_clone = Arc::clone(service);
            let result = self.stop_service(service_arc_clone).await;

            results.push(result);
        }

        results
    }

    /*
        I tried to do this in safe rust for 3 days, but I couldn't figure it out
        Should you come up with a way to do this in safe rust, please make a PR! :)
        Anyways, this should never cause any issues, since we checked if the service is of type T
    */
    pub async fn get_service_by_type<T>(&self) -> Option<Arc<Mutex<T>>>
    where
        T: Service,
    {
        for service in self.services.iter() {
            let lock = service.lock().await;

            let is_t = lock.as_any().is::<T>();
            if is_t {
                let service_ptr: *const Arc<Mutex<dyn Service>> = service;

                unsafe {
                    let t_ptr: *const Arc<Mutex<T>> = mem::transmute(service_ptr);
                    return Some(Arc::clone(&*t_ptr));
                }
            }
        }

        None
    }

    pub async fn get_service_by_uuid(&self, uuid: &Uuid) -> Option<Arc<Mutex<dyn Service>>> {
        for service in self.services.iter() {
            let service_lock = service.lock().await;

            if service_lock.info().uuid == *uuid {
                return Some(Arc::clone(service));
            }
        }

        None
    }

    pub async fn manages_service_by_uuid(&self, uuid: &Uuid) -> bool {
        self.get_service_by_uuid(uuid).await.is_some()
    }

    pub async fn manages_service(&self, service: &Arc<Mutex<dyn Service>>) -> bool {
        let service_lock = service.lock().await;
        let uuid = service_lock.info().uuid;
        drop(service_lock); //Avoids deadlock because manages_service_by_uuid also locks the service

        self.manages_service_by_uuid(&uuid).await
    }

    pub async fn has_background_tasks_by_uuid(&self, uuid: &Uuid) -> bool {
        let tasks = self.background_tasks.lock().await;
        tasks.contains_key(uuid)
    }

    pub async fn has_background_tasks_by_mutex_guard(
        &self,
        service: &MutexGuard<'_, dyn Service>,
    ) -> bool {
        let uuid = &service.info().uuid;

        self.has_background_tasks_by_uuid(uuid).await
    }

    pub async fn has_background_tasks(&self, service: &Arc<Mutex<dyn Service>>) -> bool {
        let service_lock = service.lock().await;
        let uuid = &service_lock.info().uuid;

        self.has_background_tasks_by_uuid(uuid).await
    }

    //TODO: When Rust allows async closures, refactor this to use iterator methods instead of for loop
    pub async fn health(&self) -> Health {
        for service in self.services.iter() {
            let service = service.lock().await;

            if service.info().priority != Priority::Essential {
                continue;
            }

            let status = service.info().status.get();
            if status != Status::Started {
                return Health::Unhealthy;
            }
        }

        Health::Healthy
    }

    //TODO: When Rust allows async closures, refactor this to use iterator methods instead of for loop
    pub async fn status_overview(&self) -> String {
        let mut text_buffer = String::new();

        let mut failed_essentials = Vec::new();
        let mut failed_optionals = Vec::new();
        let mut non_failed_essentials = Vec::new();
        let mut non_failed_optionals = Vec::new();
        let mut others = Vec::new();

        for service in self.services.iter() {
            let service = service.lock().await;
            let info = service.info();
            let priority = &info.priority;
            let status = info.status.get();

            match status {
                Status::Started | Status::Stopped => match priority {
                    Priority::Essential => {
                        non_failed_essentials.push(format!(" - {}: {}", info.name, status));
                    }
                    Priority::Optional => {
                        non_failed_optionals.push(format!(" - {}: {}", info.name, status));
                    }
                },
                Status::FailedToStart(_) | Status::FailedToStop(_) | Status::RuntimeError(_) => {
                    match priority {
                        Priority::Essential => {
                            failed_essentials.push(format!(" - {}: {}", info.name, status));
                        }
                        Priority::Optional => {
                            failed_optionals.push(format!(" - {}: {}", info.name, status));
                        }
                    }
                }
                _ => {
                    others.push(format!(" - {}: {}", info.name, status));
                }
            }
        }

        if !failed_essentials.is_empty() {
            text_buffer.push_str(&format!("{}:\n", "Failed essential services"));
            text_buffer.push_str(failed_essentials.join("\n").as_str());
        }

        if !failed_optionals.is_empty() {
            text_buffer.push_str(&format!("{}:\n", "Failed optional services"));
            text_buffer.push_str(failed_optionals.join("\n").as_str());
        }

        if !non_failed_essentials.is_empty() {
            text_buffer.push_str(&format!("{}:\n", "Essential services"));
            text_buffer.push_str(non_failed_essentials.join("\n").as_str());
        }

        if !non_failed_optionals.is_empty() {
            text_buffer.push_str(&format!("{}:\n", "Optional services"));
            text_buffer.push_str(non_failed_optionals.join("\n").as_str());
        }

        if !others.is_empty() {
            text_buffer.push_str(&format!("{}:\n", "Other services"));
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
        let service_manager_weak = match self.weak.get() {
            Some(weak) => weak,
            None => {
                let service_uuid = service.info().uuid;
                let service_id = service.info().id.clone();
                error!("ServiceManager's Weak self-reference was None while initializing service {} ({}). This should never happen. Did you not use a ServiceManager::new()? Panicking to prevent further undefined behavior.", service_id, service_uuid);
                panic!(
                    "ServiceManager's Weak self-reference was None while initializing service {} ({}).",
                    service_id, service_uuid
                );
            }
        };

        // This can't fail now because the Arc is guaranteed to be valid as long as &self is valid. We do error handling just in case.
        let service_manager_arc = match service_manager_weak.upgrade() {
            Some(arc) => arc,
            None => {
                let service_uuid = service.info().uuid;
                let service_id = service.info().id.clone();
                error!("ServiceManager's Weak self-reference could not be upgraded to Arc while initializing service {} ({}). This should never happen. Shutting down ungracefully to prevent further undefined behavior.", service_id, service_uuid);
                unreachable!("ServiceManager's Weak self-reference could not be upgraded to Arc while initializing service {} ({}).", service_id, service_uuid);
            }
        };

        service.info_mut().status.set(Status::Starting).await;
        let start = service.start(service_manager_arc);
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
                    let service_id = service_info.id.clone();
                    let service_uuid = service_info.uuid;

                    return Err(StartupError::FailedToStartService(service_id, service_uuid));
                }
            },
            Err(error) => {
                service_info
                    .status
                    .set(Status::FailedToStart(error.to_string()))
                    .await;
                let service_id = service_info.id.clone();
                let service_uuid = service_info.uuid;

                return Err(StartupError::FailedToStartService(service_id, service_uuid));
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
                    let service_id = service_info.id.clone();
                    let service_uuid = service_info.uuid;

                    return Err(ShutdownError::FailedToStopService(service_id, service_uuid));
                }
            },
            Err(error) => {
                service_info
                    .status
                    .set(Status::FailedToStop(error.to_string()))
                    .await;
                let service_id = service_info.id.clone();
                let service_uuid = service_info.uuid;

                return Err(ShutdownError::FailedToStopService(service_id, service_uuid));
            }
        }

        Ok(())
    }

    async fn fail_service<IntoString: Into<String>>(
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
        let service_uuid = service_info.uuid;
        let service_id = service_info.id.clone();
        let service_status = service_info.status.get();

        if service_status != Status::Starting && service_status != Status::Started {
            return Err(RunTaskError::ServiceNotStarted(service_id, service_uuid));
        }

        let service_manager_weak = match self.weak.get() {
            Some(weak) => weak.clone(),
            None => {
                error!("ServiceManager's Weak self-reference was None while running a task for service {} ({}). This should never happen. Did you not use a ServiceManager::new()? Panicking to prevent further undefined behavior.", service_id, service_uuid);
                panic!(
                    "ServiceManager's Weak self-reference was None while running a task for service {} ({}).",
                    service_id, service_uuid
                );
            }
        };

        let service_uuid_clone = service_uuid;
        let mut taskchain = Taskchain::new(task);
        //TODO: When Rust allows async closures, refactor this to have the "async" keyword after the "move" keyword
        taskchain.append(move |result| async move {
            let service_manager_weak = service_manager_weak;
            let service_uuid = service_uuid_clone;
            let service_id = service_id;

            let service_manager_arc = match service_manager_weak.upgrade() {
                Some(arc) => arc,
                None => {
                    error!(
                        "A task of a service {} ({}) unexpectedly ended, but cannot mark service as failed because its corresponding ServiceManager was already dropped. Panicking to prevent further undefined behavior.",
                        service_id, service_uuid);
                    panic!(
                        "A task of a service {} ({}) unexpectedly ended, but cannot mark service as failed because its corresponding ServiceManager was already dropped.",
                        service_id, service_uuid
                    );     
                }
            };

            let service = match service_manager_arc.get_service_by_uuid(&service_uuid).await {
                Some(service) => service,
                None => {
                    error!(
                        "A task of a service {} ({}) unexpectedly ended, but no service with that ID was registered in its corresponding ServiceManager. Was it removed while the task was running? Panicking to prevent further undefined behavior.",
                        service_id, service_uuid
                    );
                    panic!(
                        "A task of a service {} ({}) unexpectedly ended, but no service with that ID was registered in its corresponding ServiceManager. Was it removed while the task was running?",
                        service_id, service_uuid
                    );
                }
            };
            let mut service_lock = service.lock().await;

            match result {
                Ok(()) => {
                    error!(
                        "Background task of service {} ({}) ended unexpectedly! Service will be marked as failed.",
                        service_id, service_uuid
                    );

                    service_manager_arc.fail_service(&mut service_lock, "Background task ended unexpectedly!").await;
                }

                Err(error) => {
                    error!(
                        "Background task of service {} ({}) ended with error: {}. Service will be marked as failed.",
                        service_id, service_uuid,
                        error
                    );

                    service_manager_arc.fail_service(&mut service_lock, error.to_string()).await;
                }
            }
            Ok(())
        });

        let join_handle = spawn(taskchain.run());

        let mut background_tasks = self.background_tasks.lock().await;
        background_tasks
            .entry(service_uuid)
            .or_insert(Vec::new())
            .push(join_handle);

        Ok(())
    }

    async fn abort_background_tasks(&self, service_lock: &MutexGuard<'_, dyn Service>) {
        let service_uuid = &service_lock.info().uuid;

        if !self.has_background_tasks_by_uuid(service_uuid).await {
            return;
        }

        let mut background_tasks = self.background_tasks.lock().await;
        let tasks = background_tasks.get_mut(service_uuid).unwrap();
        for task in tasks.iter() {
            task.abort();
        }
        background_tasks.remove(service_uuid);
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
        while let Some(service) = services.next() {
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
