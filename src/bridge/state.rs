use crate::engine::installer::ConflictReport;
use parking_lot::Mutex;
use std::path::PathBuf;

/// Estado que precisa sobreviver entre callbacks da UI.
///
/// Os callbacks Slint são closures independentes: o que um seleciona, o outro
/// não enxerga. Sem este estado, a fila do instalador existia apenas como
/// texto na tela e o caminho real dos arquivos era perdido no instante em que
/// o diálogo de seleção fechava.
#[derive(Default)]
pub struct AppState {
    /// Arquivos que o usuário escolheu para instalar (caminhos reais em disco).
    pub installer_sources: Mutex<Vec<PathBuf>>,
    /// Análise de conflitos aguardando confirmação no diálogo.
    pub pending_install: Mutex<Option<ConflictReport>>,
}

impl AppState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_installer_sources(&self, files: Vec<PathBuf>) {
        *self.installer_sources.lock() = files;
    }

    pub fn installer_sources(&self) -> Vec<PathBuf> {
        self.installer_sources.lock().clone()
    }

    pub fn clear_installer(&self) {
        self.installer_sources.lock().clear();
        *self.pending_install.lock() = None;
    }

    pub fn set_pending_install(&self, report: ConflictReport) {
        *self.pending_install.lock() = Some(report);
    }

    /// Consome a análise pendente. Devolve `None` se o diálogo já foi resolvido,
    /// o que também protege contra duplo-clique em "Prosseguir".
    pub fn take_pending_install(&self) -> Option<ConflictReport> {
        self.pending_install.lock().take()
    }
}
