use s4suite::bridge::slint_adapter::setup_app_adapter;
use s4suite::bridge::state::AppState;
use s4suite::core::config::ConfigManager;
use s4suite::MainWindow;
use slint::ComponentHandle;
use std::sync::Arc;
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let config_mgr = Arc::new(ConfigManager::new()?);

    // Primeira execução numa máquina que já roda a Hydra Shell: o app nasce seguindo o tema do
    // desktop, em vez de impor o verde e obrigar a passar pelas Configurações.
    //
    // A checagem mora aqui, e não em `Config::default()`, para o valor padrão continuar sendo uma
    // constante — do contrário os testes passariam a depender do que existe em ~/.config da
    // máquina que os roda.
    if !config_mgr.config_file().exists() && s4suite::core::theme::system_available() {
        let mut cfg = config_mgr.load();
        cfg.theme = s4suite::core::theme::SYSTEM_NAME.to_string();
        let _ = config_mgr.save(&cfg);
    }

    let state = Arc::new(AppState::new(config_mgr.config_dir()));
    let main_window = MainWindow::new()?;

    setup_app_adapter(&main_window, Arc::clone(&config_mgr), Arc::clone(&state));

    main_window.run()?;
    Ok(())
}
