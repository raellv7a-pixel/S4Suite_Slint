//! Sessão simulada de um usuário real contra uma pasta `Mods` de verdade.
//!
//! Diferente de `tests/*_flow.rs`, aqui não há fixture sintética: os arquivos
//! são mods reais copiados da biblioteca do usuário, e a janela é a mesma que
//! `main.rs` monta — mesmo adapter, mesmo event loop, mesmos callbacks. O que
//! não dá para dirigir é o diálogo nativo de arquivos (`rfd`); nesses pontos o
//! estado é alimentado exatamente como o handler faria depois do diálogo, e o
//! resto do fluxo segue pela UI.
//!
//! Uso: `S4_SANDBOX=<dir> cargo run --example simulacao_usuario`

use s4suite::bridge::slint_adapter::setup_app_adapter;
use s4suite::bridge::state::AppState;
use s4suite::core::config::{get_mods_dir, get_tray_dir, ConfigManager};
use s4suite::engine::dbpf::DBPFReader;
use s4suite::engine::translations::{install_translation, translations_dir};
use s4suite::engine::tray::{prepare_tray_candidates, TrayCacheDb};
use s4suite::MainWindow;
use slint::{ComponentHandle, Model};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

// --------------------------------------------------------------------------
// Relatório
// --------------------------------------------------------------------------

struct Relatorio {
    ok: usize,
    falhas: Vec<String>,
    ato: String,
}

impl Relatorio {
    fn new() -> Self {
        Self { ok: 0, falhas: Vec::new(), ato: String::new() }
    }

    fn ato(&mut self, titulo: &str) {
        self.ato = titulo.to_string();
        println!("\n\x1b[1m━━━ {} ━━━\x1b[0m", titulo);
    }

    fn check(&mut self, nome: &str, passou: bool, detalhe: impl AsRef<str>) {
        let d = detalhe.as_ref();
        if passou {
            self.ok += 1;
            println!("  \x1b[32m✔\x1b[0m {}{}", nome, if d.is_empty() { String::new() } else { format!(" — {}", d) });
        } else {
            self.falhas.push(format!("[{}] {} — {}", self.ato, nome, d));
            println!("  \x1b[31m✘\x1b[0m {} — \x1b[31m{}\x1b[0m", nome, d);
        }
    }

    fn nota(&self, texto: &str) {
        println!("  \x1b[36mℹ\x1b[0m {}", texto);
    }
}

// --------------------------------------------------------------------------
// Event loop
// --------------------------------------------------------------------------

fn pump(ms: u64) {
    let timer = slint::Timer::default();
    timer.start(slint::TimerMode::SingleShot, Duration::from_millis(ms), || {
        let _ = slint::quit_event_loop();
    });
    let _ = slint::run_event_loop();
}

/// Espera a condição virar verdadeira, rodando o event loop enquanto isso.
fn esperar(cond: impl Fn() -> bool, limite_ms: u64) -> bool {
    let inicio = Instant::now();
    while inicio.elapsed() < Duration::from_millis(limite_ms) {
        if cond() {
            pump(60); // deixa a última closure da UI terminar
            return true;
        }
        pump(60);
    }
    cond()
}

// --------------------------------------------------------------------------
// Fixtures: os "downloads" do usuário, montados a partir de mods reais
// --------------------------------------------------------------------------

fn packages_reais(mods: &Path, n: usize, min: u64) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = walkdir::WalkDir::new(mods)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path().is_file()
                && e.path().extension().map(|x| x.eq_ignore_ascii_case("package")).unwrap_or(false)
                && e.metadata().map(|m| m.len() > min).unwrap_or(false)
        })
        .map(|e| e.path().to_path_buf())
        .collect();
    v.sort();
    v.into_iter().take(n).collect()
}

fn zip_com(destino: &Path, entradas: &[(&str, &Path)], extra_texto: &[(&str, &[u8])]) {
    let f = fs::File::create(destino).unwrap();
    let mut z = zip::ZipWriter::new(f);
    let opts: zip::write::FileOptions<()> =
        zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for (nome, origem) in entradas {
        z.start_file(*nome, opts).unwrap();
        z.write_all(&fs::read(origem).unwrap()).unwrap();
    }
    for (nome, conteudo) in extra_texto {
        z.start_file(*nome, opts).unwrap();
        z.write_all(conteudo).unwrap();
    }
    z.finish().unwrap();
}

// --------------------------------------------------------------------------

fn conta(dir: &Path, ext: &str) -> usize {
    walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path().is_file()
                && e.path().extension().map(|x| x.eq_ignore_ascii_case(ext)).unwrap_or(false)
        })
        .count()
}

/// Chaves TGI distintas de um conjunto de packages.
///
/// O merger sobrescreve recursos de mesma chave — dois packages que trazem o
/// mesmo TGI viram um só na saída, como o jogo faria de qualquer jeito. Comparar
/// com a soma bruta acusaria perda onde só houve deduplicação.
fn chaves_distintas(paths: &[PathBuf]) -> usize {
    let mut chaves = std::collections::HashSet::new();
    for p in paths {
        let Ok(f) = fs::File::open(p) else { continue };
        let mut r = std::io::BufReader::new(f);
        if let Ok((_, idx)) = DBPFReader::read_index(&mut r) {
            for e in idx {
                chaves.insert(e.key);
            }
        }
    }
    chaves.len()
}

fn recursos_do_package(p: &Path) -> usize {
    let Ok(f) = fs::File::open(p) else { return 0 };
    let mut r = std::io::BufReader::new(f);
    DBPFReader::read_index(&mut r).map(|(_, idx)| idx.len()).unwrap_or(0)
}

#[tokio::main]
async fn main() {
    i_slint_backend_testing::init_integration_test_with_system_time();

    let sandbox = PathBuf::from(std::env::var("S4_SANDBOX").expect("S4_SANDBOX"));
    let sims_path = sandbox.join("The Sims 4");
    let mods = get_mods_dir(&sims_path);
    let tray = get_tray_dir(&sims_path);
    let downloads = sandbox.join("downloads");
    let config_dir = sandbox.join("config");
    fs::create_dir_all(&downloads).unwrap();
    fs::create_dir_all(&config_dir).unwrap();

    let mut r = Relatorio::new();

    println!("\x1b[1m\x1b[35mS4Suite — sessão simulada de usuário\x1b[0m");
    println!("Mods: {}", mods.display());
    println!(
        "Estado inicial: {} packages, {} scripts, {} arquivos no Tray",
        conta(&mods, "package"),
        conta(&mods, "ts4script"),
        fs::read_dir(&tray).map(|d| d.count()).unwrap_or(0)
    );

    // ---------------------------------------------------------------- Ato 0
    r.ato("Ato 0 — Primeira abertura e configuração");

    // Primeira abertura de verdade: sem config de sessão anterior.
    let _ = fs::remove_file(config_dir.join("config.json"));
    let cfg = Arc::new(ConfigManager::with_dir(config_dir.clone()).unwrap());
    let state = Arc::new(AppState::new(cfg.config_dir()));
    let w = MainWindow::new().unwrap();
    setup_app_adapter(&w, Arc::clone(&cfg), Arc::clone(&state));
    pump(200);

    // Sem config, `load()` já sai procurando a instalação — na máquina real,
    // um prefixo Heroic.
    let detectado = cfg.load().sims4_path.clone();
    r.check(
        "primeira abertura já encontra a instalação real do usuário",
        detectado.as_ref().map(|p| p.join("Mods").is_dir()).unwrap_or(false),
        format!("{:?}", detectado),
    );
    r.check(
        "o painel abre apontando para a pasta detectada",
        w.get_game_path() != "Não configurado",
        w.get_game_path().to_string(),
    );

    w.invoke_autodetect_game_path();
    pump(200);
    r.check(
        "botão de autodetectar confirma o mesmo caminho",
        cfg.load().sims4_path == detectado,
        w.get_config_status().to_string(),
    );

    // O usuário aponta a pasta manualmente (o rfd::pick_folder não roda aqui).
    let mut c = cfg.load();
    c.sims4_path = Some(sims_path.clone());
    cfg.save(&c).unwrap();
    w.invoke_refresh_stats();
    pump(400);

    r.check(
        "config gravado sobrevive a um reload",
        cfg.load().sims4_path.as_deref() == Some(sims_path.as_path()),
        "",
    );

    // ---------------------------------------------------------------- Ato 1
    r.ato("Ato 1 — Painel");

    w.invoke_select_tab(0);
    esperar(|| w.get_total_mods_count() != "0", 20_000);

    let pk_disco = conta(&mods, "package");
    let sc_disco = conta(&mods, "ts4script");
    r.check(
        "contagem de packages bate com o disco",
        w.get_total_mods_count().to_string() == pk_disco.to_string(),
        format!("painel {} vs disco {}", w.get_total_mods_count(), pk_disco),
    );
    r.check(
        "contagem de scripts bate com o disco",
        w.get_total_scripts_count().to_string() == sc_disco.to_string(),
        format!("painel {} vs disco {}", w.get_total_scripts_count(), sc_disco),
    );
    let tray_disco = fs::read_dir(&tray).map(|d| d.count()).unwrap_or(0);
    r.check(
        "contagem do Tray bate com o disco",
        w.get_total_tray_count().to_string() == tray_disco.to_string(),
        format!("painel {} vs disco {}", w.get_total_tray_count(), tray_disco),
    );
    r.nota(&format!("tamanho total exibido: {}", w.get_total_mods_size()));

    let detalhes: Vec<String> = w.get_folder_size_details().iter().map(|s| s.to_string()).collect();
    r.check(
        "card de tamanho lista as pastas, maior primeiro",
        detalhes.len() >= 3,
        format!("{} linhas; topo: {}", detalhes.len(), detalhes.first().cloned().unwrap_or_default()),
    );
    r.check(
        "card de scripts lista os .ts4script",
        w.get_scripts_details().iter().count() > 0,
        format!("{} linhas", w.get_scripts_details().iter().count()),
    );
    r.check(
        "card de tray lista os arquivos",
        w.get_tray_details().iter().count() > 0,
        format!("{} linhas", w.get_tray_details().iter().count()),
    );

    // Cache do jogo: o app apaga localthumbcache/avatarcache do diretório do jogo.
    fs::write(sims_path.join("localthumbcache.package"), b"cache").unwrap();
    w.invoke_clear_cache();
    esperar(|| w.get_dashboard_status().contains("cache"), 5_000);
    r.check(
        "limpar cache apaga o localthumbcache",
        !sims_path.join("localthumbcache.package").exists(),
        w.get_dashboard_status().to_string(),
    );

    // ---------------------------------------------------------------- Ato 2
    r.ato("Ato 2 — Instalador");

    let amostra = packages_reais(&mods, 12, 40_000);
    assert!(amostra.len() >= 6, "sandbox sem packages suficientes");

    // (a) mod novo em zip
    let zip_v1 = downloads.join("Cabelos Lindos da Raell v1.zip");
    zip_com(
        &zip_v1,
        &[
            ("Cabelos Lindos/S4R_CabeloLongo.package", &amostra[0]),
            ("Cabelos Lindos/S4R_CabeloCurto.package", &amostra[1]),
        ],
        &[("Cabelos Lindos/LEIA-ME.txt", b"instrucoes")],
    );

    state.set_installer_sources(vec![zip_v1.clone()]);
    w.invoke_start_install();
    let abriu = esperar(|| w.get_show_careful_scan_dialog(), 30_000);
    r.check("análise do zip abre o diálogo de confirmação", abriu, w.get_installer_status().to_string());
    r.nota(&w.get_careful_scan_message().to_string().replace('\n', " | "));
    r.check(
        "fila mostra os 2 packages (e ignora o .txt)",
        w.get_installer_queue().iter().count() == 2,
        format!("{} itens", w.get_installer_queue().iter().count()),
    );

    w.invoke_proceed_install_clicked();
    esperar(|| w.get_installer_status().contains("concluída") || w.get_installer_status().contains("Falha"), 30_000);
    let destino_a = mods.join("00_Triagem_Novos/Cabelos Lindos da Raell v1/Cabelos Lindos/S4R_CabeloLongo.package");
    r.check("mod novo aterrissa em 00_Triagem_Novos", destino_a.exists(), w.get_installer_status().to_string());
    r.check(
        "staging não fica para trás",
        !mods.join(".s4suite_staging").exists(),
        "",
    );

    // (b) reinstalar o mesmo arquivo
    state.set_installer_sources(vec![zip_v1.clone()]);
    w.invoke_start_install();
    esperar(|| w.get_show_careful_scan_dialog(), 30_000);
    let msg_b = w.get_careful_scan_message().to_string();
    r.check(
        "reinstalar o mesmo zip é reportado como já instalado",
        msg_b.contains("2 já instalados"),
        msg_b.replace('\n', " | "),
    );
    let mtime_antes = fs::metadata(&destino_a).unwrap().modified().unwrap();
    w.invoke_proceed_install_clicked();
    esperar(|| w.get_installer_status().contains("concluída"), 30_000);
    r.check(
        "arquivo idêntico não é recopiado",
        fs::metadata(&destino_a).unwrap().modified().unwrap() == mtime_antes,
        w.get_installer_status().to_string(),
    );

    // (c) versão nova do mesmo mod
    let v2_a = downloads.join("_v2a.package");
    let v2_b = downloads.join("_v2b.package");
    let mut bytes = fs::read(&amostra[0]).unwrap();
    bytes.extend_from_slice(&[0u8; 4096]);
    fs::write(&v2_a, &bytes).unwrap();
    let mut bytes2 = fs::read(&amostra[1]).unwrap();
    bytes2.extend_from_slice(&[0u8; 8192]);
    fs::write(&v2_b, &bytes2).unwrap();
    let zip_v2 = downloads.join("Cabelos Lindos da Raell v2.zip");
    zip_com(
        &zip_v2,
        &[
            ("Cabelos Lindos/S4R_CabeloLongo.package", &v2_a),
            ("Cabelos Lindos/S4R_CabeloCurto.package", &v2_b),
        ],
        &[],
    );

    state.set_installer_sources(vec![zip_v2.clone()]);
    w.invoke_start_install();
    esperar(|| w.get_show_careful_scan_dialog(), 30_000);
    let msg_c = w.get_careful_scan_message().to_string();
    r.check(
        "versão diferente é detectada como atualização",
        msg_c.contains("2 atualizações"),
        msg_c.replace('\n', " | "),
    );
    w.invoke_proceed_install_clicked();
    esperar(|| w.get_installer_status().contains("concluída"), 30_000);
    let tam_novo = fs::metadata(&destino_a).map(|m| m.len()).unwrap_or(0);
    r.check(
        "atualização grava no lugar do arquivo antigo",
        tam_novo == bytes.len() as u64,
        format!("{} bytes", tam_novo),
    );
    let backups = mods.join(".s4suite_backups");
    r.check(
        "cópia do arquivo substituído vai para .s4suite_backups",
        backups.is_dir() && fs::read_dir(&backups).unwrap().count() >= 2,
        format!("{} arquivo(s) no backup", fs::read_dir(&backups).map(|d| d.count()).unwrap_or(0)),
    );

    // (d) zip com executável
    let zip_exe = downloads.join("MegaMod com instalador.zip");
    zip_com(
        &zip_exe,
        &[("MegaMod/bom.package", &amostra[2])],
        &[("MegaMod/setup.exe", b"MZ\x90\x00 fake")],
    );
    let antes_exe = conta(&mods, "package");
    state.set_installer_sources(vec![zip_exe.clone()]);
    w.invoke_start_install();
    esperar(|| !w.get_is_installing(), 30_000);
    r.check(
        "zip com .exe é recusado",
        w.get_installer_status().contains("❌") && !w.get_show_careful_scan_dialog(),
        w.get_installer_status().to_string(),
    );
    r.check(
        "nada do zip recusado entra em Mods, e o staging some",
        conta(&mods, "package") == antes_exe && !mods.join(".s4suite_staging").exists(),
        "",
    );

    // (e) variantes exclusivas
    let zip_opts = downloads.join("Vestido de Festa.zip");
    zip_com(
        &zip_opts,
        &[
            ("Options/S4R_Vestido Vermelho.package", &amostra[3]),
            ("Options/S4R_Vestido Azul.package", &amostra[4]),
        ],
        &[],
    );
    state.set_installer_sources(vec![zip_opts.clone()]);
    w.invoke_start_install();
    let abriu_ex = esperar(|| w.get_show_exclusive_dialog(), 30_000);
    let opcoes: Vec<String> = w.get_exclusive_options().iter().map(|s| s.to_string()).collect();
    r.check(
        "pasta Options/ dispara a escolha de variante",
        abriu_ex && opcoes.len() == 2,
        format!("{:?}", opcoes),
    );
    if abriu_ex {
        let escolhida = opcoes.iter().find(|o| o.contains("Vermelho")).cloned().unwrap_or_default();
        w.invoke_exclusive_option_picked(escolhida.clone().into());
        pump(200);
        r.check("escolher a variante leva ao resumo", w.get_show_careful_scan_dialog(), "");
        w.invoke_proceed_install_clicked();
        esperar(|| w.get_installer_status().contains("concluída"), 30_000);
        let base = mods.join("00_Triagem_Novos/Vestido de Festa/Options");
        r.check(
            "só a variante escolhida é gravada",
            base.join("S4R_Vestido Vermelho.package").exists()
                && !base.join("S4R_Vestido Azul.package").exists(),
            w.get_installer_status().to_string(),
        );
    }

    // (f) script solto
    let script_origem = walkdir::WalkDir::new(&mods)
        .into_iter()
        .filter_map(|e| e.ok())
        .find(|e| e.path().extension().map(|x| x == "ts4script").unwrap_or(false))
        .map(|e| e.path().to_path_buf());
    if let Some(orig) = script_origem {
        let solto = downloads.join("S4R_MeuScript.ts4script");
        fs::copy(&orig, &solto).unwrap();
        state.set_installer_sources(vec![solto.clone()]);
        w.invoke_start_install();
        esperar(|| w.get_show_careful_scan_dialog(), 30_000);
        w.invoke_proceed_install_clicked();
        esperar(|| w.get_installer_status().contains("concluída"), 30_000);
        let dest_script = mods.join("00_Triagem_Novos/S4R_MeuScript.ts4script");
        r.check(
            ".ts4script fica a um nível de Mods",
            dest_script.exists(),
            format!("{}", dest_script.strip_prefix(&mods).unwrap().display()),
        );
    }

    // (g) cancelar
    let zip_cancel = downloads.join("Mod que vou cancelar.zip");
    zip_com(&zip_cancel, &[("Cancelado/S4R_Cancelado.package", &amostra[5])], &[]);
    let antes_cancel = conta(&mods, "package");
    state.set_installer_sources(vec![zip_cancel.clone()]);
    w.invoke_start_install();
    esperar(|| w.get_show_careful_scan_dialog(), 30_000);
    w.invoke_cancel_install_clicked();
    pump(300);
    r.check(
        "cancelar não grava nada e limpa o staging",
        conta(&mods, "package") == antes_cancel && !mods.join(".s4suite_staging").exists(),
        w.get_installer_status().to_string(),
    );

    // (h) dependências declaradas por mods reais
    let deps_vistas = walkdir::WalkDir::new(&mods)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|x| x == "package").unwrap_or(false))
        .take(150)
        .filter(|e| {
            !s4suite::engine::installer::detect_dependencies_in_package(e.path()).is_empty()
        })
        .count();
    r.nota(&format!(
        "dependências declaradas encontradas em {}/150 packages reais varridos",
        deps_vistas
    ));

    // ---------------------------------------------------------------- Ato 3
    r.ato("Ato 3 — Organizador");

    w.invoke_select_tab(2);
    esperar(|| w.get_mod_entries().iter().count() > 0, 20_000);
    let no_disco = s4suite::engine::walk_user_mods(&mods, usize::MAX).count() - 1; // -1: a própria raiz
    let na_arvore = w.get_mod_entries().iter().count();
    r.check(
        "árvore lista a pasta real inteira sem as pastas internas",
        na_arvore == no_disco,
        format!("árvore {} vs disco {}", na_arvore, no_disco),
    );
    let raiz_disco = fs::read_dir(&mods)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| !e.file_name().to_string_lossy().starts_with(".s4suite"))
        .count();
    let mostra_interna = w
        .get_mod_entries()
        .iter()
        .any(|e| e.name.contains(".s4suite"));
    r.check("staging e backups não aparecem na árvore", !mostra_interna, "");

    w.invoke_organizer_search_changed("wicked".into());
    pump(200);
    let filtrados = w.get_mod_entries().iter().count();
    r.check(
        "busca filtra mantendo o caminho até o resultado",
        filtrados > 0 && filtrados < raiz_disco.max(1) * 50,
        format!("{} entradas para 'wicked'", filtrados),
    );
    w.invoke_organizer_search_changed("".into());
    pump(200);

    // nova pasta com acento e emoji
    w.invoke_create_mod_folder();
    r.check("botão de nova pasta abre o campo de nome", w.get_show_organizer_input(), "");
    w.invoke_confirm_organizer_input("Cabelos Ruivos 💇 Coleção".into());
    pump(200);
    let pasta_nova = mods.join("Cabelos Ruivos 💇 Coleção");
    r.check("cria pasta com acento e emoji", pasta_nova.is_dir(), w.get_organizer_status().to_string());

    // copiar packages para dentro dela
    let para_copiar = packages_reais(&mods, 3, 10_000);
    for p in &para_copiar {
        w.invoke_select_mod_entry(p.display().to_string().into());
    }
    r.check(
        "seleção múltipla é contada na UI",
        w.get_organizer_selected_count() == 3,
        format!("{}", w.get_organizer_selected_count()),
    );
    w.invoke_copy_selected_mods();
    r.check("copiar abre o seletor de pasta interno", w.get_show_folder_picker(), "");
    r.check(
        "seletor lista as pastas de Mods",
        w.get_folder_choices().iter().count() > 1,
        format!("{} destinos", w.get_folder_choices().iter().count()),
    );
    w.invoke_folder_destination_picked(pasta_nova.display().to_string().into());
    pump(300);
    r.check(
        "copiar duplica os arquivos no destino",
        conta(&pasta_nova, "package") == 3 && para_copiar.iter().all(|p| p.exists()),
        w.get_organizer_status().to_string(),
    );

    // mover de volta
    let copiados: Vec<PathBuf> = fs::read_dir(&pasta_nova)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .collect();
    let destino_mover = mods.join("Cabelos Ruivos 💇 Coleção/Subpasta");
    fs::create_dir_all(&destino_mover).unwrap();
    for p in &copiados {
        w.invoke_select_mod_entry(p.display().to_string().into());
    }
    w.invoke_move_selected_mods();
    w.invoke_folder_destination_picked(destino_mover.display().to_string().into());
    pump(300);
    r.check(
        "mover tira do lugar de origem",
        conta(&destino_mover, "package") == 3 && copiados.iter().all(|p| !p.exists()),
        w.get_organizer_status().to_string(),
    );

    // renomear
    let alvo_rename = fs::read_dir(&destino_mover).unwrap().filter_map(|e| e.ok()).next().unwrap().path();
    w.invoke_clear_mod_selection();
    w.invoke_select_mod_entry(alvo_rename.display().to_string().into());
    w.invoke_rename_selected_mod();
    w.invoke_confirm_organizer_input("Renomeado à Mão.package".into());
    pump(200);
    r.check(
        "renomear com acento funciona",
        destino_mover.join("Renomeado à Mão.package").exists() && !alvo_rename.exists(),
        w.get_organizer_status().to_string(),
    );

    // desativar / painel de desativados / restaurar
    let alvo_off = destino_mover.join("Renomeado à Mão.package");
    w.invoke_toggle_disabled_mod(alvo_off.display().to_string().into());
    pump(300);
    let off_path = destino_mover.join("Renomeado à Mão.package.disabled");
    r.check("desativar renomeia para .disabled", off_path.exists() && !alvo_off.exists(), "");

    // um .disabled criado fora do app
    fs::write(mods.join("feito_na_mao.package.disabled"), b"x").unwrap();
    w.invoke_open_disabled_panel();
    pump(300);
    let entradas_off: Vec<String> =
        w.get_disabled_entries().iter().map(|e| e.name.to_string()).collect();
    r.check(
        "painel de desativados lista o do app e o feito à mão",
        w.get_show_disabled_panel()
            && entradas_off.iter().any(|n| n.contains("Renomeado"))
            && entradas_off.iter().any(|n| n.contains("feito_na_mao")),
        format!("{:?}", entradas_off),
    );

    w.invoke_toggle_disabled_selection(off_path.display().to_string().into());
    w.invoke_restore_selected_disabled();
    pump(300);
    r.check(
        "restaurar devolve o arquivo ao nome original",
        alvo_off.exists() && !off_path.exists(),
        w.get_organizer_status().to_string(),
    );
    w.invoke_close_disabled_panel();

    // duplicatas
    let dup_origem = &para_copiar[0];
    let dup_copia = mods.join("Cabelos Ruivos 💇 Coleção/copia_identica.package");
    fs::copy(dup_origem, &dup_copia).unwrap();
    w.invoke_find_duplicates();
    esperar(|| !w.get_organizer_status().contains("Procurando"), 60_000);
    pump(400);
    let msg_dup = w.get_organizer_status().to_string();
    let itens_dup: Vec<String> =
        w.get_organizer_confirm_items().iter().map(|s| s.to_string()).collect();
    r.check(
        "busca de duplicatas encontra a cópia recém-criada",
        itens_dup.iter().any(|i| i.contains("copia_identica")) || msg_dup.contains("duplicat"),
        format!("{} | itens: {:?}", msg_dup, itens_dup.iter().take(3).collect::<Vec<_>>()),
    );
    r.check(
        "duplicatas não acusam os próprios backups",
        !itens_dup.iter().any(|i| i.contains(".s4suite_backups")),
        "",
    );
    if w.get_show_organizer_confirm() {
        w.invoke_cancel_organizer_action();
        pump(200);
        r.check("cancelar mantém as duplicatas no disco", dup_copia.exists(), "");
    }

    // lixo
    fs::write(mods.join("Cabelos Ruivos 💇 Coleção/LEIA-ME.txt"), b"lixo").unwrap();
    fs::write(mods.join("Cabelos Ruivos 💇 Coleção/preview.png"), b"lixo").unwrap();
    w.invoke_clean_junk();
    esperar(|| w.get_show_organizer_confirm(), 60_000);
    let lixo: Vec<String> = w.get_organizer_confirm_items().iter().map(|s| s.to_string()).collect();
    r.check(
        "limpeza de lixo lista antes de apagar",
        w.get_show_organizer_confirm() && lixo.iter().any(|i| i.contains("LEIA-ME")),
        format!("{} arquivo(s) listados", lixo.len()),
    );
    w.invoke_confirm_organizer_action();
    pump(500);
    r.check(
        "confirmar apaga o lixo",
        !mods.join("Cabelos Ruivos 💇 Coleção/LEIA-ME.txt").exists(),
        w.get_organizer_status().to_string(),
    );

    // profundidade de script
    let fundo = mods.join("Cabelos Ruivos 💇 Coleção/Subpasta/Mais Fundo");
    fs::create_dir_all(&fundo).unwrap();
    fs::write(fundo.join("script_perdido.ts4script"), b"PK\x03\x04").unwrap();
    let fundos: Vec<String> = s4suite::engine::organizer::check_script_depth(&mods)
        .iter()
        .map(|i| format!("{} (nível {})", i.filename, i.depth))
        .collect();
    r.check(
        "scripts fundos demais são detectados na pasta real",
        fundos.len() >= 2,
        format!("{:?}", fundos),
    );
    w.invoke_check_scripts();
    esperar(|| w.get_show_organizer_confirm(), 30_000);
    pump(300);
    let pediu_confirmacao = w.get_show_organizer_confirm();
    r.check(
        "corrigir profundidade de script pede confirmação antes de mover",
        pediu_confirmacao,
        format!(
            "{} script(s) listados no diálogo antes de sair do lugar",
            w.get_organizer_confirm_items().iter().count()
        ),
    );
    if pediu_confirmacao {
        w.invoke_confirm_organizer_action();
        pump(400);
    }
    r.check(
        "correção deixa o script a um nível de Mods",
        mods.join("00_Scripts_Corrigidos/script_perdido.ts4script").exists(),
        w.get_organizer_status().to_string(),
    );

    // excluir de verdade
    w.invoke_clear_mod_selection();
    w.invoke_select_mod_entry(dup_copia.display().to_string().into());
    w.invoke_delete_selected_mods();
    r.check("excluir pede confirmação", w.get_show_organizer_confirm() && dup_copia.exists(), "");
    w.invoke_confirm_organizer_action();
    pump(400);
    r.check("confirmar exclui o arquivo", !dup_copia.exists(), w.get_organizer_status().to_string());

    // ---------------------------------------------------------------- Ato 4
    r.ato("Ato 4 — Merger e DBPF");

    let grupo_a: Vec<PathBuf> = packages_reais(&mods.join("15 Sliders"), 6, 1_000);
    let grupo_b: Vec<PathBuf> = packages_reais(&mods.join("9 Townie Makeover"), 5, 1_000);
    let (grupo_a, grupo_b) = if grupo_a.len() >= 2 && grupo_b.len() >= 2 {
        (grupo_a, grupo_b)
    } else {
        let todos = packages_reais(&mods, 12, 20_000);
        (todos[..4].to_vec(), todos[4..8].to_vec())
    };

    let saida = mods.join("99 Unificados");
    fs::create_dir_all(&saida).unwrap();

    // Medido antes: a pós-ação apaga ou desativa os originais.
    let todas_entradas: Vec<PathBuf> =
        grupo_a.iter().chain(grupo_b.iter()).cloned().collect();
    let recursos_brutos: usize = todas_entradas.iter().map(|p| recursos_do_package(p)).sum();
    let recursos_entrada = chaves_distintas(&todas_entradas);
    r.check(
        "DBPF lê os packages reais de entrada",
        recursos_entrada > 0,
        format!(
            "{} recursos em {} packages reais, {} chaves TGI distintas",
            recursos_brutos,
            todas_entradas.len(),
            recursos_entrada
        ),
    );

    w.invoke_select_tab(3);
    w.invoke_merge_post_action_selected("backup".into());
    pump(100);
    state.set_merger_inputs(grupo_a.clone());
    state.set_merger_output(Some(saida.clone()));
    w.invoke_add_merge_job();
    pump(100);

    w.invoke_merge_post_action_selected("disable".into());
    state.set_merger_inputs(grupo_b.clone());
    state.set_merger_output(Some(saida.clone()));
    w.invoke_add_merge_job();
    pump(100);

    let jobs: Vec<String> = w
        .get_merge_jobs()
        .iter()
        .map(|j| format!("{} ({}, {})", j.name, j.files_count, j.post_label))
        .collect();
    r.check(
        "duas tarefas na fila com pós-ações diferentes",
        w.get_merge_jobs().iter().count() == 2,
        format!("{:?}", jobs),
    );

    w.invoke_start_merge_queue();
    let terminou = esperar(|| !w.get_is_merging(), 180_000);
    r.check("fila do merger termina", terminou, w.get_merge_status().to_string());

    let partes: Vec<PathBuf> = fs::read_dir(&saida)
        .map(|d| d.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.extension().map(|x| x == "package").unwrap_or(false)).collect())
        .unwrap_or_default();
    r.check(
        "packages unificados foram gerados",
        !partes.is_empty(),
        format!("{} parte(s): {:?}", partes.len(), partes.iter().map(|p| p.file_name().unwrap().to_string_lossy().to_string()).collect::<Vec<_>>()),
    );

    r.check(
        "cada tarefa da fila gera o seu próprio package",
        partes.len() == 2,
        format!(
            "{} arquivo(s) para 2 tarefas, nomeados pelo grupo de origem",
            partes.len()
        ),
    );

    let recursos_saida: usize = partes.iter().map(|p| recursos_do_package(p)).sum();
    r.check(
        "DBPF: o unificado abre e mantém todos os recursos",
        recursos_saida > 0 && recursos_saida >= recursos_entrada,
        format!(
            "{} recursos na saída vs {} chaves distintas na entrada",
            recursos_saida, recursos_entrada
        ),
    );

    let status_jobs: Vec<String> =
        w.get_merge_jobs().iter().map(|j| j.status_label.to_string()).collect();
    r.check(
        "todas as tarefas terminam com sucesso",
        status_jobs.iter().all(|s| s == "Concluído"),
        format!("{:?}", status_jobs),
    );

    let zip_backup: Vec<PathBuf> = walkdir::WalkDir::new(&mods)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|x| x == "zip").unwrap_or(false))
        .map(|e| e.path().to_path_buf())
        .collect();
    r.check(
        "pós-ação backup gera o zip dos originais",
        !zip_backup.is_empty(),
        format!("{:?}", zip_backup.iter().map(|p| p.file_name().unwrap().to_string_lossy().to_string()).collect::<Vec<_>>()),
    );
    r.check(
        "pós-ação backup remove os originais depois de zipar",
        grupo_a.iter().all(|p| !p.exists()),
        format!("{}/{} originais ainda no disco", grupo_a.iter().filter(|p| p.exists()).count(), grupo_a.len()),
    );
    r.check(
        "pós-ação desativar renomeia os originais para .disabled",
        grupo_b.iter().all(|p| !p.exists())
            && grupo_b.iter().all(|p| p.with_extension("package.disabled").exists()),
        format!("{}/{} viraram .disabled", grupo_b.iter().filter(|p| p.with_extension("package.disabled").exists()).count(), grupo_b.len()),
    );

    // A fila guarda as tarefas concluídas; executá-la de novo repetiria um
    // merge cujos originais já foram consumidos pela pós-ação.
    r.nota(&format!(
        "após executar, a fila ainda tem {} tarefa(s) concluída(s) e o botão \"Executar\" continua ativo",
        w.get_merge_jobs().iter().count()
    ));
    for i in 0..w.get_merge_jobs().iter().count() {
        w.invoke_merge_job_clicked(i as i32);
    }
    w.invoke_remove_merge_job();
    pump(100);
    r.check(
        "usuário consegue limpar as tarefas concluídas da fila",
        w.get_merge_jobs().iter().count() == 0,
        w.get_merge_status().to_string(),
    );

    // --- limite de tamanho por parte ---
    let grupo_c = packages_reais(&mods.join("10a Personal CAS"), 20, 1_000);
    let grupo_c = if grupo_c.len() >= 4 { grupo_c } else { packages_reais(&mods, 20, 50_000) };
    let bytes_c: u64 = grupo_c.iter().filter_map(|p| fs::metadata(p).ok()).map(|m| m.len()).sum();
    let limite_gb = (bytes_c as f64 / 3.0) / 1_073_741_824.0;
    w.invoke_merge_limit_selected(format!("{}", limite_gb).into());
    pump(100);

    let saida_c = mods.join("99 Unificados/Limitado");
    state.set_merger_inputs(grupo_c.clone());
    state.set_merger_output(Some(saida_c.clone()));
    w.invoke_merge_post_action_selected("keep".into());
    w.invoke_add_merge_job();
    pump(100);
    w.invoke_start_merge_queue();
    esperar(|| !w.get_is_merging(), 180_000);

    let partes_c: Vec<u64> = fs::read_dir(&saida_c)
        .map(|d| d.filter_map(|e| e.ok()).filter_map(|e| e.metadata().ok()).map(|m| m.len()).collect())
        .unwrap_or_default();
    let teto = (bytes_c as f64 / 3.0) as u64;
    r.check(
        "limite por parte divide a saída em várias partes",
        partes_c.len() > 1,
        format!("{} parte(s) para {} KB de entrada, teto {} KB", partes_c.len(), bytes_c / 1024, teto / 1024),
    );
    r.check(
        "nenhuma parte estoura o limite configurado",
        partes_c.iter().all(|t| *t <= teto),
        format!("maior parte: {} KB", partes_c.iter().max().copied().unwrap_or(0) / 1024),
    );
    r.check(
        "pós-ação manter não toca nos originais",
        grupo_c.iter().all(|p| p.exists()),
        "",
    );

    // --- uma tarefa que falha não derruba as seguintes ---
    for i in 0..w.get_merge_jobs().iter().count() {
        w.invoke_merge_job_clicked(i as i32);
    }
    w.invoke_remove_merge_job();
    pump(100);
    let quebrado = mods.join("99 Unificados/corrompido.package");
    fs::write(&quebrado, b"isto nao e um DBPF valido, e so texto").unwrap();
    state.set_merger_inputs(vec![quebrado.clone()]);
    state.set_merger_output(Some(mods.join("99 Unificados/Falha")));
    w.invoke_add_merge_job();
    let grupo_d = packages_reais(&mods.join("13 Poses and Animation"), 3, 1_000);
    let grupo_d = if grupo_d.len() >= 2 { grupo_d } else { packages_reais(&mods, 3, 30_000) };
    state.set_merger_inputs(grupo_d.clone());
    state.set_merger_output(Some(mods.join("99 Unificados/Depois")));
    w.invoke_add_merge_job();
    pump(100);
    w.invoke_start_merge_queue();
    esperar(|| !w.get_is_merging(), 180_000);

    let status_final: Vec<String> =
        w.get_merge_jobs().iter().map(|j| format!("{}={}", j.name, j.status_label)).collect();
    r.check(
        "tarefa com package corrompido falha sem derrubar a seguinte",
        conta(&mods.join("99 Unificados/Depois"), "package") > 0,
        format!("{:?} | {}", status_final, w.get_merge_status()),
    );

    // ---------------------------------------------------------------- Ato 5
    r.ato("Ato 5 — Tray");

    let trayitem = fs::read_dir(&tray)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| p.extension().map(|x| x == "trayitem").unwrap_or(false));
    let household = fs::read_dir(&tray)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| p.extension().map(|x| x == "householdbinary").unwrap_or(false));

    if let (Some(ti), Some(hb)) = (trayitem, household) {
        // O usuário "baixou" um Sim da galeria: arquivos de tray + CC junto.
        let zip_sim = downloads.join("Sim da Galeria 😀.zip");
        let cc = packages_reais(&mods, 2, 5_000);
        zip_com(
            &zip_sim,
            &[
                (&format!("Sim/{}", ti.file_name().unwrap().to_string_lossy()), &ti),
                (&format!("Sim/{}", hb.file_name().unwrap().to_string_lossy()), &hb),
                ("Sim/CC/S4R_cc_do_sim.package", &cc[0]),
            ],
            &[],
        );

        // Equivalente ao que o handler faz depois do rfd.
        let db = TrayCacheDb::open(&cfg.config_dir().join("mods_cache.db")).ok();
        let (cands, falhas) =
            prepare_tray_candidates(&[zip_sim.clone()], state.tray_work_dir(), db.as_ref()).unwrap();
        r.check(
            "análise do zip encontra o Sim e o CC",
            cands.len() == 1 && cands[0].analysis.tray_files.len() == 2 && cands[0].analysis.package_files.len() == 1,
            format!(
                "nome detectado: {:?}, tipo: {}, falhas: {:?}",
                cands.first().map(|c| c.analysis.detected_name.clone()),
                cands.first().map(|c| c.kind_label()).unwrap_or("-"),
                falhas
            ),
        );
        state.set_tray_candidates(cands);
        w.invoke_select_tab(4);
        // O handler real redesenha a fila logo após a análise; sem o rfd, o
        // primeiro clique num item faz o mesmo refresh.
        w.invoke_tray_item_clicked(0);
        w.invoke_tray_item_clicked(0);
        pump(200);
        let itens: Vec<String> = w
            .get_tray_items()
            .iter()
            .map(|i| format!("{} [{}] {} arq → {}", i.name, i.type_str, i.files_count, i.cc_target_label))
            .collect();
        r.check("fila do tray reflete o conteúdo real", !itens.is_empty(), format!("{:?}", itens));

        // Trocar o destino do CC (o rfd escolheria a pasta; aqui é a mesma chamada).
        let destino_cc = mods.join("Cabelos Ruivos 💇 Coleção");
        state.update_tray_candidates(false, |c| c.cc_target = Some(destino_cc.clone()));
        pump(100);

        w.invoke_tray_item_clicked(0);
        w.invoke_skip_selected_tray();
        pump(200);
        let pulado = w.get_tray_items().iter().next().map(|i| i.skipped).unwrap_or(false);
        r.check("marcar item como pulado reflete na fila", pulado, "");
        w.invoke_skip_selected_tray();
        pump(200);

        let tray_antes = fs::read_dir(&tray).unwrap().count();
        w.invoke_start_tray_import();
        esperar(|| w.get_tray_status().contains("concluída") || w.get_tray_status().contains("⚠️") || w.get_tray_status().contains("❌"), 60_000);
        let tray_depois = fs::read_dir(&tray).unwrap().count();
        r.check(
            "arquivos de Sim vão para a pasta Tray",
            tray_depois >= tray_antes,
            format!("{} → {} arquivos | {}", tray_antes, tray_depois, w.get_tray_status()),
        );
        r.check(
            "CC do Sim vai para a pasta escolhida",
            destino_cc.join("S4R_cc_do_sim.package").exists(),
            format!("{}", w.get_tray_status()),
        );
        r.check(
            "diretório de trabalho do tray é limpo",
            !state.tray_work_dir().exists() || fs::read_dir(state.tray_work_dir()).map(|d| d.count()).unwrap_or(0) == 0,
            "",
        );
    } else {
        r.check("sandbox tem arquivos de Tray", false, "nenhum .trayitem encontrado");
    }

    // ---------------------------------------------------------------- Ato 6
    r.ato("Ato 6 — Traduções");

    let trad_origem = packages_reais(&mods.join("0 Traduçoes"), 1, 0)
        .into_iter()
        .next()
        .or_else(|| packages_reais(&mods, 1, 1_000).into_iter().next())
        .unwrap();
    let trad_baixada = downloads.join("Mod X — Tradução PT-BR.package");
    fs::copy(&trad_origem, &trad_baixada).unwrap();

    let allowed = vec![mods.clone()];
    let rep1 = install_translation(&trad_baixada, &mods, &allowed);
    w.invoke_select_tab(5);
    pump(300);
    let dir_trad = translations_dir(&mods);
    let arquivo_instalado = walkdir::WalkDir::new(&dir_trad)
        .into_iter()
        .filter_map(|e| e.ok())
        .find(|e| e.file_name().to_string_lossy().contains("Mod X"))
        .map(|e| e.path().strip_prefix(&dir_trad).unwrap().display().to_string());
    r.check(
        "tradução em .package avulso é instalada",
        rep1.as_ref().map(|r| r.installed).unwrap_or(0) == 1 && arquivo_instalado.is_some(),
        format!("01_Traducoes/{}", arquivo_instalado.clone().unwrap_or_default()),
    );
    r.check(
        "tradução avulsa não ganha pasta intermediária",
        arquivo_instalado.as_deref() == Some("Mod X — Tradução PT-BR.package"),
        format!(
            "gravada em 01_Traducoes/{}",
            arquivo_instalado.clone().unwrap_or_default()
        ),
    );
    let listadas: Vec<String> =
        w.get_installed_translations().iter().map(|t| t.name.to_string()).collect();
    r.check(
        "a lista da aba mostra a tradução pelo nome que o usuário baixou",
        listadas.iter().any(|n| n.contains("Mod X")),
        format!("lista exibe: {:?}", listadas),
    );

    let rep2 = install_translation(&trad_baixada, &mods, &allowed);
    r.check(
        "reinstalar a mesma tradução não duplica",
        rep2.as_ref().map(|r| r.installed).unwrap_or(9) == 0,
        format!("{:?}", rep2.as_ref().map(|r| (r.installed, r.updated, r.skipped))),
    );

    let zip_trad = downloads.join("Traducao Empacotada.zip");
    zip_com(&zip_trad, &[("PTBR/S4R_Outra Tradução.package", &trad_origem)], &[]);
    let rep3 = install_translation(&zip_trad, &mods, &allowed);
    let extraida = walkdir::WalkDir::new(&dir_trad)
        .into_iter()
        .filter_map(|e| e.ok())
        .any(|e| e.file_name().to_string_lossy().contains("Outra Tradução"));
    r.check(
        "tradução em .zip é extraída, não copiada como zip",
        rep3.is_ok() && extraida && conta(&dir_trad, "zip") == 0,
        format!("{:?}", rep3.as_ref().map(|r| r.installed)),
    );

    // O usuário clica em excluir na linha que a lista mostra.
    w.invoke_select_tab(5);
    pump(200);
    let alvo_del = w
        .get_installed_translations()
        .iter()
        .map(|t| t.name.to_string())
        .find(|n| n.to_lowercase().contains("mod_x") || n.contains("Mod X"))
        .unwrap_or_default();
    w.invoke_delete_translation(alvo_del.clone().into());
    pump(300);
    let sumiu = !walkdir::WalkDir::new(&dir_trad)
        .into_iter()
        .filter_map(|e| e.ok())
        .any(|e| e.file_name().to_string_lossy().contains("Mod X"));
    r.check(
        "excluir pela lista remove a tradução do disco",
        sumiu && !w.get_installed_translations().iter().any(|t| t.name == alvo_del.as_str()),
        format!("linha clicada: '{}' | {}", alvo_del, w.get_translations_status()),
    );

    // ---------------------------------------------------------------- Ato 7
    r.ato("Ato 7 — Configurações e idioma");

    w.invoke_language_selected("en".into());
    pump(200);
    let i18n = w.global::<s4suite::I18n>();
    r.check(
        "trocar para inglês muda a tradução na hora",
        i18n.get_lang() == "en" && i18n.invoke_translate("en".into(), "Salvar".into()) == "Save",
        format!("'Salvar' → '{}'", i18n.invoke_translate("en".into(), "Salvar".into())),
    );
    r.check("idioma é persistido no config", cfg.load().language == "en", "");

    let tema_antes = w.global::<s4suite::Theme>().get_active_theme();
    w.invoke_theme_selected(2);
    pump(200);
    r.check(
        "trocar o tema muda as cores na hora",
        w.global::<s4suite::Theme>().get_active_theme() == 2,
        format!(
            "Theme.active_theme continua {} depois de escolher o índice 2",
            w.global::<s4suite::Theme>().get_active_theme()
        ),
    );
    let _ = tema_antes;
    r.check("tema escolhido é persistido no config", cfg.load().theme == "Cyber Purple", cfg.load().theme);

    w.invoke_merge_limit_selected("2.5".into());
    pump(100);
    r.check(
        "limite do merger é persistido",
        (cfg.load().merge_limit_gb - 2.5).abs() < f64::EPSILON,
        format!("{} GB", cfg.load().merge_limit_gb),
    );

    // Reabrir o app: uma segunda janela com o mesmo config.
    let state2 = Arc::new(AppState::new(cfg.config_dir()));
    let w2 = MainWindow::new().unwrap();
    setup_app_adapter(&w2, Arc::clone(&cfg), Arc::clone(&state2));
    pump(300);
    r.check(
        "reabrir preserva idioma, tema e limite",
        w2.get_active_language() == "en"
            && w2.global::<s4suite::Theme>().get_active_theme() == 2
            && w2.get_merge_max_size() == "2.5",
        format!(
            "lang={} tema={} limite={}",
            w2.get_active_language(),
            w2.global::<s4suite::Theme>().get_active_theme(),
            w2.get_merge_max_size()
        ),
    );
    w.invoke_language_selected("pt".into());
    pump(100);

    // ---------------------------------------------------------------- Ato 8
    r.ato("Ato 8 — Robustez");

    // Staging órfão de uma sessão anterior que o usuário matou no meio.
    let staging = mods.join(".s4suite_staging");
    fs::create_dir_all(staging.join("meio_do_caminho")).unwrap();
    fs::write(staging.join("meio_do_caminho/x.package"), b"lixo").unwrap();
    let state3 = Arc::new(AppState::new(cfg.config_dir()));
    let w3 = MainWindow::new().unwrap();
    setup_app_adapter(&w3, Arc::clone(&cfg), Arc::clone(&state3));
    pump(400);
    r.check(
        "staging órfão não é carregado como mod na abertura seguinte",
        !w3.get_mod_entries().iter().any(|e| e.path_str.contains(".s4suite_staging")),
        "",
    );
    r.check(
        "staging órfão é removido na abertura seguinte",
        !staging.exists(),
        if staging.exists() {
            "continua em Mods; o jogo tentaria carregar esses arquivos".to_string()
        } else {
            String::new()
        },
    );
    let _ = fs::remove_dir_all(&staging);

    // Nome hostil.
    w.invoke_create_mod_folder();
    w.invoke_confirm_organizer_input("../fuga".into());
    pump(200);
    r.check(
        "nome com travessia de diretório é recusado",
        !sandbox.join("fuga").exists() && !mods.parent().unwrap().join("fuga").exists(),
        w.get_organizer_status().to_string(),
    );

    w.invoke_create_mod_folder();
    w.invoke_confirm_organizer_input("Ação 日本語 🎮 ok".into());
    pump(200);
    r.check(
        "nome com unicode variado é aceito",
        mods.join("Ação 日本語 🎮 ok").is_dir(),
        w.get_organizer_status().to_string(),
    );

    // Árvore grande: quanto tempo a UI leva para redesenhar a pasta inteira.
    let inicio = Instant::now();
    w.invoke_refresh_mod_tree();
    pump(60);
    let dur = inicio.elapsed();
    r.check(
        "recarregar a árvore da pasta real é rápido",
        dur < Duration::from_secs(3),
        format!("{:?} para {} entradas de topo", dur, w.get_mod_entries().iter().count()),
    );

    let inicio = Instant::now();
    w.invoke_refresh_stats();
    esperar(|| w.get_total_mods_count() != "0", 60_000);
    r.check(
        "recalcular as estatísticas da pasta real é rápido",
        inicio.elapsed() < Duration::from_secs(10),
        format!("{:?}", inicio.elapsed()),
    );

    // ---------------------------------------------------------------- Fim
    println!("\n\x1b[1m━━━ Resultado ━━━\x1b[0m");
    println!("  \x1b[32m{} verificações passaram\x1b[0m", r.ok);
    if r.falhas.is_empty() {
        println!("  \x1b[32mnenhuma falha\x1b[0m");
    } else {
        println!("  \x1b[31m{} falha(s):\x1b[0m", r.falhas.len());
        for f in &r.falhas {
            println!("    \x1b[31m•\x1b[0m {}", f);
        }
    }
    println!(
        "\nEstado final: {} packages, {} scripts, {} no Tray",
        conta(&mods, "package"),
        conta(&mods, "ts4script"),
        fs::read_dir(&tray).map(|d| d.count()).unwrap_or(0)
    );
}
