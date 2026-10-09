//! Limpeza restrita aos quatro caches conhecidos do diretório de dados do jogo.

use crate::core::safety::{is_safe_to_delete, safe_remove_dir_all, safe_remove_file, SafetyError};
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Default)]
pub struct CacheCleanupReport {
    pub removed: Vec<PathBuf>,
    pub missing: Vec<PathBuf>,
    pub failures: Vec<CacheCleanupFailure>,
}

#[derive(Debug)]
pub struct CacheCleanupFailure {
    pub path: PathBuf,
    pub error: String,
}

/// Valida a raiz e sua pasta `Mods` antes de remover qualquer cache.
///
/// A raiz deve ser um diretório real, não protegido, sem `..` nem links em
/// qualquer ancestral. Somente os dois arquivos e duas pastas listados abaixo
/// são alvos. Ausências e falhas por alvo não interrompem os demais alvos;
/// links, inclusive dangling e reparse points, nunca são alvos autorizados.
/// Links dentro de uma pasta de cache real são desvinculados sem seguir destinos.
///
/// A validação usa a API de segurança e não é atômica com a remoção: as APIs
/// portáveis de `std::fs` não eliminam corridas TOCTOU com mutações concorrentes.
/// `removed` contém apenas remoções concluídas; uma falha recursiva pode deixar
/// uma pasta parcialmente removida e será registrada em `failures`.
pub fn clean_game_cache(game_dir: &Path) -> Result<CacheCleanupReport, SafetyError> {
    if game_dir.as_os_str().is_empty()
        || game_dir
            .components()
            .any(|part| part == Component::ParentDir)
    {
        return Err(SafetyError::InvalidPath(game_dir.display().to_string()));
    }
    let absolute = if game_dir.is_absolute() {
        game_dir.to_path_buf()
    } else {
        std::env::current_dir()?.join(game_dir)
    };
    let parent = absolute
        .parent()
        .ok_or_else(|| SafetyError::ProtectedPath(game_dir.display().to_string()))?;
    // Autorizar a raiz como descendente de seu pai reutiliza a canonicalização
    // estrita e as proteções de caminhos pessoais/sistema, sem removê-la.
    let root = is_safe_to_delete(&absolute, &[parent.to_path_buf()])?;
    if !fs::symlink_metadata(&root)?.is_dir() {
        return Err(SafetyError::InvalidPath(game_dir.display().to_string()));
    }
    let roots = [root.clone()];
    let mods = is_safe_to_delete(&root.join("Mods"), &roots)?;
    if !fs::symlink_metadata(&mods)?.is_dir() {
        return Err(SafetyError::InvalidPath(mods.display().to_string()));
    }

    let mut report = CacheCleanupReport::default();
    for (name, directory) in [
        ("localthumbcache.package", false),
        ("spotlight_thumbnails.package", false),
        ("cache", true),
        ("cachestr", true),
    ] {
        let path = root.join(name);
        let result = match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                report.missing.push(path);
                continue;
            }
            Err(error) => Err(SafetyError::Io(error)),
            Ok(_) if directory => safe_remove_dir_all(&path, &roots),
            Ok(_) => safe_remove_file(&path, &roots),
        };
        match result {
            Ok(()) => report.removed.push(path),
            Err(error) => report.failures.push(CacheCleanupFailure {
                path,
                error: error.to_string(),
            }),
        }
    }
    Ok(report)
}
