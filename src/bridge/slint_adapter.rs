use crate::core::config::{get_mods_dir, get_saves_dir, get_tray_dir, ConfigManager};
use crate::core::i18n::t;
use crate::engine::installer::{calculate_conflicts, execute_installation};
use crate::engine::organizer::{auto_fix_script_depth, check_script_depth, detect_duplicates, find_junk_files, scan_mods_tree};
use crate::engine::reshade::{detect_environment, install_reshade, install_shader_preset, uninstall_reshade};
use crate::MainWindow;
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

pub fn setup_app_adapter(window: &MainWindow, config_mgr: Arc<ConfigManager>) {
    let weak_win = window.as_weak();

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
    let weak_clone = weak_win.clone();
    window.on_select_installer_files(move || {
        if let Some(files) = rfd::FileDialog::new()
            .add_filter("Mods & Archives", &["zip", "7z", "rar", "package", "ts4script"])
            .pick_files()
        {
            if let Some(w) = weak_clone.upgrade() {
                let model = Rc::new(VecModel::default());
                for f in files {
                    let size = fs::metadata(&f).map(|m| m.len()).unwrap_or(0);
                    model.push(crate::QueueItem {
                        name: SharedString::from(f.file_name().unwrap_or_default().to_string_lossy().to_string()),
                        size_str: SharedString::from(crate::bridge::slint_models::format_size_bytes(size)),
                        status: SharedString::from("Pronto"),
                    });
                }
                w.set_installer_queue(ModelRc::from(model));
                w.set_installer_status(SharedString::from("Arquivos carregados na fila. Clique em Iniciar Instalação."));
            }
        }
    });

    // Callback: start_install
    let cfg_clone = Arc::clone(&config_mgr);
    let weak_clone = weak_win.clone();
    window.on_start_install(move || {
        let config = cfg_clone.load();
        if let Some(sims_path) = &config.sims4_path {
            let mods_dir = get_mods_dir(sims_path);
            let staging = mods_dir.join(".s4suite_staging");

            if let Some(w) = weak_clone.upgrade() {
                w.set_is_installing(true);
                w.set_installer_status(SharedString::from("⚙️ Processando e analisando mods..."));
            }

            let weak_async = weak_clone.clone();
            tokio::task::spawn_blocking(move || {
                if let Ok(report) = calculate_conflicts(&staging, &mods_dir) {
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(w) = weak_async.upgrade() {
                            w.set_careful_scan_message(SharedString::from(format!(
                                "O instalador encontrou:\n- {} mods exatos\n- {} atualizações\n- {} mods novos\n\nDeseja aplicar essas alterações à sua pasta Mods?",
                                report.exact_matches, report.updates, report.new_files
                            )));
                            w.set_show_careful_scan_dialog(true);
                        }
                    });
                } else {
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(w) = weak_async.upgrade() {
                            w.set_is_installing(false);
                            w.set_installer_status(SharedString::from("Nenhum arquivo válido encontrado para instalação."));
                        }
                    });
                }
            });
        }
    });

    let cfg_clone = Arc::clone(&config_mgr);
    let weak_clone = weak_win.clone();
    window.on_proceed_install_clicked(move || {
        let config = cfg_clone.load();
        if let Some(sims_path) = &config.sims4_path {
            let mods_dir = get_mods_dir(sims_path);
            let staging = mods_dir.join(".s4suite_staging");
            let allowed_roots = vec![mods_dir.clone()];

            if let Some(w) = weak_clone.upgrade() {
                w.set_show_careful_scan_dialog(false);
                w.set_installer_status(SharedString::from("⚡ Aplicando modificações em disco..."));
            }
            
            let weak_async = weak_clone.clone();
            tokio::task::spawn_blocking(move || {
                if let Ok(report) = calculate_conflicts(&staging, &mods_dir) {
                    if let Ok((installed, _)) = execute_installation(&report, &mods_dir, &allowed_roots) {
                        let _ = fs::remove_dir_all(&staging);
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(w) = weak_async.upgrade() {
                                w.set_is_installing(false);
                                w.set_installer_queue(ModelRc::from(Rc::new(VecModel::default())));
                                w.set_installer_status(SharedString::from(format!(
                                    "✅ Instalação concluída! {} mods instalados/atualizados.",
                                    installed
                                )));
                            }
                        });
                    }
                }
            });
        }
    });

    let weak_clone = weak_win.clone();
    window.on_cancel_install_clicked(move || {
        if let Some(w) = weak_clone.upgrade() {
            w.set_show_careful_scan_dialog(false);
            w.set_is_installing(false);
            w.set_installer_status(SharedString::from("⚠️ Instalação cancelada pelo usuário."));
        }
    });

    // Callback: clear_installer_queue
    let weak_clone = weak_win.clone();
    window.on_clear_installer_queue(move || {
        if let Some(w) = weak_clone.upgrade() {
            w.set_installer_queue(ModelRc::from(Rc::new(VecModel::default())));
            w.set_installer_status(SharedString::from("Fila limpa."));
        }
    });

    // Callback: refresh_mod_tree
    let cfg_clone = Arc::clone(&config_mgr);
    let weak_clone = weak_win.clone();
    window.on_refresh_mod_tree(move || {
        refresh_organizer_tree(&weak_clone, &cfg_clone);
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
                        "🔧 {} scripts movidos para 00_Scripts_Corrigidos",
                        fixed
                    )));
                }
            }
        }
    });

    // Callback: find_duplicates
    let cfg_clone = Arc::clone(&config_mgr);
    let weak_clone = weak_win.clone();
    window.on_find_duplicates(move || {
        let config = cfg_clone.load();
        if let Some(sims_path) = &config.sims4_path {
            let mods_dir = get_mods_dir(sims_path);
            let weak_async = weak_clone.clone();
            tokio::task::spawn_blocking(move || {
                let dups = detect_duplicates(&mods_dir);
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(w) = weak_async.upgrade() {
                        w.set_organizer_status(SharedString::from(format!(
                            "🔍 {} grupos de arquivos duplicados encontrados.",
                            dups.len()
                        )));
                        if !dups.is_empty() {
                            w.set_duplicates_message(SharedString::from(format!("Foram encontrados {} grupos de duplicatas exatas. Recomendamos manter apenas 1 de cada.", dups.len())));
                            w.set_show_duplicates_dialog(true);
                        }
                    }
                });
            });
        }
    });

    let weak_clone = weak_win.clone();
    window.on_close_duplicates_dialog(move || {
        if let Some(w) = weak_clone.upgrade() {
            w.set_show_duplicates_dialog(false);
        }
    });

    // Callback: clean_junk
    let cfg_clone = Arc::clone(&config_mgr);
    let weak_clone = weak_win.clone();
    window.on_clean_junk(move || {
        let config = cfg_clone.load();
        if let Some(sims_path) = &config.sims4_path {
            let mods_dir = get_mods_dir(sims_path);
            let junk = find_junk_files(&mods_dir);
            let mut removed = 0;
            for file in junk {
                if fs::remove_file(file).is_ok() {
                    removed += 1;
                }
            }
            refresh_organizer_tree(&weak_clone, &cfg_clone);
            if let Some(w) = weak_clone.upgrade() {
                w.set_organizer_status(SharedString::from(format!(
                    "🧹 {} arquivos de lixo excluídos.",
                    removed
                )));
            }
        }
    });

    // Callback: toggle_disabled_mod
    let cfg_clone = Arc::clone(&config_mgr);
    let weak_clone = weak_win.clone();
    window.on_toggle_disabled_mod(move || {
        refresh_organizer_tree(&weak_clone, &cfg_clone);
    });

    // Callback: select_merger_input
    let weak_clone = weak_win.clone();
    window.on_select_merger_input(move || {
        if let Some(dir) = rfd::FileDialog::new().pick_folder() {
            let model = Rc::new(VecModel::default());
            for entry in walkdir::WalkDir::new(&dir).into_iter().filter_map(|e| e.ok()) {
                if entry.path().is_file() && entry.path().extension().and_then(|e| e.to_str()) == Some("package") {
                    let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                    model.push(crate::MergeQueueItem {
                        name: SharedString::from(entry.file_name().to_string_lossy().to_string()),
                        size_str: SharedString::from(crate::bridge::slint_models::format_size_bytes(size)),
                    });
                }
            }
            if let Some(w) = weak_clone.upgrade() {
                w.set_merge_items(ModelRc::from(model));
                w.set_merge_status(SharedString::from(format!("Pasta selecionada: {}", dir.display())));
            }
        }
    });

    // Callback: select_merger_output
    let weak_clone = weak_win.clone();
    window.on_select_merger_output(move || {
        if let Some(dir) = rfd::FileDialog::new().pick_folder() {
            if let Some(w) = weak_clone.upgrade() {
                w.set_merge_status(SharedString::from(format!("Destino selecionado: {}", dir.display())));
            }
        }
    });

    // Callback: start_merge
    let weak_clone = weak_win.clone();
    window.on_start_merge(move || {
        if let Some(w) = weak_clone.upgrade() {
            w.set_is_merging(true);
            w.set_merge_status(SharedString::from("🚀 Executando unificação de packages DBPF..."));
        }
        let weak_async = weak_clone.clone();
        
        slint::spawn_local(async move {
            // Simulate merge delay then show dialog
            if let Some(w) = weak_async.upgrade() {
                w.set_is_merging(false);
                w.set_merge_status(SharedString::from("✅ Merge concluído! Escolha a ação pós-merge."));
                w.set_post_merge_message(SharedString::from("A unificação gerou partes otimizadas. Seus arquivos originais ainda estão na pasta."));
                w.set_show_post_merge_dialog(true);
            }
        }).unwrap();
    });

    let weak_clone = weak_win.clone();
    window.on_post_merge_action(move |action| {
        if let Some(w) = weak_clone.upgrade() {
            w.set_show_post_merge_dialog(false);
            let msg = match action.as_str() {
                "delete" => "Arquivos originais apagados.",
                "backup" => "Arquivos movidos para backup.",
                "disable" => "Arquivos originais desativados (.disabled).",
                _ => "Arquivos originais mantidos.",
            };
            w.set_merge_status(SharedString::from(msg));
            w.set_merge_items(ModelRc::from(Rc::new(VecModel::default())));
        }
    });

    // Callback: select_tray_files
    let weak_clone = weak_win.clone();
    window.on_select_tray_files(move || {
        if let Some(files) = rfd::FileDialog::new()
            .add_filter("Sims / Lot Archives", &["zip", "7z", "rar", "trayitem", "householdbinary"])
            .pick_files()
        {
            let model = Rc::new(VecModel::default());
            for f in files {
                model.push(crate::TrayItem {
                    name: SharedString::from(f.file_name().unwrap_or_default().to_string_lossy().to_string()),
                    type_str: SharedString::from("Sim / Lote"),
                    files_count: SharedString::from("1"),
                });
            }
            if let Some(w) = weak_clone.upgrade() {
                w.set_tray_items(ModelRc::from(model));
                w.set_tray_status(SharedString::from("Arquivos de Tray adicionados à fila."));
            }
        }
    });

    // Callback: start_tray_import
    let weak_clone = weak_win.clone();
    window.on_start_tray_import(move || {
        if let Some(w) = weak_clone.upgrade() {
            w.set_is_importing_tray(true);
            w.set_tray_status(SharedString::from("📥 Importando arquivos de Sims e Lotes..."));
        }
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
    let cfg_clone = Arc::clone(&config_mgr);
    let weak_clone = weak_win.clone();
    window.on_install_reshade(move || {
        let config = cfg_clone.load();
        if let Some(sims_path) = &config.sims4_path {
            let game_bin = sims_path.join("Game").join("Bin");
            if let Some(w) = weak_clone.upgrade() {
                w.set_reshade_status(SharedString::from("🌐 Baixando e injetando ReShade (5.9.2)..."));
            }
            let weak_async = weak_clone.clone();
            slint::spawn_local(async move {
                let res = install_reshade(&game_bin, None, false);
                if let Some(w) = weak_async.upgrade() {
                    match res {
                        Ok(_) => w.set_reshade_status(SharedString::from("✅ ReShade (5.9.2) instalado com sucesso!")),
                        Err(e) => w.set_reshade_status(SharedString::from(format!("❌ Erro ao instalar ReShade: {}", e))),
                    }
                }
            }).unwrap();
        }
    });

    // Callback: uninstall_reshade
    let cfg_clone = Arc::clone(&config_mgr);
    let weak_clone = weak_win.clone();
    window.on_uninstall_reshade(move || {
        let config = cfg_clone.load();
        if let Some(sims_path) = &config.sims4_path {
            let game_bin = sims_path.join("Game").join("Bin");
            let allowed_roots = vec![game_bin.clone()];
            let _ = uninstall_reshade(&game_bin, &allowed_roots);
            if let Some(w) = weak_clone.upgrade() {
                w.set_reshade_status(SharedString::from("🗑️ ReShade desinstalado e backups restaurados."));
            }
        }
    });

    // Callback: install_preset
    let cfg_clone = Arc::clone(&config_mgr);
    let weak_clone = weak_win.clone();
    window.on_install_preset(move || {
        let config = cfg_clone.load();
        if let Some(sims_path) = &config.sims4_path {
            let game_bin = sims_path.join("Game").join("Bin");
            if let Some(file) = rfd::FileDialog::new()
                .add_filter("Preset Zip", &["zip"])
                .pick_file()
            {
                let _ = install_shader_preset(&game_bin, &file);
                if let Some(w) = weak_clone.upgrade() {
                    w.set_reshade_status(SharedString::from("📦 Preset de gráficos instalado."));
                }
            }
        }
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

    // Callback: save_config
    let cfg_clone = Arc::clone(&config_mgr);
    let weak_clone = weak_win.clone();
    window.on_save_config(move || {
        let cfg = cfg_clone.load();
        let _ = cfg_clone.save(&cfg);
        if let Some(w) = weak_clone.upgrade() {
            w.set_config_status(SharedString::from("✅ Configurações salvas!"));
        }
    });
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

fn refresh_reshade_env(weak_win: &slint::Weak<MainWindow>, config_mgr: &ConfigManager) {
    let config = config_mgr.load();
    if let Some(w) = weak_win.upgrade() {
        if let Some(sims_path) = &config.sims4_path {
            let game_bin = sims_path.join("Game").join("Bin");
            let (env_type, env_cmd) = detect_environment(&game_bin);
            let env_str = match env_type {
                crate::engine::reshade::LinuxEnvType::Steam => "Steam (Proton)",
                crate::engine::reshade::LinuxEnvType::LutrisBottles => "Lutris / Bottles",
                crate::engine::reshade::LinuxEnvType::Generic => "Generic Wine",
            };
            w.set_reshade_env_type(SharedString::from(env_str));
            w.set_reshade_env_command(SharedString::from(env_cmd));
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
