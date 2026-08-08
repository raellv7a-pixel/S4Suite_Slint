use crate::engine::disabled::DisabledManager;
use crate::engine::installer::ConflictReport;
use crate::engine::organizer::DuplicateGroup;
use crate::engine::tray::{TrayImportCandidate, TRAY_WORK_DIR};
use parking_lot::Mutex;
use std::path::{Path, PathBuf};

/// Ação destrutiva do organizador aguardando confirmação do usuário.
///
/// Guardamos a lista já resolvida em vez de recalculá-la depois do "confirmar":
/// entre a análise e a confirmação o disco pode mudar, e o usuário precisa
/// apagar exatamente aquilo que a tela lhe mostrou.
pub enum PendingOrganizerAction {
    RemoveDuplicates(Vec<DuplicateGroup>),
    CleanJunk(Vec<PathBuf>),
}

/// Estado que precisa sobreviver entre callbacks da UI.
///
/// Os callbacks Slint são closures independentes: o que um seleciona, o outro
/// não enxerga. Sem este estado, a fila do instalador existia apenas como
/// texto na tela e o caminho real dos arquivos era perdido no instante em que
/// o diálogo de seleção fechava.
pub struct AppState {
    /// Arquivos que o usuário escolheu para instalar (caminhos reais em disco).
    installer_sources: Mutex<Vec<PathBuf>>,
    /// Análise de conflitos aguardando confirmação no diálogo.
    pending_install: Mutex<Option<ConflictReport>>,

    /// Item selecionado na árvore do organizador.
    selected_mod: Mutex<Option<PathBuf>>,
    /// Remoção aguardando confirmação no diálogo do organizador.
    pending_organizer: Mutex<Option<PendingOrganizerAction>>,

    /// Packages encontrados na pasta de origem do merge.
    merger_inputs: Mutex<Vec<PathBuf>>,
    /// Destino escolhido para as partes unificadas.
    merger_output: Mutex<Option<PathBuf>>,

    /// Zips e arquivos de tray escolhidos para importar.
    tray_sources: Mutex<Vec<PathBuf>>,
    /// Fontes já extraídas e analisadas, aguardando a importação.
    tray_candidates: Mutex<Vec<TrayImportCandidate>>,

    disabled_mgr: DisabledManager,
    /// Onde as fontes do tray são extraídas antes de irem para o jogo.
    tray_work_dir: PathBuf,
}

impl AppState {
    pub fn new(config_dir: &Path) -> Self {
        Self {
            installer_sources: Mutex::new(Vec::new()),
            pending_install: Mutex::new(None),
            selected_mod: Mutex::new(None),
            pending_organizer: Mutex::new(None),
            merger_inputs: Mutex::new(Vec::new()),
            merger_output: Mutex::new(None),
            tray_sources: Mutex::new(Vec::new()),
            tray_candidates: Mutex::new(Vec::new()),
            disabled_mgr: DisabledManager::new(config_dir),
            tray_work_dir: config_dir.join(TRAY_WORK_DIR),
        }
    }

    pub fn disabled_manager(&self) -> &DisabledManager {
        &self.disabled_mgr
    }

    // --- Instalador ---

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

    // --- Organizador ---

    pub fn set_selected_mod(&self, path: Option<PathBuf>) {
        *self.selected_mod.lock() = path;
    }

    pub fn selected_mod(&self) -> Option<PathBuf> {
        self.selected_mod.lock().clone()
    }

    pub fn set_pending_organizer(&self, action: PendingOrganizerAction) {
        *self.pending_organizer.lock() = Some(action);
    }

    pub fn take_pending_organizer(&self) -> Option<PendingOrganizerAction> {
        self.pending_organizer.lock().take()
    }

    // --- Merger ---

    pub fn set_merger_inputs(&self, files: Vec<PathBuf>) {
        *self.merger_inputs.lock() = files;
    }

    pub fn merger_inputs(&self) -> Vec<PathBuf> {
        self.merger_inputs.lock().clone()
    }

    pub fn set_merger_output(&self, dir: Option<PathBuf>) {
        *self.merger_output.lock() = dir;
    }

    pub fn merger_output(&self) -> Option<PathBuf> {
        self.merger_output.lock().clone()
    }

    pub fn clear_merger(&self) {
        self.merger_inputs.lock().clear();
        *self.merger_output.lock() = None;
    }

    // --- Tray ---

    pub fn tray_work_dir(&self) -> &Path {
        &self.tray_work_dir
    }

    pub fn set_tray_sources(&self, files: Vec<PathBuf>) {
        *self.tray_sources.lock() = files;
    }

    pub fn tray_sources(&self) -> Vec<PathBuf> {
        self.tray_sources.lock().clone()
    }

    pub fn set_tray_candidates(&self, candidates: Vec<TrayImportCandidate>) {
        *self.tray_candidates.lock() = candidates;
    }

    /// Consome as fontes analisadas. Devolve vazio se a importação já rodou,
    /// o que protege contra duplo-clique em "Importar".
    pub fn take_tray_candidates(&self) -> Vec<TrayImportCandidate> {
        std::mem::take(&mut *self.tray_candidates.lock())
    }

    pub fn clear_tray(&self) {
        self.tray_sources.lock().clear();
        self.tray_candidates.lock().clear();
    }
}
