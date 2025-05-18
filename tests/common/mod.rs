use std::{sync::Arc, time::Duration};

use lum_boxtypes::{BoxedError, PinnedBoxedFuture};
use lum_event::Event;
use lum_libs::{
    async_trait::async_trait,
    tokio::{sync::Mutex, time::sleep},
};
use lum_log::info;
use lum_service::{
    service::{Service, ServiceInfo},
    service_manager::ServiceManager,
    types::Priority,
};

pub struct DummyService {
    pub on_start: Event<()>,
    pub on_stop: Event<()>,
    //pub on_task_started: Event<()>,
    //pub on_failed: Event<String>,
    info: ServiceInfo,
}

impl DummyService {
    pub fn new() -> Self {
        let name = "dummy-service";
        let dummy_service = Self {
            on_start: Event::new(format!("{}::on_start", name)),
            on_stop: Event::new(format!("{}::on_stop", name)),
            //on_task_started: Event::new(format!("{}::on_task_started", name)),
            //on_failed: Event::new(format!("{}::on_failed", name)),
            info: ServiceInfo::new(name, "DummyService", Priority::Essential),
        };

        info!("DummyService created");
        dummy_service
    }
}

#[async_trait]
impl Service for DummyService {
    fn info(&self) -> &ServiceInfo {
        &self.info
    }

    fn info_mut(&mut self) -> &mut ServiceInfo {
        &mut self.info
    }

    async fn start(&mut self, service_manager: Arc<ServiceManager>) -> Result<(), BoxedError> {
        info!("Starting DummyService");

        info!("Dispatching on_start event");
        if let Err(error) = self.on_start.dispatch(()).await {
            let message = error
                .iter()
                .map(|error| error.to_string())
                .collect::<Vec<_>>()
                .join(", ");

            return Err(message.into());
        }

        info!("Running task");
        service_manager
            .run_task(
                &self.info,
                Box::pin(async move {
                    loop {
                        info!("Task running...");
                        sleep(Duration::from_secs(3)).await;
                    }
                }),
            )
            .await?;

        Ok(())
    }

    async fn stop(&mut self) -> Result<(), BoxedError> {
        info!("Stopping DummyService");

        info!("Dispatching on_stop event");
        if let Err(error) = self.on_stop.dispatch(()).await {
            let message = error
                .iter()
                .map(|error| error.to_string())
                .collect::<Vec<_>>()
                .join(", ");

            return Err(message.into());
        }

        info!("DummyService stopped");
        Ok(())
    }

    fn fail(&mut self, message: &str) -> PinnedBoxedFuture<()> {
        info!("DummyService failed: {}", message);

        Box::pin(async move {})
    }
}

pub async fn service_manager_with_dummy_service() -> Arc<ServiceManager> {
    let services: Vec<Arc<Mutex<dyn Service>>> = vec![Arc::new(Mutex::new(DummyService::new()))];

    ServiceManager::new(services).await
}
