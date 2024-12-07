use lum_boxtypes::BoxedError;
use lum_event::EventRepeater;
use lum_libs::tokio::{
    spawn,
    sync::{Mutex, MutexGuard},
    task::JoinHandle,
    time::timeout,
};
use lum_log::{error, info};

use crate::taskchain::Taskchain;

use super::{
    service::Service,
    types::{OverallStatus, Priority, ShutdownError, StartupError, Status},
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
    background_tasks: Mutex<HashMap<String, JoinHandle<Result<(), BoxedError>>>>,
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

    pub async fn manages_service(&self, service_id: &str) -> bool {
        for service in self.services.iter() {
            let service_lock = service.lock().await;

            if service_lock.info().id == service_id {
                return true;
            }
        }

        false
    }

    pub async fn start_service(
        &self,
        service: Arc<Mutex<dyn Service>>,
    ) -> Result<(), StartupError> {
        let mut service_lock = service.lock().await;

        let service_id = &service_lock.info().id;
        if !self.manages_service(service_id).await {
            return Err(StartupError::ServiceNotManaged(service_id.clone()));
        }

        let status = service_lock.info().status.get();
        if !matches!(status, Status::Stopped) {
            return Err(StartupError::ServiceNotStopped(service_id.clone()));
        }

        if self.has_background_task_registered(service_id).await {
            return Err(StartupError::BackgroundTaskAlreadyRunning(
                service_id.clone(),
            ));
        }

        let service_status_event = &service_lock.info().status.on_change;
        let attachment_result = self.on_status_change.attach(service_status_event, 2).await;
        if let Err(err) = attachment_result {
            return Err(StartupError::StatusAttachmentFailed(
                service_id.clone(),
                err,
            ));
        }

        service_lock.info_mut().status.set(Status::Starting).await;
        self.init_service(&mut service_lock).await?;
        self.start_background_task(&service_lock, Arc::clone(&service))
            .await;

        info!("Started service {}", service_lock.info().name);

        Ok(())
    }

    //TODO: Clean up
    pub async fn stop_service(
        &self,
        service: Arc<Mutex<dyn Service>>,
    ) -> Result<(), ShutdownError> {
        let service_id = service.lock().await.info().id.clone();
        if !(self.manages_service(&service_id).await) {
            return Err(ShutdownError::ServiceNotManaged(service_id.clone()));
        }

        let mut service_lock = service.lock().await;

        let status = service_lock.info().status.get();
        if !matches!(status, Status::Started) {
            return Err(ShutdownError::ServiceNotStarted(service_id.clone()));
        }

        self.stop_background_task(&service_lock).await;

        service_lock.info_mut().status.set(Status::Stopping).await;

        self.shutdown_service(&mut service_lock).await?;

        let service_status_event = service_lock.info().status.as_ref();
        let detach_result = self.on_status_change.detach(service_status_event).await;
        if let Err(err) = detach_result {
            return Err(ShutdownError::StatusDetachmentFailed(
                service_id.clone(),
                err,
            ));
        }

        info!("Stopped service {}", service_lock.info().name);

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
    pub async fn get_service<T>(&self) -> Option<Arc<Mutex<T>>>
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

    //TODO: When Rust allows async closures, refactor this to use iterator methods instead of for loop
    pub async fn overall_status(&self) -> OverallStatus {
        for service in self.services.iter() {
            let service = service.lock().await;

            if service.info().priority != Priority::Essential {
                continue;
            }

            let status = service.info().status.get();
            if status != Status::Started {
                return OverallStatus::Unhealthy;
            }
        }

        OverallStatus::Healthy
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
        let weak = match self.weak.get() {
            Some(weak) => weak,
            None => {
                error!("ServiceManager's Weak self-reference was None while initializing service {}. This should never happen. Did you not use a ServiceManagerBuilder? Shutting down ungracefully to prevent further undefined behavior.", service.info().name);
                unreachable!(
                    "ServiceManager's Weak self-reference was None while initializing service {}.",
                    service.info().name
                );
            }
        };

        // This can't fail because the Arc is guaranteed to be valid as long as &self is valid.
        let arc = match weak.upgrade() {
            Some(arc) => arc,
            None => {
                error!("ServiceManager's Weak self-reference could not be upgraded to Arc while initializing service {}. This should never happen. Shutting down ungracefully to prevent further undefined behavior.", service.info().name);
                unreachable!("ServiceManager's Weak self-reference could not be upgraded to Arc while initializing service {}.", service.info().name);
            }
        };

        //TODO: Add to config instead of hardcoding duration
        let start = service.start(arc);
        let timeout_result = timeout(Duration::from_secs(10), start).await;

        match timeout_result {
            Ok(start_result) => match start_result {
                Ok(()) => {
                    service.info_mut().status.set(Status::Started).await;
                }
                Err(error) => {
                    service
                        .info_mut()
                        .status
                        .set(Status::FailedToStart(error.to_string()))
                        .await;
                    return Err(StartupError::FailedToStartService(
                        service.info().id.clone(),
                    ));
                }
            },
            Err(error) => {
                service
                    .info_mut()
                    .status
                    .set(Status::FailedToStart(error.to_string()))
                    .await;
                return Err(StartupError::FailedToStartService(
                    service.info().id.clone(),
                ));
            }
        }

        Ok(())
    }

    async fn shutdown_service(
        &self,
        service: &mut MutexGuard<'_, dyn Service>,
    ) -> Result<(), ShutdownError> {
        //TODO: Add to config instead of hardcoding duration
        let stop = service.stop();
        let timeout_result = timeout(Duration::from_secs(10), stop).await;

        match timeout_result {
            Ok(stop_result) => match stop_result {
                Ok(()) => {
                    service.info_mut().status.set(Status::Stopped).await;
                }
                Err(error) => {
                    service
                        .info_mut()
                        .status
                        .set(Status::FailedToStop(error.to_string()))
                        .await;
                    return Err(ShutdownError::FailedToStopService(
                        service.info().id.clone(),
                    ));
                }
            },
            Err(error) => {
                service
                    .info_mut()
                    .status
                    .set(Status::FailedToStop(error.to_string()))
                    .await;
                return Err(ShutdownError::FailedToStopService(
                    service.info().id.clone(),
                ));
            }
        }

        Ok(())
    }

    async fn has_background_task_registered(&self, service_id: &str) -> bool {
        let tasks = self.background_tasks.lock().await;
        tasks.contains_key(service_id)
    }

    async fn start_background_task(
        &self,
        service_lock: &MutexGuard<'_, dyn Service>,
        service: Arc<Mutex<dyn Service>>,
    ) {
        if self
            .has_background_task_registered(&service_lock.info().id)
            .await
        {
            return;
        }

        let task = service_lock.task();
        if let Some(task) = task {
            let mut taskchain = Taskchain::new(task);

            taskchain.append(|result| async move {
                let mut service = service.lock().await;

                match result {
                    Ok(()) => {
                        error!(
                            "Background task of service {} ended unexpectedly! Service will be marked as failed.",
                            service.info().name
                        );

                        service
                            .info_mut()
                            .status
                            .set(Status::RuntimeError("Background task ended unexpectedly!".to_string()))
                            .await;
                    }

                    Err(error) => {
                        error!(
                            "Background task of service {} ended with error: {}. Service will be marked as failed.",
                            service.info().name,
                            error
                        );

                        service
                            .info_mut()
                            .status
                            .set(Status::RuntimeError(
                                format!("Background task ended with error: {}", error),
                            ))
                            .await;
                    }
                }
                Ok(())
            });

            let join_handle = spawn(taskchain.run());

            self.background_tasks
                .lock()
                .await
                .insert(service_lock.info().id.clone(), join_handle);
        }
    }

    async fn stop_background_task(&self, service_lock: &MutexGuard<'_, dyn Service>) {
        if !self
            .has_background_task_registered(&service_lock.info().id)
            .await
        {
            return;
        }

        let mut tasks_lock = self.background_tasks.lock().await;
        let task = tasks_lock.get(&service_lock.info().id).unwrap();
        task.abort();
        tasks_lock.remove(&service_lock.info().id);
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
            write!(f, "{} ({})", service.info().name, service.info().id)?;
            if services.peek().is_some() {
                write!(f, ", ")?;
            }
        }
        Ok(())
    }
}
