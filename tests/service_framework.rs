mod common;

#[cfg(test)]
mod tests {
    use lum_libs::tokio;

    use crate::common::service_manager_with_dummy_service;

    #[tokio::test]
    async fn test() {
        let service_manager = service_manager_with_dummy_service().await;

        service_manager.start_services().await;
    }
}
