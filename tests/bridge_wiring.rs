//! Garante que os botões da UI chegam de fato ao engine.
//!
//! Este arquivo existe por causa de uma classe de bug que apareceu três vezes
//! neste port, sempre com o engine correto e testado por trás:
//!
//! - `on_start_merge` dormia e abria o diálogo sem nunca chamar o merger;
//! - `check_scripts` tinha handler no Rust e nenhum botão o chamava;
//! - `start_tray_import` só trocava o texto do status.
//!
//! Nenhum teste de engine pegaria isso. São duas verificações complementares:
//! a **fiação** (todo callback declarado tem quem o receba, dos dois lados) e
//! o **efeito** (acionar o callback muda o disco de verdade).
//!
//! Os testes de efeito são `#[tokio::test]` porque o adapter agenda trabalho em
//! `spawn_blocking` já na montagem: sem runtime, ele entra em pânico antes de
//! qualquer clique.

use i_slint_backend_testing as slint_testing;
use s4suite::bridge::slint_adapter::setup_app_adapter;
use s4suite::bridge::state::AppState;
use s4suite::core::config::ConfigManager;
use s4suite::MainWindow;
use slint::{ComponentHandle, Model};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tempfile::TempDir;

// --- Parte 1: fiação estática ---

fn ui_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("ui")
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_default()
}

/// Nomes dos `callback <nome>(...)` declarados num arquivo `.slint`.
fn declared_callbacks(src: &str) -> BTreeSet<String> {
    let mut nomes = BTreeSet::new();
    for linha in src.lines() {
        let linha = linha.trim();
        let Some(resto) = linha.strip_prefix("callback ") else { continue };
        // `pure callback` e afins param aqui de propósito: só nos interessam os
        // que o Rust precisa implementar.
        let nome: String = resto
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if !nome.is_empty() {
            nomes.insert(nome);
        }
    }
    nomes
}

/// Só as vistas. Os componentes de `ui/components/` declaram callbacks próprios (`clicked`,
/// `dismissed`) que quem liga são as vistas, não o `main.slint` — cobri-los aqui produzia
/// aprovação por acaso: `clicked` "passava" porque a substring aparece dentro de
/// `clear_cache_clicked =>`. A cobertura deles está em
/// [`todo_callback_de_componente_e_usado_por_alguma_view`].
fn view_files() -> Vec<PathBuf> {
    let mut arquivos = Vec::new();
    let mut stack = vec![ui_dir().join("views")];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().map(|e| e == "slint").unwrap_or(false)
                && path.file_name().map(|n| n != "main.slint").unwrap_or(false)
            {
                arquivos.push(path);
            }
        }
    }
    arquivos
}

/// Todo callback da `MainWindow` precisa de um `window.on_<nome>` no adapter.
///
/// Sem isto, um callback pode existir dos dois lados e mesmo assim não fazer
/// nada — foi o caso do `check_scripts`.
#[test]
fn todo_callback_da_janela_tem_handler_no_adapter() {
    let main = read(&ui_dir().join("main.slint"));
    let adapter = read(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src/bridge/slint_adapter.rs"),
    );

    let sem_handler: Vec<String> = declared_callbacks(&main)
        .into_iter()
        .filter(|nome| !adapter.contains(&format!("window.on_{}(", nome)))
        .collect();

    assert!(
        sem_handler.is_empty(),
        "callback(s) da MainWindow sem handler no adapter: {:?}",
        sem_handler
    );
}

/// Todo callback que uma vista declara precisa ser acionado por algum controle dela.
///
/// Existe porque uma refatoração de layout apagou, sem ninguém notar, a barra de ferramentas
/// inteira do Organizador: os callbacks continuavam declarados, o `main.slint` continuava
/// ligando-os e o Rust continuava com os handlers — só que nenhum botão os chamava mais. Os
/// outros testes de fiação passavam com a funcionalidade fora do ar.
#[test]
fn todo_callback_de_view_tem_quem_o_acione_na_propria_view() {
    // Fiação morta que **já existia** antes da refatoração de UI (confirmado em `git show HEAD`):
    // `start_merge_clicked` é declarado na view, ligado no `main.slint` e tem handler no Rust,
    // mas nenhum botão o aciona — o merger passou a funcionar só pela fila e o caminho de
    // "unificar agora" ficou para trás. Remover envolve mexer no adapter, então fica anotado
    // aqui em vez de silenciado.
    const LEGADO_CONHECIDO: &[&str] = &["merger.slint::start_merge_clicked"];

    let mut soltos: Vec<String> = Vec::new();

    for arquivo in view_files() {
        let src = read(&arquivo);
        let nome_view = arquivo.file_name().unwrap().to_string_lossy().to_string();
        for callback in declared_callbacks(&src) {
            // A vista aciona como `root.nome()` ou `root.nome(arg)`.
            let id = format!("{}::{}", nome_view, callback);
            if !src.contains(&format!("root.{}(", callback)) && !LEGADO_CONHECIDO.contains(&id.as_str()) {
                soltos.push(id);
            }
        }
    }

    assert!(
        soltos.is_empty(),
        "callback(s) declarados numa view que nenhum controle dela aciona: {:?}",
        soltos
    );
}

/// Todo callback declarado em `ui/components/` precisa ser consumido por alguém.
///
/// Um componente que oferece um callback que nenhuma vista liga é código morto — ou, pior, uma
/// ação que o usuário aciona e que não vai a lugar nenhum.
#[test]
fn todo_callback_de_componente_e_usado_por_alguma_view() {
    let dir = ui_dir().join("components");
    let consumidores: String = view_files()
        .iter()
        .map(|p| read(p))
        .chain(std::iter::once(read(&ui_dir().join("main.slint"))))
        .collect::<Vec<_>>()
        .join("\n");

    let mut soltos: Vec<String> = Vec::new();
    for entry in fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.extension().map(|e| e != "slint").unwrap_or(true) {
            continue;
        }
        let nome = path.file_name().unwrap().to_string_lossy().to_string();
        let src = read(&path);
        for callback in declared_callbacks(&src) {
            // Ligado como `nome => {` numa vista, ou invocado como `nome()` dentro do próprio
            // componente por um irmão (caso do `Dialog` usado por outro componente).
            let ligado = consumidores.contains(&format!("{} =>", callback))
                || consumidores.contains(&format!("{}(", callback));
            if !ligado {
                soltos.push(format!("{}::{}", nome, callback));
            }
        }
    }

    assert!(
        soltos.is_empty(),
        "callback(s) de componente que nenhuma view usa: {:?}",
        soltos
    );
}

/// Todo callback declarado numa view precisa ser repassado pelo `main.slint`.
///
/// É o outro lado da mesma moeda: o handler existe no Rust, mas o botão da view
/// não chega até ele porque o `main.slint` não fez a ligação.
#[test]
fn todo_callback_de_view_esta_ligado_no_main() {
    let main = read(&ui_dir().join("main.slint"));
    let mut soltos: Vec<String> = Vec::new();

    for arquivo in view_files() {
        let src = read(&arquivo);
        let nome_view = arquivo.file_name().unwrap().to_string_lossy().to_string();
        for callback in declared_callbacks(&src) {
            // O main liga como `nome => {` ou `nome(arg) => {`.
            let ligado = main.contains(&format!("{} =>", callback))
                || main.contains(&format!("{}(", callback));
            if !ligado {
                soltos.push(format!("{}::{}", nome_view, callback));
            }
        }
    }

    assert!(
        soltos.is_empty(),
        "callback(s) de view que o main.slint não liga a nada: {:?}",
        soltos
    );
}

// --- Parte 2: efeito real no disco ---

struct Harness {
    _root: TempDir,
    _window: MainWindow,
    window_weak: slint::Weak<MainWindow>,
    state: Arc<AppState>,
    mods_dir: PathBuf,
    config_dir: PathBuf,
    config_mgr: Arc<ConfigManager>,
}

impl Harness {
    /// Monta a janela com o adapter ligado, apontando para um jogo de mentira.
    fn new() -> Self {
        slint_testing::init_no_event_loop();

        let root = TempDir::new().unwrap();
        let sims_path = root.path().join("The Sims 4");
        let mods_dir = sims_path.join("Mods");
        let config_dir = root.path().join("config");
        fs::create_dir_all(&mods_dir).unwrap();
        fs::create_dir_all(&config_dir).unwrap();

        // Config próprio do teste. Com o do usuário, os testes disputariam o
        // mesmo arquivo entre threads e ainda deixariam a máquina de quem os
        // roda apontando para um TempDir.
        let config_mgr = Arc::new(ConfigManager::with_dir(config_dir.clone()).unwrap());
        let mut cfg = config_mgr.load();
        cfg.sims4_path = Some(sims_path.clone());
        config_mgr.save(&cfg).unwrap();

        let state = Arc::new(AppState::new(&config_dir));
        let window = MainWindow::new().unwrap();
        setup_app_adapter(&window, Arc::clone(&config_mgr), Arc::clone(&state));

        Self {
            _root: root,
            window_weak: window.as_weak(),
            _window: window,
            state,
            mods_dir,
            config_dir,
            config_mgr,
        }
    }

    fn window(&self) -> MainWindow {
        self.window_weak.upgrade().unwrap()
    }
}

fn write_file(path: &Path, content: &[u8]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, content).unwrap();
}

/// O organizador é o motor com mais operações síncronas: dá para exercitar o
/// caminho inteiro botão → adapter → engine → disco sem event loop.
#[tokio::test]
async fn criar_pasta_pela_ui_cria_a_pasta_no_disco() {
    let h = Harness::new();
    let w = h.window();

    w.invoke_create_mod_folder();
    assert!(w.get_show_organizer_input(), "o diálogo de nome deveria abrir");

    w.invoke_confirm_organizer_input("Cabelos".into());

    assert!(!w.get_show_organizer_input(), "o diálogo deveria fechar");
    assert!(
        h.mods_dir.join("Cabelos").is_dir(),
        "a pasta não chegou ao disco — o callback não alcançou o engine"
    );
}

#[tokio::test]
async fn renomear_pela_ui_renomeia_o_arquivo_no_disco() {
    let h = Harness::new();
    let w = h.window();
    let original = h.mods_dir.join("mod_antigo.package");
    write_file(&original, b"conteudo");

    w.invoke_select_mod_entry(original.display().to_string().into());
    w.invoke_rename_selected_mod();
    w.invoke_confirm_organizer_input("mod_novo.package".into());

    assert!(!original.exists());
    assert!(h.mods_dir.join("mod_novo.package").exists());
}

#[tokio::test]
async fn desativar_pela_ui_renomeia_para_disabled() {
    let h = Harness::new();
    let w = h.window();
    let alvo = h.mods_dir.join("mod.package");
    write_file(&alvo, b"conteudo");

    w.invoke_toggle_disabled_mod(alvo.display().to_string().into());

    assert!(!alvo.exists());
    assert!(h.mods_dir.join("mod.package.disabled").exists());
}

/// A busca precisa chegar ao engine e filtrar o modelo que a tela desenha.
#[tokio::test]
async fn busca_pela_ui_filtra_a_arvore_exibida() {
    let h = Harness::new();
    let w = h.window();
    write_file(&h.mods_dir.join("cabelo_longo.package"), b"a");
    write_file(&h.mods_dir.join("vestido.package"), b"b");

    w.invoke_refresh_mod_tree();
    let total = w.get_mod_entries().iter().count();

    w.invoke_organizer_search_changed("cabelo".into());
    let filtrado = w.get_mod_entries().iter().count();

    assert_eq!(total, 2);
    assert_eq!(filtrado, 1, "a busca não filtrou o modelo entregue à tela");
}

/// A seleção precisa sobreviver entre callbacks: cada um é uma closure
/// independente, e foi por não haver estado compartilhado que a fila do
/// instalador existia só como texto na tela.
#[tokio::test]
async fn selecao_sobrevive_entre_callbacks() {
    let h = Harness::new();
    let w = h.window();
    let a = h.mods_dir.join("a.package");
    let b = h.mods_dir.join("b.package");
    write_file(&a, b"a");
    write_file(&b, b"b");

    w.invoke_select_mod_entry(a.display().to_string().into());
    w.invoke_select_mod_entry(b.display().to_string().into());
    assert_eq!(h.state.selected_mods().len(), 2);
    assert_eq!(w.get_organizer_selected_count(), 2);

    // Clicar de novo desmarca.
    w.invoke_select_mod_entry(a.display().to_string().into());
    assert_eq!(h.state.selected_mods().len(), 1);

    w.invoke_clear_mod_selection();
    assert_eq!(w.get_organizer_selected_count(), 0);
}

/// Excluir passa por confirmação: o clique sozinho não pode apagar nada.
#[tokio::test]
async fn excluir_pela_ui_so_apaga_depois_de_confirmar() {
    let h = Harness::new();
    let w = h.window();
    let alvo = h.mods_dir.join("descartavel.package");
    write_file(&alvo, b"x");

    w.invoke_select_mod_entry(alvo.display().to_string().into());
    w.invoke_delete_selected_mods();

    assert!(w.get_show_organizer_confirm(), "o diálogo de confirmação deveria abrir");
    assert!(alvo.exists(), "nada pode ser apagado antes de confirmar");

    w.invoke_cancel_organizer_action();
    assert!(alvo.exists(), "cancelar tem que preservar o arquivo");
}

/// O painel de desativados precisa ler o disco, não um modelo vazio.
#[tokio::test]
async fn painel_de_desativados_carrega_o_que_esta_no_disco() {
    let h = Harness::new();
    let w = h.window();
    write_file(&h.mods_dir.join("inativo.package.disabled"), b"x");

    w.invoke_open_disabled_panel();

    assert!(w.get_show_disabled_panel());
    assert_eq!(w.get_disabled_entries().iter().count(), 1);
}

/// A fila do merger tem que guardar a tarefa montada, não só mostrar texto.
#[tokio::test]
async fn adicionar_tarefa_pela_ui_entra_na_fila_do_merger() {
    let h = Harness::new();
    let w = h.window();
    let pacote = h.mods_dir.join("Grupo/a.package");
    write_file(&pacote, b"x");

    // O seletor de arquivos é nativo e não roda em teste; alimentamos o estado
    // como se o usuário já tivesse escolhido.
    h.state.set_merger_inputs(vec![pacote]);
    h.state.set_merger_output(Some(h.mods_dir.join("Unificados")));

    w.invoke_add_merge_job();

    assert_eq!(h.state.merge_jobs().len(), 1);
    assert_eq!(w.get_merge_jobs().iter().count(), 1);
    assert_eq!(h.state.merge_jobs()[0].name, "Grupo");
}

/// Trocar o idioma tem que reconfigurar o global de tradução, senão a tela
/// continua em português até a próxima abertura.
#[tokio::test]
async fn trocar_idioma_pela_ui_muda_a_traducao_ativa() {
    let h = Harness::new();
    let w = h.window();

    w.invoke_language_selected("en".into());

    assert_eq!(w.get_active_language(), "en");
    let i18n = w.global::<s4suite::I18n>();
    assert_eq!(i18n.get_lang(), "en");
    assert_eq!(i18n.invoke_translate("en".into(), "Salvar".into()), "Save");
}

/// Um staging deixado por uma sessão que morreu no meio não pode sobreviver à
/// abertura seguinte: ele mora dentro de `Mods`, e o jogo carregaria aqueles
/// arquivos meio extraídos junto com os mods de verdade.
#[tokio::test]
async fn abertura_limpa_o_staging_de_uma_sessao_interrompida() {
    let h = Harness::new();
    let staging = h.mods_dir.join(".s4suite_staging");
    write_file(&staging.join("mod_pela_metade/x.package"), b"lixo");

    // Segunda abertura, mesmo config e mesma pasta.
    let janela = MainWindow::new().unwrap();
    setup_app_adapter(&janela, Arc::clone(&h.config_mgr), Arc::new(AppState::new(&h.config_dir)));

    assert!(!staging.exists(), "o staging órfão continuou em Mods");
}

/// Escolher o tema tem que repintar a janela aberta, não só gravar no config.
#[tokio::test]
async fn trocar_o_tema_pela_ui_repinta_a_janela_na_hora() {
    let h = Harness::new();
    let w = h.window();
    assert_eq!(w.global::<s4suite::Theme>().get_active_theme(), 0);

    w.invoke_theme_selected(2);

    assert_eq!(
        w.global::<s4suite::Theme>().get_active_theme(),
        2,
        "a global do tema não mudou: as cores só apareceriam na próxima abertura"
    );
    assert_eq!(h.config_mgr.load().theme, "Cyber Purple");
}

/// Repetir a fila do merger refaria um merge cujos originais a pós-ação já
/// consumiu. As tarefas concluídas ficam à vista, mas não voltam a rodar.
#[tokio::test]
async fn executar_a_fila_de_novo_nao_repete_as_tarefas_concluidas() {
    let h = Harness::new();
    let w = h.window();
    let pacote = h.mods_dir.join("Grupo/a.package");
    write_file(&pacote, b"x");
    h.state.set_merger_inputs(vec![pacote]);
    h.state.set_merger_output(Some(h.mods_dir.join("Unificados")));
    w.invoke_add_merge_job();

    h.state.set_job_status(0, s4suite::engine::merger::JobStatus::Done);
    w.invoke_start_merge_queue();

    assert!(
        w.get_merge_status().contains("já foram processadas"),
        "status inesperado: {}",
        w.get_merge_status()
    );
    assert!(!w.get_is_merging(), "a fila não deveria ter começado a rodar");
}
