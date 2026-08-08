//! Estatísticas da biblioteca de mods exibidas no painel.
//!
//! A varredura é uma só: contar mods, somar tamanho e listar scripts em passes
//! separados leria a árvore inteira três vezes, e uma pasta `Mods` real tem
//! dezenas de milhares de arquivos.

use crate::engine::walk_user_mods;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Rótulo dos arquivos que ficam soltos na raiz de `Mods`.
pub const ROOT_FOLDER_LABEL: &str = "(Pasta Raiz)";

#[derive(Debug, Default, Clone)]
pub struct ModsStats {
    pub mods_count: usize,
    pub scripts_count: usize,
    pub tray_count: usize,
    pub total_size: u64,
    /// Tamanho por pasta de primeiro nível, da maior para a menor.
    pub folder_sizes: Vec<(String, u64)>,
    pub scripts: Vec<PathBuf>,
    pub tray_files: Vec<PathBuf>,
}

pub fn collect_stats(mods_dir: &Path, tray_dir: &Path) -> ModsStats {
    let mut stats = ModsStats::default();
    let mut por_pasta: HashMap<String, u64> = HashMap::new();

    if mods_dir.exists() {
        for entry in walk_user_mods(mods_dir, usize::MAX) {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }

            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_lowercase();
            let is_package = ext == "package";
            let is_script = ext == "ts4script";
            if !is_package && !is_script {
                continue;
            }

            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            stats.total_size += size;

            if is_package {
                stats.mods_count += 1;
            } else {
                stats.scripts_count += 1;
                stats.scripts.push(path.to_path_buf());
            }

            *por_pasta.entry(top_level_folder(path, mods_dir)).or_default() += size;
        }
    }

    // Maior primeiro: o painel existe para responder "o que está ocupando
    // espaço", e a resposta tem que estar na primeira linha.
    stats.folder_sizes = por_pasta.into_iter().collect();
    stats.folder_sizes.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

    if let Ok(entries) = fs::read_dir(tray_dir) {
        stats.tray_files = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_file())
            .collect();
        stats.tray_files.sort();
    }
    stats.tray_count = stats.tray_files.len();

    stats
}

/// Pasta de primeiro nível dentro de `Mods` à qual o arquivo pertence.
fn top_level_folder(file: &Path, mods_dir: &Path) -> String {
    file.strip_prefix(mods_dir)
        .ok()
        .and_then(|rel| rel.parent())
        .and_then(|parent| parent.components().next())
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .unwrap_or_else(|| ROOT_FOLDER_LABEL.to_string())
}
