pub mod dbpf;
pub mod disabled;
pub mod installer;
pub mod merger;
pub mod organizer;
pub mod reshade;
pub mod tray;

use std::path::Path;
use walkdir::{DirEntry, WalkDir};

/// Diretórios de trabalho que a S4Suite cria *dentro* de `Mods`.
///
/// Eles contêm cópias de arquivos que também existem legitimamente na pasta do
/// usuário. Qualquer varredura que os inclua produz resultados errados:
/// duplicatas fantasma, conflitos falsos, ou remoção de backups. Percorra a
/// árvore de mods sempre por [`walk_user_mods`].
pub const INTERNAL_WORK_DIRS: [&str; 3] = [
    installer::STAGING_DIR_NAME,
    installer::BACKUP_DIR_NAME,
    ".s4suite_merge_backup",
];

fn is_internal_work_dir(entry: &DirEntry) -> bool {
    entry.file_type().is_dir()
        && INTERNAL_WORK_DIRS.contains(&entry.file_name().to_string_lossy().as_ref())
}

/// Percorre apenas o conteúdo real de mods, pulando as pastas internas.
pub fn walk_user_mods(root: &Path, max_depth: usize) -> impl Iterator<Item = DirEntry> {
    WalkDir::new(root)
        .max_depth(max_depth)
        .into_iter()
        .filter_entry(|e| !is_internal_work_dir(e))
        .filter_map(|e| e.ok())
}
