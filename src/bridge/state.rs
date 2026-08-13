use crate::engine::disabled::DisabledManager;
use crate::engine::installer::ConflictReport;
use crate::engine::merger::{JobStatus, PostMergeAction};
use crate::engine::organizer::{DuplicateGroup, ScriptDepthIssue, TransferMode};
use crate::engine::tray::{TrayImportCandidate, TRAY_WORK_DIR};
use parking_lot::Mutex;
use std::path::{Path, PathBuf};

/// Ação do organizador que mexe em arquivos do usuário e aguarda confirmação.
///
/// Guardamos a lista já resolvida em vez de recalculá-la depois do "confirmar":
/// entre a análise e a confirmação o disco pode mudar, e o usuário precisa
/// apagar exatamente aquilo que a tela lhe mostrou.
pub enum PendingOrganizerAction {
    RemoveDuplicates(Vec<DuplicateGroup>),
    CleanJunk(Vec<PathBuf>),
    /// Exclusão de itens escolhidos à mão no explorador.
    DeleteSelection(Vec<PathBuf>),
    /// Exclusão definitiva de mods que estavam apenas desativados.
    DeleteDisabled(Vec<PathBuf>),
    /// Realocação de scripts fundos demais para o jogo carregar. Não apaga
    /// nada, mas tira arquivos de onde o autor do mod os pôs — o usuário
    /// precisa ver a lista antes.
    FixScriptDepth(Vec<ScriptDepthIssue>),
}

/// Para que serve o texto que o usuário está digitando no diálogo.
///
/// Um só diálogo atende os dois casos; sem guardar a intenção, o "OK" não teria
/// como saber se cria uma pasta ou renomeia o item.
pub enum PendingInput {
    CreateFolder { parent: PathBuf },
    Rename { target: PathBuf },
}

/// Transferência esperando o usuário escolher a pasta de destino.
pub struct PendingTransfer {
    pub sources: Vec<PathBuf>,
    pub mode: TransferMode,
}

/// Uma tarefa montada na fila do merger, ainda não executada.
///
/// Guarda a lista de arquivos, e não a pasta de origem: entre montar a fila e
/// executá-la o usuário pode trocar a seleção, e a tarefa precisa unificar
/// exatamente o que estava à vista quando foi adicionada.
#[derive(Clone)]
pub struct QueuedJob {
    pub name: String,
    pub input_files: Vec<PathBuf>,
    pub output_dir: PathBuf,
    pub post_action: PostMergeAction,
    pub status: JobStatus,
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

    /// Itens marcados na árvore do organizador. As operações do explorador
    /// agem sobre este conjunto, como no menu de contexto do app PyQt.
    selected_mods: Mutex<Vec<PathBuf>>,
    /// Texto de busca ativo, preservado entre recargas da árvore.
    organizer_search: Mutex<String>,
    /// Remoção aguardando confirmação no diálogo do organizador.
    pending_organizer: Mutex<Option<PendingOrganizerAction>>,
    /// Nome sendo digitado (nova pasta ou renomeação).
    pending_input: Mutex<Option<PendingInput>>,
    /// Mover/copiar aguardando a escolha do destino.
    pending_transfer: Mutex<Option<PendingTransfer>>,
    /// Desativados marcados no painel, para restaurar ou excluir em lote.
    selected_disabled: Mutex<Vec<PathBuf>>,

    /// Packages encontrados na pasta de origem do merge.
    merger_inputs: Mutex<Vec<PathBuf>>,
    /// Destino escolhido para as partes unificadas.
    merger_output: Mutex<Option<PathBuf>>,
    /// Fila de tarefas montada pelo usuário.
    merge_jobs: Mutex<Vec<QueuedJob>>,
    /// Índices marcados na fila, para remoção.
    selected_jobs: Mutex<Vec<usize>>,
    /// Destino dos originais depois de unificar.
    merge_post_action: Mutex<PostMergeAction>,

    /// Zips e arquivos de tray escolhidos para importar.
    tray_sources: Mutex<Vec<PathBuf>>,
    /// Fontes já extraídas e analisadas, aguardando a importação.
    tray_candidates: Mutex<Vec<TrayImportCandidate>>,
    /// Índices marcados na fila do tray.
    selected_tray: Mutex<Vec<usize>>,

    disabled_mgr: DisabledManager,
    /// Onde as fontes do tray são extraídas antes de irem para o jogo.
    tray_work_dir: PathBuf,
}

impl AppState {
    pub fn new(config_dir: &Path) -> Self {
        Self {
            installer_sources: Mutex::new(Vec::new()),
            pending_install: Mutex::new(None),
            selected_mods: Mutex::new(Vec::new()),
            organizer_search: Mutex::new(String::new()),
            pending_organizer: Mutex::new(None),
            pending_input: Mutex::new(None),
            pending_transfer: Mutex::new(None),
            selected_disabled: Mutex::new(Vec::new()),
            merger_inputs: Mutex::new(Vec::new()),
            merger_output: Mutex::new(None),
            merge_jobs: Mutex::new(Vec::new()),
            selected_jobs: Mutex::new(Vec::new()),
            merge_post_action: Mutex::new(PostMergeAction::Keep),
            tray_sources: Mutex::new(Vec::new()),
            tray_candidates: Mutex::new(Vec::new()),
            selected_tray: Mutex::new(Vec::new()),
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

    /// Marca ou desmarca um item da árvore, devolvendo se ele ficou marcado.
    pub fn toggle_selected_mod(&self, path: PathBuf) -> bool {
        let mut selected = self.selected_mods.lock();
        match selected.iter().position(|p| *p == path) {
            Some(idx) => {
                selected.remove(idx);
                false
            }
            None => {
                selected.push(path);
                true
            }
        }
    }

    pub fn selected_mods(&self) -> Vec<PathBuf> {
        self.selected_mods.lock().clone()
    }

    pub fn clear_selected_mods(&self) {
        self.selected_mods.lock().clear();
    }

    /// Descarta da seleção o que não existe mais em disco.
    ///
    /// Depois de mover ou excluir, os caminhos antigos continuariam marcados e
    /// a próxima operação agiria sobre arquivos que já não estão lá.
    pub fn prune_selected_mods(&self) {
        self.selected_mods.lock().retain(|p| p.exists());
    }

    pub fn set_organizer_search(&self, query: String) {
        *self.organizer_search.lock() = query;
    }

    pub fn organizer_search(&self) -> String {
        self.organizer_search.lock().clone()
    }

    pub fn set_pending_organizer(&self, action: PendingOrganizerAction) {
        *self.pending_organizer.lock() = Some(action);
    }

    pub fn take_pending_organizer(&self) -> Option<PendingOrganizerAction> {
        self.pending_organizer.lock().take()
    }

    pub fn set_pending_input(&self, input: PendingInput) {
        *self.pending_input.lock() = Some(input);
    }

    pub fn take_pending_input(&self) -> Option<PendingInput> {
        self.pending_input.lock().take()
    }

    pub fn set_pending_transfer(&self, transfer: PendingTransfer) {
        *self.pending_transfer.lock() = Some(transfer);
    }

    pub fn take_pending_transfer(&self) -> Option<PendingTransfer> {
        self.pending_transfer.lock().take()
    }

    pub fn toggle_selected_disabled(&self, path: PathBuf) -> bool {
        let mut selected = self.selected_disabled.lock();
        match selected.iter().position(|p| *p == path) {
            Some(idx) => {
                selected.remove(idx);
                false
            }
            None => {
                selected.push(path);
                true
            }
        }
    }

    pub fn selected_disabled(&self) -> Vec<PathBuf> {
        self.selected_disabled.lock().clone()
    }

    pub fn clear_selected_disabled(&self) {
        self.selected_disabled.lock().clear();
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

    // --- Fila do merger ---

    pub fn push_merge_job(&self, job: QueuedJob) {
        self.merge_jobs.lock().push(job);
    }

    pub fn merge_jobs(&self) -> Vec<QueuedJob> {
        self.merge_jobs.lock().clone()
    }

    pub fn set_job_status(&self, index: usize, status: JobStatus) {
        if let Some(job) = self.merge_jobs.lock().get_mut(index) {
            job.status = status;
        }
    }

    pub fn toggle_selected_job(&self, index: usize) {
        let mut selected = self.selected_jobs.lock();
        match selected.iter().position(|i| *i == index) {
            Some(pos) => {
                selected.remove(pos);
            }
            None => selected.push(index),
        }
    }

    pub fn selected_jobs(&self) -> Vec<usize> {
        self.selected_jobs.lock().clone()
    }

    /// Remove as tarefas marcadas. Descartamos de trás para frente porque cada
    /// remoção desloca os índices seguintes.
    pub fn remove_selected_jobs(&self) -> usize {
        let mut selected = self.selected_jobs.lock();
        selected.sort_unstable();
        selected.dedup();

        let mut jobs = self.merge_jobs.lock();
        let mut removidos = 0;
        for index in selected.iter().rev() {
            if *index < jobs.len() {
                jobs.remove(*index);
                removidos += 1;
            }
        }
        selected.clear();
        removidos
    }

    pub fn clear_selected_jobs(&self) {
        self.selected_jobs.lock().clear();
    }

    pub fn set_merge_post_action(&self, action: PostMergeAction) {
        *self.merge_post_action.lock() = action;
    }

    pub fn merge_post_action(&self) -> PostMergeAction {
        *self.merge_post_action.lock()
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
        self.selected_tray.lock().clear();
    }

    /// Aplica uma mudança a cada candidato da fila, opcionalmente só aos
    /// marcados. Usado por "destino do CC" e "pular".
    pub fn update_tray_candidates<F>(&self, apenas_marcados: bool, mut f: F)
    where
        F: FnMut(&mut TrayImportCandidate),
    {
        let marcados = self.selected_tray.lock().clone();
        for (index, candidate) in self.tray_candidates.lock().iter_mut().enumerate() {
            if !apenas_marcados || marcados.contains(&index) {
                f(candidate);
            }
        }
    }

    /// Lê a fila sem consumi-la, para redesenhar a tela.
    pub fn with_tray_candidates<R>(&self, f: impl FnOnce(&[TrayImportCandidate]) -> R) -> R {
        f(&self.tray_candidates.lock())
    }

    pub fn toggle_selected_tray(&self, index: usize) {
        let mut selected = self.selected_tray.lock();
        match selected.iter().position(|i| *i == index) {
            Some(pos) => {
                selected.remove(pos);
            }
            None => selected.push(index),
        }
    }

    pub fn selected_tray(&self) -> Vec<usize> {
        self.selected_tray.lock().clone()
    }

    /// Consome as fontes analisadas. Devolve vazio se a importação já rodou,
    /// o que protege contra duplo-clique em "Importar".
    pub fn take_tray_candidates(&self) -> Vec<TrayImportCandidate> {
        std::mem::take(&mut *self.tray_candidates.lock())
    }

    pub fn clear_tray(&self) {
        self.tray_sources.lock().clear();
        self.tray_candidates.lock().clear();
        self.selected_tray.lock().clear();
    }
}
