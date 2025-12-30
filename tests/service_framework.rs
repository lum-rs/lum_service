mod common;

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use lum_libs::tokio::{self, time::sleep};
    use lum_log::info;

    use crate::common::service_manager_with_dummy_service;

    #[tokio::test]
    async fn test() {
        let service_manager = service_manager_with_dummy_service();

        service_manager.start_services().await;

        info!("Forcing an await of 0ms to allow the task to print a message");
        sleep(Duration::from_millis(0)).await;
    }

    //TODO: Add test for stop_services()
    //TODO: Add tests for starting/stopping services multiple times
    //TODO: Add test for ignoring services with same ID
    //TODO: Add test for get_service_by_type()
}
