//! Exercita efeitos observáveis de callbacks com jogo e configuração temporários.
//!
//! As verificações textuais de fiação não provavam execução nem autorização e
//! mantinham uma exceção para o caminho órfão do merger. A cobertura abaixo
//! verifica seleção, confirmação, cancelamento e efeitos reais no disco.
//! `tokio::test` fornece o runtime usado pelos workers do adapter.

use i_slint_backend_testing as slint_testing;
use s4suite::bridge::slint_adapter::setup_app_adapter;
use s4suite::bridge::state::AppState;
use s4suite::core::config::ConfigManager;
use s4suite::MainWindow;
use slint::{ComponentHandle, Model};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tempfile::TempDir;


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

#[tokio::test]
async fn variantes_pela_ui_preservam_grupos_e_resumo_da_selecao() {
    use s4suite::engine::installer::{calculate_conflicts, STAGING_DIR_NAME};

    let h = Harness::new();
    let w = h.window();
    let staging = h.mods_dir.join(STAGING_DIR_NAME);
    write_file(&staging.join("A/Options/keep.package"), b"XML Injector");
    write_file(&staging.join("A/Options/discard.package"), b"Lot51");
    write_file(&staging.join("B/Options/one.package"), b"one");
    write_file(&staging.join("B/Options/two.package"), b"two");
    write_file(&staging.join("A/common.package"), b"common");
    let report = calculate_conflicts(&staging, &h.mods_dir).unwrap();
    h.state
        .set_pending_install(report, "\n\nFonte recusada: unsafe.zip".to_string());
    w.set_is_installing(true);

    w.invoke_exclusive_mode_selected("one".into());
    w.invoke_toggle_exclusive_option("A/Options/keep.package".into());
    w.invoke_proceed_install_clicked();
    assert!(!h.mods_dir.join("00_Triagem_Novos").exists());
    assert!(!w.get_show_careful_scan_dialog());
    w.invoke_confirm_exclusive_selection();
    assert!(w.get_show_exclusive_dialog());
    assert_eq!(w.get_exclusive_selected_count(), 2);
    assert!(w
        .get_exclusive_options()
        .iter()
        .all(|option| option.path.starts_with("B/")));

    w.invoke_exclusive_mode_selected("manual".into());
    w.invoke_toggle_exclusive_option("B/Options/one.package".into());
    w.invoke_toggle_exclusive_option("B/Options/two.package".into());
    w.invoke_confirm_exclusive_selection();
    assert!(w.get_show_exclusive_dialog());
    assert!(!w.get_show_careful_scan_dialog());
    w.invoke_exclusive_mode_selected("all".into());
    w.invoke_confirm_exclusive_selection();

    assert!(w.get_show_careful_scan_dialog());
    assert!(!w.get_show_exclusive_dialog());
    let summary = w.get_careful_scan_message();
    assert!(summary.contains("4 mods novos"), "{summary}");
    assert!(summary.contains("XML Injector"));
    assert!(!summary.contains("Lot51"));
    assert!(summary.contains("unsafe.zip"));
    assert_eq!(w.get_installer_queue().row_count(), 4);
    assert!(!h.mods_dir.join("00_Triagem_Novos").exists());
    assert!(staging.join("A/Options/discard.package").exists());
}

#[tokio::test]
async fn exclusao_de_traducao_exige_confirmacao_e_cancelar_preserva_disco() {
    let h = Harness::new();
    let w = h.window();
    let item = h.mods_dir.join("01_Traducoes/Pacote/a.package");
    write_file(&item, b"translation");
    w.invoke_delete_translation("Pacote".into());
    assert!(w.get_show_translation_confirm());
    assert_eq!(w.get_translation_removal_name(), "Pacote");
    assert_eq!(fs::read(&item).unwrap(), b"translation");
    w.invoke_cancel_translation_removal();
    w.invoke_confirm_translation_removal();
    assert!(!w.get_show_translation_confirm());
    assert!(item.exists());
    w.invoke_delete_translation("Pacote".into());
    w.invoke_confirm_translation_removal();
    assert!(!item.parent().unwrap().exists());
    assert!(h.mods_dir.is_dir());
}

#[tokio::test]
async fn traducao_substituida_apos_confirmacao_pendente_nao_e_excluida() {
    let h = Harness::new();
    let w = h.window();
    let item = h.mods_dir.join("01_Traducoes/a.package");
    write_file(&item, b"old");
    w.invoke_delete_translation("a.package".into());
    let replacement = h.mods_dir.join("replacement.package");
    write_file(&replacement, b"new");
    fs::rename(&replacement, &item).unwrap();
    w.invoke_confirm_translation_removal();
    assert_eq!(fs::read(&item).unwrap(), b"new");
    assert!(!w.get_show_translation_confirm());
    assert!(w.get_translations_status().starts_with("❌"));
}

#[tokio::test]
async fn troca_de_jogo_invalida_exclusao_pendente_e_caminho_protegido_e_recusado() {
    let h = Harness::new();
    let w = h.window();
    let item = h.mods_dir.join("01_Traducoes/a.package");
    write_file(&item, b"keep");
    w.invoke_delete_translation("../../Mods".into());
    assert!(!w.get_show_translation_confirm());
    w.invoke_delete_translation("a.package".into());
    let mut cfg = h.config_mgr.load();
    cfg.sims4_path = Some(h._root.path().join("Other Game"));
    fs::create_dir_all(cfg.sims4_path.as_ref().unwrap().join("Mods")).unwrap();
    h.config_mgr.save(&cfg).unwrap();
    w.invoke_confirm_translation_removal();
    assert_eq!(fs::read(&item).unwrap(), b"keep");
    assert!(w.get_translations_status().starts_with("❌"));
}

#[test]
fn manutencao_impede_worker_concorrente_e_libera_apos_falha() {
    let root = TempDir::new().unwrap();
    let state = Arc::new(AppState::new(root.path()));
    let permit = state.try_begin_background_operation().unwrap();
    let other = Arc::clone(&state);
    assert!(
        std::thread::spawn(move || other.try_begin_background_operation().is_none())
            .join()
            .unwrap()
    );
    assert!(std::thread::spawn(move || {
        let _permit = permit;
        panic!("falha sintética do worker");
    })
    .join()
    .is_err());
    assert!(state.try_begin_background_operation().is_some());
}

fn preparar_revisao_delete(h: &Harness, w: &MainWindow) -> Vec<PathBuf> {
    use s4suite::bridge::state::{PendingMergeReview, QueuedJob};
    use s4suite::engine::dbpf::{DBPFWriter, PackageResource, ResourceKey};
    use s4suite::engine::merger::{JobStatus, MergeJob, MergeReview, PostMergeAction};
    let inputs = vec![
        h.mods_dir.join("CC/a.package"),
        h.mods_dir.join("CC/b.package"),
    ];
    let output = h._root.path().join("unificados");
    fs::create_dir_all(&output).unwrap();
    fs::create_dir_all(inputs[0].parent().unwrap()).unwrap();
    for (i, path) in inputs.iter().enumerate() {
        let mut file = fs::File::create(path).unwrap();
        DBPFWriter::write_package(
            &mut file,
            &[PackageResource {
                key: ResourceKey {
                    type_id: 0x034AEECB,
                    group_id: 0,
                    instance_ex: 0,
                    instance_low: i as u32,
                },
                data: vec![i as u8; 32],
                mem_size: 32,
                compressed: 0,
            }],
        )
        .unwrap();
        h.state.push_merge_job(QueuedJob {
            name: "Mesmo nome".into(),
            input_files: vec![path.clone()],
            output_dir: output.clone(),
            post_action: PostMergeAction::Delete,
            status: JobStatus::Pending,
        });
    }
    let queue = h.state.merge_jobs();
    let limit = (h.config_mgr.load().merge_limit_gb * 1_073_741_824.0) as u64;
    let reviews = queue
        .iter()
        .map(|job| {
            MergeReview::prepare(MergeJob {
                name: job.name.clone(),
                input_files: job.input_files.clone(),
                output_dir: job.output_dir.clone(),
                max_size_bytes: limit,
                post_action: job.post_action,
            })
            .unwrap()
        })
        .collect();
    h.state.set_pending_merge_review(PendingMergeReview {
        queue,
        indices: vec![0, 1],
        limit,
        revision: h.state.merger_revision(),
        decision_revision: 1,
        reviews,
        authorized: Vec::new(),
        permit: h.state.try_begin_background_operation().unwrap(),
    });
    w.set_merge_review_revision(1);
    w.set_show_merge_review(true);
    w.set_is_merging(true);
    inputs
}

#[tokio::test]
async fn delete_exige_frase_exata_e_cancelar_preserva_toda_fila() {
    let h = Harness::new();
    let w = h.window();
    let inputs = preparar_revisao_delete(&h, &w);
    for phrase in ["", "excluir originais", "EXCLUIR ORIGINAIS "] {
        w.invoke_confirm_merge_review(1, phrase.into());
        assert!(h
            .state
            .with_pending_merge_review(|pending| pending.authorized.is_empty())
            .unwrap());
        assert!(inputs.iter().all(|p| p.exists()));
    }
    w.invoke_cancel_merge_review();
    w.invoke_confirm_merge_review(1, "EXCLUIR ORIGINAIS".into());
    assert!(inputs.iter().all(|p| p.exists()));
    assert!(!h
        ._root
        .path()
        .join("unificados")
        .read_dir()
        .unwrap()
        .any(|e| e.is_ok()));
    assert!(!w.get_is_merging());
    assert!(h.state.try_begin_background_operation().is_some());
}

#[tokio::test]
async fn confirmacao_de_uma_tarefa_nao_autoriza_outro_job_de_mesmo_nome() {
    let h = Harness::new();
    let w = h.window();
    let inputs = preparar_revisao_delete(&h, &w);
    w.invoke_confirm_merge_review(1, "EXCLUIR ORIGINAIS".into());
    let next = w.get_merge_review_revision();
    assert_ne!(next, 1);
    assert!(w.get_show_merge_review());
    assert!(w.get_merge_review_requires_delete());
    w.invoke_confirm_merge_review(1, "EXCLUIR ORIGINAIS".into());
    assert_eq!(
        h.state
            .with_pending_merge_review(|pending| pending.authorized.len()),
        Some(1)
    );
    assert!(inputs.iter().all(|p| p.exists()));
    w.invoke_cancel_merge_review();
    assert!(inputs.iter().all(|p| p.exists()));
}

fn verificar_consentimento_invalidado(change: usize) {
    let h = Harness::new();
    let w = h.window();
    let inputs = preparar_revisao_delete(&h, &w);
    match change {
        0 => h.state.set_merger_inputs(Vec::new()),
        1 => {
            h.state.toggle_selected_job(1);
            h.state.remove_selected_jobs();
        }
        _ => {
            let mut config = h.config_mgr.load();
            config.merge_limit_gb += 1.0;
            h.config_mgr.save(&config).unwrap();
        }
    }
    w.invoke_confirm_merge_review(1, "EXCLUIR ORIGINAIS".into());
    assert!(inputs.iter().all(|p| p.exists()));
    assert!(!w.get_show_merge_review());
    assert!(!w.get_is_merging());
    assert!(h.state.take_pending_merge_review().is_none());
}

#[tokio::test]
async fn selecao_alterada_invalida_consentimento() {
    verificar_consentimento_invalidado(0);
}

#[tokio::test]
async fn fila_alterada_invalida_consentimento() {
    verificar_consentimento_invalidado(1);
}

#[tokio::test]
async fn limite_alterado_invalida_consentimento() {
    verificar_consentimento_invalidado(2);
}
