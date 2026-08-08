use crate::bridge::state::{AppState, PendingOrganizerAction};
use crate::core::config::{get_mods_dir, get_saves_dir, get_tray_dir, ConfigManager};
use crate::core::i18n::t;
use crate::engine::installer::{
    calculate_conflicts, clear_staging, execute_installation, prepare_staging, ConflictType,
    STAGING_DIR_NAME,
};
use crate::engine::merger::{
    apply_post_merge_action, common_ancestor, merge_sims4_packages, MergeTask, PostMergeAction,
};
use crate::engine::organizer::{
    auto_fix_script_depth, check_script_depth, detect_duplicates, find_junk_files, remove_files,
    scan_mods_tree,
};
use crate::engine::reshade::{
    detect_environment, detect_installation, install_reshade, install_shader_preset,
    uninstall_reshade,
};
use crate::engine::tray::{
    clear_tray_work_dir, import_tray_item, prepare_tray_candidates, TrayCacheDb,
};
use crate::MainWindow;
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

pub fn setup_app_adapter(window: &MainWindow, config_mgr: Arc<ConfigManager>, state: Arc<AppState>) {
    let weak_win = window.as_weak();

    // Tradução: a UI chama I18n.tr(chave), que repassa para core::i18n::t com
    // o idioma corrente. Precisa ser instalado antes do primeiro refresh.
    let i18n = window.global::<crate::I18n>();
    i18n.on_translate(|lang, key| SharedString::from(t(key.as_str(), lang.as_str())));
    i18n.set_lang(SharedString::from(config_mgr.load().language.clone()));

    // Initial stats load
    refresh_dashboard_stats(&weak_win, &config_mgr);
    refresh_organizer_tree(&weak_win, &config_mgr);
    refresh_reshade_env(&weak_win, &config_mgr);
    refresh_translations_list(&weak_win, &config_mgr);

    // Callback: select_tab
    let cfg_clone = Arc::clone(&config_mgr);
    let weak_clone = weak_win.clone();
    window.on_select_tab(move |idx| {
        if idx == 0 {
            refresh_dashboard_stats(&weak_clone, &cfg_clone);
        } else if idx == 2 {
            refresh_organizer_tree(&weak_clone, &cfg_clone);
        } else if idx == 5 {
            refresh_translations_list(&weak_clone, &cfg_clone);
        } else if idx == 6 {
            refresh_reshade_env(&weak_clone, &cfg_clone);
        }
    });

    // Callback: clear_cache
    let cfg_clone = Arc::clone(&config_mgr);
    let weak_clone = weak_win.clone();
    window.on_clear_cache(move || {
        let config = cfg_clone.load();
        if let Some(sims_path) = &config.sims4_path {
            let locothumb = sims4_path_cache(sims_path);
            let lang = config.language.clone();
            let weak_async = weak_clone.clone();
            slint::spawn_local(async move {
                let mut _count = 0;
                for p in locothumb {
                    if p.exists() {
                        let _ = fs::remove_file(p);
                        _count += 1;
                    }
                }
                if let Some(w) = weak_async.upgrade() {
                    w.set_dashboard_status(SharedString::from(format!(
                        "🧹 {}",
                        t("Arquivos de cache limpos com sucesso!", &lang)
                    )));
                }
            })
            .unwrap();
        }
    });

    // Callback: backup_saves
    let cfg_clone = Arc::clone(&config_mgr);
    let weak_clone = weak_win.clone();
    window.on_backup_saves(move || {
        let config = cfg_clone.load();
        if let Some(sims_path) = &config.sims4_path {
            let saves_dir = get_saves_dir(sims_path);
            if saves_dir.exists() {
                if let Some(dest_file) = rfd::FileDialog::new()
                    .set_file_name("Saves_Backup.zip")
                    .save_file()
                {
                    let weak_async = weak_clone.clone();
                    slint::spawn_local(async move {
                        if create_zip_archive(&saves_dir, &dest_file).is_ok() {
                            if let Some(w) = weak_async.upgrade() {
                                w.set_dashboard_status(SharedString::from(format!(
                                    "🛡️ Backup salvo em: {}",
                                    dest_file.display()
                                )));
                            }
                        }
                    })
                    .unwrap();
                }
            }
        }
    });

    // Callback: refresh_stats
    let cfg_clone = Arc::clone(&config_mgr);
    let weak_clone = weak_win.clone();
    window.on_refresh_stats(move || {
        refresh_dashboard_stats(&weak_clone, &cfg_clone);
    });

    // Callback: select_installer_files
    let state_clone = Arc::clone(&state);
    let weak_clone = weak_win.clone();
    window.on_select_installer_files(move || {
        if let Some(files) = rfd::FileDialog::new()
            .add_filter("Mods & Archives", &["zip", "7z", "rar", "package", "ts4script"])
            .pick_files()
        {
            // Os caminhos reais ficam no estado; a fila da tela é só o reflexo.
            state_clone.set_installer_sources(files.clone());

            if let Some(w) = weak_clone.upgrade() {
                let model = Rc::new(VecModel::default());
                for f in &files {
                    let size = fs::metadata(f).map(|m| m.len()).unwrap_or(0);
                    model.push(crate::QueueItem {
                        name: SharedString::from(f.file_name().unwrap_or_default().to_string_lossy().to_string()),
                        size_str: SharedString::from(crate::bridge::slint_models::format_size_bytes(size)),
                        status: SharedString::from("Pronto"),
                    });
                }
                w.set_installer_queue(ModelRc::from(model));
                w.set_installer_status(SharedString::from(format!(
                    "{} arquivo(s) na fila. Clique em Iniciar Instalação.",
                    files.len()
                )));
            }
        }
    });

    // Callback: start_install
    // Fase 1 de 2: valida, extrai para o staging e calcula conflitos. Nada é
    // gravado em Mods até o usuário confirmar no diálogo.
    let cfg_clone = Arc::clone(&config_mgr);
    let state_clone = Arc::clone(&state);
    let weak_clone = weak_win.clone();
    window.on_start_install(move || {
        let config = cfg_clone.load();
        let Some(sims_path) = config.sims4_path.clone() else {
            set_installer_error(&weak_clone, "⚠️ Configure a pasta do jogo antes de instalar mods.");
            return;
        };

        let sources = state_clone.installer_sources();
        if sources.is_empty() {
            set_installer_error(&weak_clone, "⚠️ Nenhum arquivo na fila. Selecione mods primeiro.");
            return;
        }

        let mods_dir = get_mods_dir(&sims_path);
        let staging = mods_dir.join(STAGING_DIR_NAME);

        if let Some(w) = weak_clone.upgrade() {
            w.set_is_installing(true);
            w.set_installer_status(SharedString::from("⚙️ Verificando segurança e extraindo arquivos..."));
        }

        let weak_async = weak_clone.clone();
        let state_async = Arc::clone(&state_clone);
        tokio::task::spawn_blocking(move || {
            let outcome = prepare_staging(&sources, &staging)
                .and_then(|staging_report| {
                    calculate_conflicts(&staging, &mods_dir).map(|conflicts| (staging_report, conflicts))
                });

            match outcome {
                Ok((staging_report, conflicts)) => {
                    let summary = format!(
                        "O instalador encontrou:\n\
                         • {} mods novos\n\
                         • {} atualizações\n\
                         • {} já instalados (serão ignorados)\n\n\
                         Deseja aplicar essas alterações à sua pasta Mods?",
                        conflicts.new_files, conflicts.updates, conflicts.exact_matches
                    );
                    let warnings = if staging_report.failures.is_empty() {
                        String::new()
                    } else {
                        format!(
                            "\n\n⚠️ {} arquivo(s) ignorado(s): {}",
                            staging_report.failures.len(),
                            staging_report
                                .failures
                                .iter()
                                .map(|(name, err)| format!("{} ({})", name, err))
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    };
                    let statuses: Vec<(String, String)> = conflicts
                        .staged_files
                        .iter()
                        .map(|f| {
                            let label = match f.conflict {
                                ConflictType::ExactMatch => "Já instalado",
                                ConflictType::SizeDiff => "Atualização",
                                ConflictType::NewFile => "Novo",
                            };
                            (f.filename.clone(), label.to_string())
                        })
                        .collect();

                    state_async.set_pending_install(conflicts);

                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(w) = weak_async.upgrade() {
                            let model = Rc::new(VecModel::default());
                            for (name, status) in statuses {
                                model.push(crate::QueueItem {
                                    name: SharedString::from(name),
                                    size_str: SharedString::default(),
                                    status: SharedString::from(status),
                                });
                            }
                            w.set_installer_queue(ModelRc::from(model));
                            w.set_careful_scan_message(SharedString::from(format!("{}{}", summary, warnings)));
                            w.set_show_careful_scan_dialog(true);
                        }
                    });
                }
                Err(e) => {
                    // Falhou antes de tocar em Mods: só o staging precisa sumir.
                    let _ = clear_staging(&staging);
                    let msg = format!("❌ {}", e);
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(w) = weak_async.upgrade() {
                            w.set_is_installing(false);
                            w.set_installer_status(SharedString::from(msg));
                        }
                    });
                }
            }
        });
    });

    // Fase 2 de 2: o usuário confirmou. Aqui sim escrevemos em Mods.
    let cfg_clone = Arc::clone(&config_mgr);
    let state_clone = Arc::clone(&state);
    let weak_clone = weak_win.clone();
    window.on_proceed_install_clicked(move || {
        let config = cfg_clone.load();
        let Some(sims_path) = config.sims4_path.clone() else {
            return;
        };

        // `take` garante que um duplo-clique não instale duas vezes.
        let Some(report) = state_clone.take_pending_install() else {
            return;
        };

        let mods_dir = get_mods_dir(&sims_path);
        let staging = mods_dir.join(STAGING_DIR_NAME);
        let allowed_roots = vec![mods_dir.clone()];

        if let Some(w) = weak_clone.upgrade() {
            w.set_show_careful_scan_dialog(false);
            w.set_installer_status(SharedString::from("⚡ Aplicando modificações em disco..."));
        }

        let weak_async = weak_clone.clone();
        let state_async = Arc::clone(&state_clone);
        tokio::task::spawn_blocking(move || {
            let result = execute_installation(&report, &mods_dir, &allowed_roots);
            let _ = clear_staging(&staging);

            let (msg, clear_queue) = match result {
                Ok((installed, skipped)) => {
                    state_async.clear_installer();
                    let mut m = format!("✅ Instalação concluída! {} mod(s) instalados.", installed);
                    if skipped > 0 {
                        m.push_str(&format!(" {} já estavam atualizados.", skipped));
                    }
                    (m, true)
                }
                // `execute_installation` reverte sozinho; a fila fica para retentar.
                Err(e) => (format!("❌ Falha na instalação (alterações revertidas): {}", e), false),
            };

            let _ = slint::invoke_from_event_loop(move || {
                if let Some(w) = weak_async.upgrade() {
                    w.set_is_installing(false);
                    if clear_queue {
                        w.set_installer_queue(ModelRc::from(Rc::new(VecModel::default())));
                    }
                    w.set_installer_status(SharedString::from(msg));
                }
            });
        });
    });

    let cfg_clone = Arc::clone(&config_mgr);
    let state_clone = Arc::clone(&state);
    let weak_clone = weak_win.clone();
    window.on_cancel_install_clicked(move || {
        // Cancelar precisa remover o staging: senão os arquivos extraídos ficam
        // dentro de Mods e o jogo tenta carregá-los.
        let config = cfg_clone.load();
        if let Some(sims_path) = &config.sims4_path {
            let _ = clear_staging(&get_mods_dir(sims_path).join(STAGING_DIR_NAME));
        }
        state_clone.take_pending_install();

        if let Some(w) = weak_clone.upgrade() {
            w.set_show_careful_scan_dialog(false);
            w.set_is_installing(false);
            w.set_installer_status(SharedString::from("⚠️ Instalação cancelada pelo usuário."));
        }
    });

    // Callback: clear_installer_queue
    let cfg_clone = Arc::clone(&config_mgr);
    let state_clone = Arc::clone(&state);
    let weak_clone = weak_win.clone();
    window.on_clear_installer_queue(move || {
        let config = cfg_clone.load();
        if let Some(sims_path) = &config.sims4_path {
            let _ = clear_staging(&get_mods_dir(sims_path).join(STAGING_DIR_NAME));
        }
        state_clone.clear_installer();

        if let Some(w) = weak_clone.upgrade() {
            w.set_installer_queue(ModelRc::from(Rc::new(VecModel::default())));
            w.set_is_installing(false);
            w.set_installer_status(SharedString::from("Fila limpa."));
        }
    });
    // Callback: refresh_mod_tree
    let cfg_clone = Arc::clone(&config_mgr);
    let weak_clone = weak_win.clone();
    window.on_refresh_mod_tree(move || {
        refresh_organizer_tree(&weak_clone, &cfg_clone);
    });

    // Callback: select_mod_entry
    let state_clone = Arc::clone(&state);
    let weak_clone = weak_win.clone();
    window.on_select_mod_entry(move |path| {
        let path_buf = PathBuf::from(path.as_str());
        let is_disabled = path.as_str().ends_with(".disabled");
        state_clone.set_selected_mod(Some(path_buf));

        if let Some(w) = weak_clone.upgrade() {
            w.set_selected_mod_path(path);
            w.set_selected_mod_is_disabled(is_disabled);
        }
    });

    // Callback: check_scripts
    let cfg_clone = Arc::clone(&config_mgr);
    let weak_clone = weak_win.clone();
    window.on_check_scripts(move || {
        let config = cfg_clone.load();
        if let Some(sims_path) = &config.sims4_path {
            let mods_dir = get_mods_dir(sims_path);
            let issues = check_script_depth(&mods_dir);
            if issues.is_empty() {
                if let Some(w) = weak_clone.upgrade() {
                    w.set_organizer_status(SharedString::from("✨ Todos os scripts estão no nível correto!"));
                }
            } else {
                let fixed = auto_fix_script_depth(&issues, &mods_dir).unwrap_or(0);
                refresh_organizer_tree(&weak_clone, &cfg_clone);
                if let Some(w) = weak_clone.upgrade() {
                    w.set_organizer_status(SharedString::from(format!(
                        "🔧 {} script(s) movidos para 00_Scripts_Corrigidos",
                        fixed
                    )));
                }
            }
        }
    });

    // Callback: toggle_disabled_mod
    // Desativar renomeia para `.disabled` e registra no manifesto, de onde o
    // caminho original é recuperado na reativação.
    let cfg_clone = Arc::clone(&config_mgr);
    let state_clone = Arc::clone(&state);
    let weak_clone = weak_win.clone();
    window.on_toggle_disabled_mod(move |path| {
        let config = cfg_clone.load();
        let Some(sims_path) = config.sims4_path.clone() else {
            return;
        };
        let target = PathBuf::from(path.as_str());
        if target.as_os_str().is_empty() {
            return;
        }

        let mods_dir = get_mods_dir(&sims_path);
        let mgr = state_clone.disabled_manager();

        let result = if path.as_str().ends_with(".disabled") {
            mgr.enable_mod(&target, &mods_dir)
                .map(|p| format!("🟢 Mod reativado: {}", file_label(&p)))
        } else {
            mgr.disable_mod(&target, &mods_dir, "manual", "Desativado pelo organizador", &[mods_dir.clone()])
                .map(|p| format!("🔴 Mod desativado: {}", file_label(&p)))
        };

        // O caminho selecionado deixou de existir de qualquer forma.
        state_clone.set_selected_mod(None);
        refresh_organizer_tree(&weak_clone, &cfg_clone);

        if let Some(w) = weak_clone.upgrade() {
            w.set_selected_mod_path(SharedString::default());
            w.set_selected_mod_is_disabled(false);
            w.set_organizer_status(SharedString::from(match result {
                Ok(msg) => msg,
                Err(e) => format!("❌ Não foi possível alternar o mod: {}", e),
            }));
        }
    });

    // Callback: find_duplicates
    let cfg_clone = Arc::clone(&config_mgr);
    let state_clone = Arc::clone(&state);
    let weak_clone = weak_win.clone();
    window.on_find_duplicates(move || {
        let config = cfg_clone.load();
        let Some(sims_path) = config.sims4_path.clone() else {
            return;
        };
        let mods_dir = get_mods_dir(&sims_path);

        if let Some(w) = weak_clone.upgrade() {
            w.set_organizer_status(SharedString::from("🔍 Comparando hashes dos packages..."));
        }

        let weak_async = weak_clone.clone();
        let state_async = Arc::clone(&state_clone);
        tokio::task::spawn_blocking(move || {
            let groups = detect_duplicates(&mods_dir);

            let redundant: Vec<PathBuf> = groups.iter().flat_map(|g| g.redundant()).collect();
            let freed: u64 = redundant
                .iter()
                .map(|p| fs::metadata(p).map(|m| m.len()).unwrap_or(0))
                .sum();
            let preview: Vec<String> = redundant.iter().map(|p| p.display().to_string()).collect();
            let group_count = groups.len();

            state_async.set_pending_organizer(PendingOrganizerAction::RemoveDuplicates(groups));

            let _ = slint::invoke_from_event_loop(move || {
                if let Some(w) = weak_async.upgrade() {
                    if preview.is_empty() {
                        w.set_organizer_status(SharedString::from("✨ Nenhuma duplicata exata encontrada."));
                        return;
                    }
                    w.set_organizer_status(SharedString::from(format!(
                        "🔍 {} grupo(s) de duplicatas encontrados.",
                        group_count
                    )));
                    w.set_organizer_confirm_title(SharedString::from("🔍 Duplicatas exatas encontradas"));
                    w.set_organizer_confirm_message(SharedString::from(format!(
                        "{} grupo(s) de arquivos idênticos. Uma cópia de cada é mantida e as {} restantes serão apagadas, liberando {}.",
                        group_count,
                        preview.len(),
                        crate::bridge::slint_models::format_size_bytes(freed)
                    )));
                    w.set_organizer_confirm_items(to_string_model(preview));
                    w.set_show_organizer_confirm(true);
                }
            });
        });
    });

    // Callback: clean_junk
    let cfg_clone = Arc::clone(&config_mgr);
    let state_clone = Arc::clone(&state);
    let weak_clone = weak_win.clone();
    window.on_clean_junk(move || {
        let config = cfg_clone.load();
        let Some(sims_path) = config.sims4_path.clone() else {
            return;
        };
        let mods_dir = get_mods_dir(&sims_path);
        let junk = find_junk_files(&mods_dir);

        if junk.is_empty() {
            if let Some(w) = weak_clone.upgrade() {
                w.set_organizer_status(SharedString::from("✨ Nenhum arquivo de lixo encontrado."));
            }
            return;
        }

        let freed: u64 = junk
            .iter()
            .map(|p| fs::metadata(p).map(|m| m.len()).unwrap_or(0))
            .sum();
        let preview: Vec<String> = junk.iter().map(|p| p.display().to_string()).collect();
        let count = junk.len();

        state_clone.set_pending_organizer(PendingOrganizerAction::CleanJunk(junk));

        if let Some(w) = weak_clone.upgrade() {
            w.set_organizer_confirm_title(SharedString::from("🧹 Arquivos de lixo encontrados"));
            w.set_organizer_confirm_message(SharedString::from(format!(
                "{} arquivo(s) que o jogo não carrega (imagens, textos, atalhos), somando {}. Confira a lista antes de confirmar — alguns podem ser leia-me que você queira guardar.",
                count,
                crate::bridge::slint_models::format_size_bytes(freed)
            )));
            w.set_organizer_confirm_items(to_string_model(preview));
            w.set_show_organizer_confirm(true);
        }
    });

    // Callback: confirm_organizer_action
    // Único ponto onde o organizador apaga algo, sempre via safe_remove_file.
    let cfg_clone = Arc::clone(&config_mgr);
    let state_clone = Arc::clone(&state);
    let weak_clone = weak_win.clone();
    window.on_confirm_organizer_action(move || {
        let config = cfg_clone.load();
        let Some(sims_path) = config.sims4_path.clone() else {
            return;
        };
        let Some(pending) = state_clone.take_pending_organizer() else {
            return;
        };

        let mods_dir = get_mods_dir(&sims_path);
        let allowed_roots = vec![mods_dir.clone()];

        if let Some(w) = weak_clone.upgrade() {
            w.set_show_organizer_confirm(false);
            w.set_organizer_confirm_items(to_string_model(Vec::new()));
            w.set_organizer_status(SharedString::from("🗑️ Removendo arquivos..."));
        }

        let (targets, what) = match pending {
            PendingOrganizerAction::RemoveDuplicates(groups) => (
                groups.iter().flat_map(|g| g.redundant()).collect::<Vec<_>>(),
                "duplicata(s)",
            ),
            PendingOrganizerAction::CleanJunk(files) => (files, "arquivo(s) de lixo"),
        };

        let weak_async = weak_clone.clone();
        let cfg_async = Arc::clone(&cfg_clone);
        tokio::task::spawn_blocking(move || {
            let outcome = remove_files(&targets, &allowed_roots);
            let mut msg = format!(
                "🗑️ {} {} removidos ({} liberados).",
                outcome.removed,
                what,
                crate::bridge::slint_models::format_size_bytes(outcome.freed_bytes)
            );
            if !outcome.failures.is_empty() {
                msg.push_str(&format!(" ⚠️ {} bloqueados por segurança.", outcome.failures.len()));
            }

            let _ = slint::invoke_from_event_loop(move || {
                refresh_organizer_tree(&weak_async, &cfg_async);
                if let Some(w) = weak_async.upgrade() {
                    w.set_organizer_status(SharedString::from(msg));
                }
            });
        });
    });

    // Callback: cancel_organizer_action
    let state_clone = Arc::clone(&state);
    let weak_clone = weak_win.clone();
    window.on_cancel_organizer_action(move || {
        state_clone.take_pending_organizer();
        if let Some(w) = weak_clone.upgrade() {
            w.set_show_organizer_confirm(false);
            w.set_organizer_confirm_items(to_string_model(Vec::new()));
            w.set_organizer_status(SharedString::from("Operação cancelada. Nada foi removido."));
        }
    });

    // Callback: select_merger_input
    let state_clone = Arc::clone(&state);
    let weak_clone = weak_win.clone();
    window.on_select_merger_input(move || {
        if let Some(dir) = rfd::FileDialog::new().pick_folder() {
            let mut inputs: Vec<PathBuf> = Vec::new();
            let model = Rc::new(VecModel::default());

            for entry in crate::engine::walk_user_mods(&dir, usize::MAX) {
                let path = entry.path();
                let is_package = path.is_file()
                    && path.extension().and_then(|e| e.to_str()).unwrap_or("").eq_ignore_ascii_case("package");
                if !is_package {
                    continue;
                }
                let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                model.push(crate::MergeQueueItem {
                    name: SharedString::from(entry.file_name().to_string_lossy().to_string()),
                    size_str: SharedString::from(crate::bridge::slint_models::format_size_bytes(size)),
                });
                inputs.push(path.to_path_buf());
            }

            let total: u64 = inputs.iter().map(|p| fs::metadata(p).map(|m| m.len()).unwrap_or(0)).sum();
            let count = inputs.len();
            state_clone.set_merger_inputs(inputs);

            if let Some(w) = weak_clone.upgrade() {
                w.set_merge_items(ModelRc::from(model));
                w.set_merge_input_dir(SharedString::from(dir.display().to_string()));
                w.set_merge_status(SharedString::from(if count == 0 {
                    "⚠️ Nenhum .package encontrado nessa pasta.".to_string()
                } else {
                    format!(
                        "{} package(s) prontos para unificar ({}).",
                        count,
                        crate::bridge::slint_models::format_size_bytes(total)
                    )
                }));
            }
        }
    });

    // Callback: select_merger_output
    let state_clone = Arc::clone(&state);
    let weak_clone = weak_win.clone();
    window.on_select_merger_output(move || {
        if let Some(dir) = rfd::FileDialog::new().pick_folder() {
            state_clone.set_merger_output(Some(dir.clone()));
            if let Some(w) = weak_clone.upgrade() {
                w.set_merge_output_dir(SharedString::from(dir.display().to_string()));
                w.set_merge_status(SharedString::from(format!("Destino: {}", dir.display())));
            }
        }
    });

    // Callback: start_merge
    // Antes isto era uma simulação: mostrava o diálogo sem nunca chamar o
    // engine. Agora roda o merge DBPF de verdade, com progresso.
    let cfg_clone = Arc::clone(&config_mgr);
    let state_clone = Arc::clone(&state);
    let weak_clone = weak_win.clone();
    window.on_start_merge(move || {
        let inputs = state_clone.merger_inputs();
        let Some(output_dir) = state_clone.merger_output() else {
            if let Some(w) = weak_clone.upgrade() {
                w.set_merge_status(SharedString::from("⚠️ Selecione a pasta de destino antes de unificar."));
            }
            return;
        };
        if inputs.is_empty() {
            if let Some(w) = weak_clone.upgrade() {
                w.set_merge_status(SharedString::from("⚠️ Nenhum package selecionado."));
            }
            return;
        }

        let max_size_gb = cfg_clone.load().merge_limit_gb;
        let task = MergeTask {
            input_files: inputs,
            output_dir,
            max_size_bytes: (max_size_gb * 1024.0 * 1024.0 * 1024.0) as u64,
        };

        if let Some(w) = weak_clone.upgrade() {
            w.set_is_merging(true);
            w.set_merge_progress(0.0);
            w.set_merge_status(SharedString::from("🚀 Lendo índices DBPF..."));
        }

        let weak_async = weak_clone.clone();
        let weak_progress = weak_clone.clone();
        tokio::task::spawn_blocking(move || {
            let result = merge_sims4_packages(&task, move |current, total, label| {
                let fraction = if total == 0 { 0.0 } else { current as f32 / total as f32 };
                let text = label.to_string();
                let weak_tick = weak_progress.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(w) = weak_tick.upgrade() {
                        w.set_merge_progress(fraction);
                        w.set_merge_status(SharedString::from(text));
                    }
                });
            });

            let _ = slint::invoke_from_event_loop(move || {
                let Some(w) = weak_async.upgrade() else { return };
                w.set_is_merging(false);
                w.set_merge_progress(0.0);

                match result {
                    Ok(report) if !report.output_packages.is_empty() => {
                        let mut msg = format!(
                            "✅ {} package(s) unificados em {} parte(s).",
                            report.success_count,
                            report.output_packages.len()
                        );
                        if report.partial {
                            msg.push_str(&format!(" ⚠️ {} arquivo(s) falharam e foram ignorados.", report.failed_files.len()));
                        }
                        w.set_merge_status(SharedString::from(msg));
                        w.set_post_merge_message(SharedString::from(format!(
                            "Foram geradas {} parte(s) otimizadas. Os {} arquivos originais continuam na pasta de origem — mantê-los junto das partes unificadas duplica o conteúdo dentro do jogo.",
                            report.output_packages.len(),
                            report.success_count
                        )));
                        w.set_show_post_merge_dialog(true);
                    }
                    Ok(report) => {
                        w.set_merge_status(SharedString::from(format!(
                            "❌ Nenhum recurso pôde ser lido ({} arquivo(s) com falha).",
                            report.failed_files.len()
                        )));
                    }
                    Err(e) => {
                        w.set_merge_status(SharedString::from(format!("❌ Falha na unificação: {}", e)));
                    }
                }
            });
        });
    });

    // Callback: post_merge_action
    // Executa de verdade o destino dos originais. Antes só trocava o texto.
    let state_clone = Arc::clone(&state);
    let weak_clone = weak_win.clone();
    window.on_post_merge_action(move |action| {
        let choice = PostMergeAction::parse(action.as_str());
        let originals = state_clone.merger_inputs();

        if let Some(w) = weak_clone.upgrade() {
            w.set_show_post_merge_dialog(false);
        }

        if choice == PostMergeAction::Keep {
            state_clone.clear_merger();
            if let Some(w) = weak_clone.upgrade() {
                w.set_merge_items(ModelRc::from(Rc::new(VecModel::default())));
                w.set_merge_input_dir(SharedString::default());
                w.set_merge_status(SharedString::from("Arquivos originais mantidos como estavam."));
            }
            return;
        }

        // A pasta de origem é a raiz de segurança: nada fora dela pode ser
        // tocado, mesmo que o manifesto ou a lista digam o contrário.
        let Some(input_root) = common_ancestor(&originals) else {
            return;
        };

        if let Some(w) = weak_clone.upgrade() {
            w.set_merge_status(SharedString::from("⏳ Processando arquivos originais..."));
        }

        let weak_async = weak_clone.clone();
        let state_async = Arc::clone(&state_clone);
        tokio::task::spawn_blocking(move || {
            let outcome =
                apply_post_merge_action(choice, &originals, &input_root, state_async.disabled_manager());
            state_async.clear_merger();

            let msg = if let Some(err) = &outcome.error {
                format!("❌ Backup falhou, nada foi removido: {}", err)
            } else {
                let mut m = match choice {
                    PostMergeAction::Disable => {
                        format!("🔴 {} original(is) desativados (.disabled).", outcome.affected)
                    }
                    PostMergeAction::Backup => format!(
                        "📦 {} original(is) arquivados em {} e removidos da pasta.",
                        outcome.affected,
                        outcome
                            .backup_path
                            .as_deref()
                            .map(file_label)
                            .unwrap_or_default()
                    ),
                    PostMergeAction::Delete => format!(
                        "🗑️ {} original(is) excluídos ({} liberados).",
                        outcome.affected,
                        crate::bridge::slint_models::format_size_bytes(outcome.freed_bytes)
                    ),
                    PostMergeAction::Keep => "Arquivos originais mantidos.".to_string(),
                };
                if outcome.failed > 0 {
                    m.push_str(&format!(" ⚠️ {} bloqueados por segurança.", outcome.failed));
                }
                m
            };

            let _ = slint::invoke_from_event_loop(move || {
                if let Some(w) = weak_async.upgrade() {
                    w.set_merge_items(ModelRc::from(Rc::new(VecModel::default())));
                    w.set_merge_input_dir(SharedString::default());
                    w.set_merge_status(SharedString::from(msg));
                }
            });
        });
    });

    // Callback: select_tray_files
    // Extrai e analisa cada fonte para mostrar o que ela realmente contém.
    // Antes a fila exibia "Sim / Lote" e "1" fixos, sem abrir os arquivos.
    let cfg_clone = Arc::clone(&config_mgr);
    let state_clone = Arc::clone(&state);
    let weak_clone = weak_win.clone();
    window.on_select_tray_files(move || {
        let Some(files) = rfd::FileDialog::new()
            .add_filter(
                "Sims / Lot Archives",
                &["zip", "7z", "rar", "trayitem", "householdbinary", "sgi", "hhi", "bpi", "blueprint", "room", "rmi"],
            )
            .pick_files()
        else {
            return;
        };

        state_clone.set_tray_sources(files.clone());
        if let Some(w) = weak_clone.upgrade() {
            w.set_is_importing_tray(true);
            w.set_tray_status(SharedString::from("🔎 Extraindo e analisando arquivos de Tray..."));
        }

        let work_dir = state_clone.tray_work_dir().to_path_buf();
        let db_path = cfg_clone.config_dir().join("mods_cache.db");
        let weak_async = weak_clone.clone();
        let state_async = Arc::clone(&state_clone);

        tokio::task::spawn_blocking(move || {
            // O cache de hashes é um bônus: sem ele a análise roda igual,
            // apenas sem marcar CC que já está instalado.
            let cache = TrayCacheDb::open(&db_path).ok();
            let result = prepare_tray_candidates(&files, &work_dir, cache.as_ref());

            match result {
                Ok((candidates, failures)) => {
                    let rows: Vec<(String, String, String)> = candidates
                        .iter()
                        .map(|c| {
                            let dups = c.analysis.duplicate_count;
                            let count = if dups > 0 {
                                format!("{} ({} dup.)", c.total_files(), dups)
                            } else {
                                c.total_files().to_string()
                            };
                            (
                                c.analysis.detected_name.clone(),
                                c.kind_label().to_string(),
                                count,
                            )
                        })
                        .collect();

                    let mut status = if rows.is_empty() {
                        "⚠️ Nenhum Sim, Lote ou CC encontrado nos arquivos escolhidos.".to_string()
                    } else {
                        format!("{} item(ns) prontos para importar.", rows.len())
                    };
                    if !failures.is_empty() {
                        status.push_str(&format!(
                            " ⚠️ {} ignorado(s): {}",
                            failures.len(),
                            failures
                                .iter()
                                .map(|(n, e)| format!("{} ({})", n, e))
                                .collect::<Vec<_>>()
                                .join(", ")
                        ));
                    }

                    state_async.set_tray_candidates(candidates);

                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(w) = weak_async.upgrade() {
                            let model = Rc::new(VecModel::default());
                            for (name, kind, count) in rows {
                                model.push(crate::TrayItem {
                                    name: SharedString::from(name),
                                    type_str: SharedString::from(kind),
                                    files_count: SharedString::from(count),
                                });
                            }
                            w.set_is_importing_tray(false);
                            w.set_tray_items(ModelRc::from(model));
                            w.set_tray_status(SharedString::from(status));
                        }
                    });
                }
                Err(e) => {
                    let msg = format!("❌ Falha ao analisar os arquivos: {}", e);
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(w) = weak_async.upgrade() {
                            w.set_is_importing_tray(false);
                            w.set_tray_status(SharedString::from(msg));
                        }
                    });
                }
            }
        });
    });

    // Callback: start_tray_import
    // Copia os arquivos de Tray para a pasta Tray e o CC para
    // Mods/Imported_Sims/<nome>. Antes só trocava o texto do status.
    let cfg_clone = Arc::clone(&config_mgr);
    let state_clone = Arc::clone(&state);
    let weak_clone = weak_win.clone();
    window.on_start_tray_import(move || {
        let config = cfg_clone.load();
        let Some(sims_path) = config.sims4_path.clone() else {
            if let Some(w) = weak_clone.upgrade() {
                w.set_tray_status(SharedString::from("⚠️ Configure a pasta do jogo antes de importar."));
            }
            return;
        };

        let candidates = state_clone.take_tray_candidates();
        if candidates.is_empty() {
            if let Some(w) = weak_clone.upgrade() {
                w.set_tray_status(SharedString::from("⚠️ Nenhum item analisado na fila. Selecione arquivos primeiro."));
            }
            return;
        }

        let tray_dir = get_tray_dir(&sims_path);
        let mods_dir = get_mods_dir(&sims_path);
        let allowed_roots = vec![tray_dir.clone(), mods_dir.clone()];
        let work_dir = state_clone.tray_work_dir().to_path_buf();

        if let Some(w) = weak_clone.upgrade() {
            w.set_is_importing_tray(true);
            w.set_tray_status(SharedString::from("📥 Importando Sims, Lotes e CC..."));
        }

        let weak_async = weak_clone.clone();
        let state_async = Arc::clone(&state_clone);
        tokio::task::spawn_blocking(move || {
            let mut tray_total = 0;
            let mut cc_total = 0;
            let mut skipped_total = 0;
            let mut failures: Vec<String> = Vec::new();

            for candidate in &candidates {
                match import_tray_item(&candidate.analysis, &tray_dir, &mods_dir, &allowed_roots) {
                    Ok((tray_n, cc_n, skipped)) => {
                        tray_total += tray_n;
                        cc_total += cc_n;
                        skipped_total += skipped;
                    }
                    // `import_tray_item` reverte a própria cópia; seguimos com
                    // os demais itens.
                    Err(e) => failures.push(format!("{} ({})", candidate.analysis.detected_name, e)),
                }
            }

            let _ = clear_tray_work_dir(&work_dir);
            state_async.clear_tray();

            let mut msg = format!(
                "✅ Importação concluída: {} arquivo(s) de Tray e {} de CC.",
                tray_total, cc_total
            );
            if skipped_total > 0 {
                msg.push_str(&format!(" {} CC já instalado(s) foram ignorados.", skipped_total));
            }
            if !failures.is_empty() {
                msg.push_str(&format!(" ⚠️ Falhas: {}", failures.join(", ")));
            }

            let _ = slint::invoke_from_event_loop(move || {
                if let Some(w) = weak_async.upgrade() {
                    w.set_is_importing_tray(false);
                    w.set_tray_items(ModelRc::from(Rc::new(VecModel::default())));
                    w.set_tray_status(SharedString::from(msg));
                }
            });
        });
    });

    // Callback: select_translation_file
    let cfg_clone = Arc::clone(&config_mgr);
    let weak_clone = weak_win.clone();
    window.on_select_translation_file(move || {
        let config = cfg_clone.load();
        if let Some(sims_path) = &config.sims4_path {
            let mods_dir = get_mods_dir(sims_path);
            let trans_dir = mods_dir.join("01_Traducoes");
            let _ = fs::create_dir_all(&trans_dir);

            if let Some(file) = rfd::FileDialog::new()
                .add_filter("Translation Packages", &["package", "zip", "7z"])
                .pick_file()
            {
                let dest = crate::core::safety::unique_dest_path(
                    &trans_dir,
                    file.file_name().unwrap_or_default().to_str().unwrap_or("traducao.package"),
                );
                let _ = fs::copy(&file, dest);
                refresh_translations_list(&weak_clone, &cfg_clone);
            }
        }
    });

    // Callback: delete_translation
    let cfg_clone = Arc::clone(&config_mgr);
    let weak_clone = weak_win.clone();
    window.on_delete_translation(move |name| {
        let config = cfg_clone.load();
        if let Some(sims_path) = &config.sims4_path {
            let target = get_mods_dir(sims_path).join("01_Traducoes").join(name.as_str());
            if target.exists() {
                let _ = fs::remove_file(target);
                refresh_translations_list(&weak_clone, &cfg_clone);
            }
        }
    });

    // Callback: install_reshade
    // O download do setup são dezenas de MB por HTTP: em `spawn_local` isso
    // rodava na thread da UI e congelava a janela inteira.
    let cfg_clone = Arc::clone(&config_mgr);
    let weak_clone = weak_win.clone();
    window.on_install_reshade(move || {
        let config = cfg_clone.load();
        let Some(sims_path) = config.sims4_path.clone() else {
            if let Some(w) = weak_clone.upgrade() {
                w.set_reshade_status(SharedString::from("⚠️ Configure a pasta do jogo antes de instalar o ReShade."));
            }
            return;
        };
        let game_bin = game_bin_dir(&sims_path);

        if let Some(w) = weak_clone.upgrade() {
            w.set_is_reshade_busy(true);
            w.set_reshade_status(SharedString::from("🌐 Baixando e injetando ReShade (5.9.2)..."));
        }

        let weak_async = weak_clone.clone();
        let cfg_async = Arc::clone(&cfg_clone);
        tokio::task::spawn_blocking(move || {
            let msg = match install_reshade(&game_bin, None, false) {
                Ok(()) => "✅ ReShade (5.9.2) instalado com sucesso!".to_string(),
                Err(e) => format!("❌ Erro ao instalar ReShade: {}", e),
            };
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(w) = weak_async.upgrade() {
                    w.set_is_reshade_busy(false);
                    w.set_reshade_status(SharedString::from(msg));
                }
                refresh_reshade_env(&weak_async, &cfg_async);
            });
        });
    });

    // Callback: uninstall_reshade
    let cfg_clone = Arc::clone(&config_mgr);
    let weak_clone = weak_win.clone();
    window.on_uninstall_reshade(move || {
        let config = cfg_clone.load();
        let Some(sims_path) = config.sims4_path.clone() else {
            return;
        };
        let game_bin = game_bin_dir(&sims_path);
        let allowed_roots = vec![game_bin.clone()];

        let msg = match uninstall_reshade(&game_bin, &allowed_roots) {
            Ok(()) => "🗑️ ReShade desinstalado e backups restaurados.".to_string(),
            Err(e) => format!("❌ Falha ao desinstalar: {}", e),
        };

        if let Some(w) = weak_clone.upgrade() {
            w.set_reshade_status(SharedString::from(msg));
        }
        refresh_reshade_env(&weak_clone, &cfg_clone);
    });

    // Callback: install_preset
    let cfg_clone = Arc::clone(&config_mgr);
    let weak_clone = weak_win.clone();
    window.on_install_preset(move || {
        let config = cfg_clone.load();
        let Some(sims_path) = config.sims4_path.clone() else {
            return;
        };
        let game_bin = game_bin_dir(&sims_path);

        let Some(file) = rfd::FileDialog::new()
            .add_filter("Preset Zip", &["zip"])
            .pick_file()
        else {
            return;
        };

        let msg = match install_shader_preset(&game_bin, &file) {
            Ok(()) => "📦 Preset de gráficos instalado na pasta presets/.".to_string(),
            Err(e) => format!("❌ Não foi possível instalar o preset: {}", e),
        };

        if let Some(w) = weak_clone.upgrade() {
            w.set_reshade_status(SharedString::from(msg));
        }
        refresh_reshade_env(&weak_clone, &cfg_clone);
    });

    // Callback: copy_reshade_command
    let cfg_clone = Arc::clone(&config_mgr);
    let weak_clone = weak_win.clone();
    window.on_copy_reshade_command(move || {
        let config = cfg_clone.load();
        if let Some(sims_path) = &config.sims4_path {
            let game_bin = sims_path.join("Game").join("Bin");
            let (_, cmd) = detect_environment(&game_bin);
            if let Some(w) = weak_clone.upgrade() {
                w.set_reshade_status(SharedString::from(format!("📋 Comando de inicialização: {}", cmd)));
            }
        }
    });

    // Callback: select_config_game_path
    let cfg_clone = Arc::clone(&config_mgr);
    let weak_clone = weak_win.clone();
    window.on_select_config_game_path(move || {
        if let Some(dir) = rfd::FileDialog::new().pick_folder() {
            let mut cfg = cfg_clone.load();
            cfg.sims4_path = Some(dir.clone());
            let _ = cfg_clone.save(&cfg);
            if let Some(w) = weak_clone.upgrade() {
                w.set_game_path(SharedString::from(dir.display().to_string()));
                w.set_config_status(SharedString::from("✅ Caminho do jogo atualizado!"));
            }
            refresh_dashboard_stats(&weak_clone, &cfg_clone);
        }
    });

    // Callback: autodetect_game_path
    let cfg_clone = Arc::clone(&config_mgr);
    let weak_clone = weak_win.clone();
    window.on_autodetect_game_path(move || {
        let detected = cfg_clone.auto_detect_sims4_path();
        if let Some(p) = detected {
            let mut cfg = cfg_clone.load();
            cfg.sims4_path = Some(p.clone());
            let _ = cfg_clone.save(&cfg);
            if let Some(w) = weak_clone.upgrade() {
                w.set_game_path(SharedString::from(p.display().to_string()));
                w.set_config_status(SharedString::from("✅ Caminho do The Sims 4 encontrado e salvo!"));
            }
            refresh_dashboard_stats(&weak_clone, &cfg_clone);
        } else if let Some(w) = weak_clone.upgrade() {
            w.set_config_status(SharedString::from("⚠️ Não foi possível localizar a pasta do The Sims 4. Selecione manualmente."));
        }
    });

    // Callback: language_selected
    // A troca é imediata: I18n.lang alimenta os bindings que chamam tr().
    let cfg_clone = Arc::clone(&config_mgr);
    let weak_clone = weak_win.clone();
    window.on_language_selected(move |lang| {
        let mut cfg = cfg_clone.load();
        cfg.language = lang.to_string();
        let saved = cfg_clone.save(&cfg).is_ok();

        if let Some(w) = weak_clone.upgrade() {
            w.global::<crate::I18n>().set_lang(lang.clone());
            w.set_active_language(lang);
            w.set_config_status(SharedString::from(if saved {
                t("✅ Configurações salvas!", &cfg.language)
            } else {
                "⚠️ Idioma alterado, mas não foi possível gravar o arquivo de configuração.".to_string()
            }));
        }
    });

    // Callback: theme_selected
    let cfg_clone = Arc::clone(&config_mgr);
    let weak_clone = weak_win.clone();
    window.on_theme_selected(move |idx| {
        let mut cfg = cfg_clone.load();
        cfg.theme = theme_name_from_index(idx).to_string();
        let saved = cfg_clone.save(&cfg).is_ok();

        if let Some(w) = weak_clone.upgrade() {
            if !saved {
                w.set_config_status(SharedString::from(
                    "⚠️ Tema aplicado, mas não foi possível gravá-lo no arquivo de configuração.",
                ));
            }
        }
    });

    // Callback: merge_limit_selected
    let cfg_clone = Arc::clone(&config_mgr);
    let weak_clone = weak_win.clone();
    window.on_merge_limit_selected(move |gb| {
        let Ok(value) = gb.parse::<f64>() else { return };
        let mut cfg = cfg_clone.load();
        cfg.merge_limit_gb = value;
        let _ = cfg_clone.save(&cfg);

        if let Some(w) = weak_clone.upgrade() {
            w.set_merge_max_size(gb);
            w.set_config_status(SharedString::from(format!(
                "Limite de {} GB por parte gerada pelo merger.",
                value
            )));
        }
    });

    // Callback: save_config
    let cfg_clone = Arc::clone(&config_mgr);
    let weak_clone = weak_win.clone();
    window.on_save_config(move || {
        let cfg = cfg_clone.load();
        let msg = match cfg_clone.save(&cfg) {
            Ok(()) => t("✅ Configurações salvas!", &cfg.language),
            Err(e) => format!("❌ Não foi possível salvar: {}", e),
        };
        if let Some(w) = weak_clone.upgrade() {
            w.set_config_status(SharedString::from(msg));
        }
    });

    // Reflete na UI o que estava salvo no config.
    apply_config_to_ui(&weak_win, &config_mgr);
}

const THEME_NAMES: [&str; 4] = ["Sims Green", "Deep Blue", "Cyber Purple", "Neon Pink"];

fn theme_name_from_index(idx: i32) -> &'static str {
    THEME_NAMES.get(idx as usize).copied().unwrap_or(THEME_NAMES[0])
}

fn theme_index_from_name(name: &str) -> i32 {
    THEME_NAMES.iter().position(|n| *n == name).unwrap_or(0) as i32
}

/// Carrega idioma, tema e limite do merger do arquivo de configuração para a UI.
fn apply_config_to_ui(weak_win: &slint::Weak<MainWindow>, config_mgr: &ConfigManager) {
    let cfg = config_mgr.load();
    if let Some(w) = weak_win.upgrade() {
        w.set_active_language(SharedString::from(cfg.language.clone()));
        w.global::<crate::I18n>().set_lang(SharedString::from(cfg.language));
        w.global::<crate::Theme>().set_active_theme(theme_index_from_name(&cfg.theme));
        w.set_merge_max_size(SharedString::from(format!("{}", cfg.merge_limit_gb)));
    }
}

fn to_string_model(items: Vec<String>) -> ModelRc<SharedString> {
    let model = Rc::new(VecModel::default());
    for item in items {
        model.push(SharedString::from(item));
    }
    ModelRc::from(model)
}

fn file_label(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string())
}

/// Devolve o instalador ao estado ocioso mostrando o motivo da recusa.
fn set_installer_error(weak_win: &slint::Weak<MainWindow>, message: &str) {
    if let Some(w) = weak_win.upgrade() {
        w.set_is_installing(false);
        w.set_installer_status(SharedString::from(message));
    }
}

fn refresh_dashboard_stats(weak_win: &slint::Weak<MainWindow>, config_mgr: &ConfigManager) {
    let config = config_mgr.load();
    if let Some(w) = weak_win.upgrade() {
        if let Some(sims_path) = config.sims4_path.clone() {
            w.set_game_path(SharedString::from(sims_path.display().to_string()));
            let weak_async = weak_win.clone();
            
            tokio::task::spawn_blocking(move || {
                let mods_dir = get_mods_dir(&sims_path);
                let tray_dir = get_tray_dir(&sims_path);

                let mut mods_count = 0;
                let mut scripts_count = 0;
                let mut mods_size = 0u64;

                if mods_dir.exists() {
                    for entry in walkdir::WalkDir::new(&mods_dir).into_iter().filter_map(|e| e.ok()) {
                        if entry.path().is_file() {
                            let ext = entry.path().extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
                            if ext == "package" {
                                mods_count += 1;
                                mods_size += entry.metadata().map(|m| m.len()).unwrap_or(0);
                            } else if ext == "ts4script" {
                                scripts_count += 1;
                                mods_size += entry.metadata().map(|m| m.len()).unwrap_or(0);
                            }
                        }
                    }
                }

                let mut tray_count = 0;
                if tray_dir.exists() {
                    if let Ok(entries) = fs::read_dir(&tray_dir) {
                        tray_count = entries.count();
                    }
                }

                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(w) = weak_async.upgrade() {
                        w.set_total_mods_count(SharedString::from(mods_count.to_string()));
                        w.set_total_scripts_count(SharedString::from(scripts_count.to_string()));
                        w.set_total_mods_size(SharedString::from(crate::bridge::slint_models::format_size_bytes(mods_size)));
                        w.set_total_tray_count(SharedString::from(tray_count.to_string()));
                        w.set_dashboard_status(SharedString::from("Estatísticas atualizadas."));
                    }
                });
            });
        } else {
            w.set_game_path(SharedString::from("Não configurado"));
            w.set_dashboard_status(SharedString::from("⚠️ Configure a pasta do jogo nas Configurações."));
        }
    }
}

fn refresh_organizer_tree(weak_win: &slint::Weak<MainWindow>, config_mgr: &ConfigManager) {
    let config = config_mgr.load();
    if let Some(w) = weak_win.upgrade() {
        if let Some(sims_path) = &config.sims4_path {
            let mods_dir = get_mods_dir(sims_path);
            let nodes = scan_mods_tree(&mods_dir);
            let model = Rc::new(VecModel::default());
            for node in nodes {
                model.push(crate::ModTreeEntry {
                    name: SharedString::from(node.name),
                    is_dir: node.is_dir,
                    size_str: SharedString::from(crate::bridge::slint_models::format_size_bytes(node.size)),
                    disabled: node.disabled,
                    path_str: SharedString::from(node.path.display().to_string()),
                });
            }
            w.set_mod_entries(ModelRc::from(model));
            w.set_organizer_status(SharedString::from("Árvore de mods atualizada."));
        }
    }
}

/// Pasta dos binários do jogo, onde a DLL do ReShade é injetada.
fn game_bin_dir(sims4_path: &Path) -> PathBuf {
    sims4_path.join("Game").join("Bin")
}

fn refresh_reshade_env(weak_win: &slint::Weak<MainWindow>, config_mgr: &ConfigManager) {
    let config = config_mgr.load();
    if let Some(w) = weak_win.upgrade() {
        if let Some(sims_path) = &config.sims4_path {
            let game_bin = game_bin_dir(sims_path);
            let (env_type, env_cmd) = detect_environment(&game_bin);
            let env_str = match env_type {
                crate::engine::reshade::LinuxEnvType::Steam => "Steam (Proton)",
                crate::engine::reshade::LinuxEnvType::LutrisBottles => "Lutris / Bottles",
                crate::engine::reshade::LinuxEnvType::Generic => "Generic Wine",
            };
            w.set_reshade_env_type(SharedString::from(env_str));
            w.set_reshade_env_command(SharedString::from(env_cmd));

            // Estado real em disco: até agora `is_reshade_installed` nunca era
            // preenchido, então a tela sempre dizia "não instalado".
            let status = detect_installation(&game_bin);
            w.set_is_reshade_installed(status.installed);
            w.set_reshade_injected_dll(SharedString::from(
                status.injected_dll.clone().unwrap_or_default(),
            ));
            w.set_reshade_presets_count(SharedString::from(status.presets_count.to_string()));
        }
    }
}

fn refresh_translations_list(weak_win: &slint::Weak<MainWindow>, config_mgr: &ConfigManager) {
    let config = config_mgr.load();
    if let Some(w) = weak_win.upgrade() {
        if let Some(sims_path) = &config.sims4_path {
            let trans_dir = get_mods_dir(sims_path).join("01_Traducoes");
            let model = Rc::new(VecModel::default());
            if trans_dir.exists() {
                if let Ok(entries) = fs::read_dir(&trans_dir) {
                    for entry in entries.filter_map(|e| e.ok()) {
                        if entry.path().is_file() {
                            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                            model.push(crate::TranslationItem {
                                name: SharedString::from(entry.file_name().to_string_lossy().to_string()),
                                size_str: SharedString::from(crate::bridge::slint_models::format_size_bytes(size)),
                            });
                        }
                    }
                }
            }
            w.set_installed_translations(ModelRc::from(model));
            w.set_translations_status(SharedString::from("Lista de traduções atualizada."));
        }
    }
}

fn sims4_path_cache(sims_path: &Path) -> Vec<PathBuf> {
    vec![
        sims_path.join("localthumbcache.package"),
        sims_path.join("spotlight_thumbnails.package"),
        sims_path.join("cache"),
        sims_path.join("cachestr"),
    ]
}

fn create_zip_archive(src_dir: &Path, zip_path: &Path) -> io::Result<()> {
    let file = File::create(zip_path)?;
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    for entry in walkdir::WalkDir::new(src_dir).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        let name = path.strip_prefix(src_dir).unwrap_or(path);

        if path.is_file() {
            zip.start_file(name.to_string_lossy().to_string(), options)?;
            let mut f = File::open(path)?;
            let mut buffer = Vec::new();
            f.read_to_end(&mut buffer)?;
            zip.write_all(&buffer)?;
        }
    }
    zip.finish()?;
    Ok(())
}
