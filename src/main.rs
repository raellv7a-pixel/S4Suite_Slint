use s4suite::bridge::slint_adapter::setup_app_adapter;
use s4suite::core::config::ConfigManager;
use s4suite::MainWindow;
use slint::ComponentHandle;
use std::sync::Arc;
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let config_mgr = Arc::new(ConfigManager::new()?);
    let main_window = MainWindow::new()?;

    setup_app_adapter(&main_window, Arc::clone(&config_mgr));

    main_window.run()?;
    Ok(())
}
