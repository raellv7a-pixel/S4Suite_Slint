//! Instalação e gerência das traduções de mods.
//!
//! No app PyQt a aba de traduções chamava o **mesmo** `install_mod()` do
//! instalador, só apontando o destino para `01_Traducoes`
//! (`legacy/ui/translations_tab.py:166`). Este módulo mantém essa decisão: uma
//! tradução é um mod como outro qualquer, e reaproveitar o instalador traz de
//! graça a varredura de segurança, a detecção de duplicata, o backup do arquivo
//! substituído e o rollback. O que sobra aqui é só o que é específico de
//! tradução — a pasta de destino e a listagem.

use crate::core::safety::{safe_remove_dir_all, safe_remove_file, SafetyError};
use crate::engine::installer::{
    calculate_conflicts, clear_staging, prepare_staging, ConflictType, InstallerError,
    STAGING_DIR_NAME,
};
use std::fs;
use std::path::{Path, PathBuf};

/// Pasta das traduções dentro de `Mods`, mesma convenção do app PyQt.
pub const TRANSLATIONS_DIR: &str = "01_Traducoes";

pub fn translations_dir(mods_dir: &Path) -> PathBuf {
    mods_dir.join(TRANSLATIONS_DIR)
}

/// Uma tradução já instalada, como aparece na lista da UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledTranslation {
    pub name: String,
    pub path: PathBuf,
    pub size: u64,
}

/// O que a instalação fez, para virar mensagem de status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranslationInstallReport {
    pub installed: usize,
    pub skipped: usize,
    pub updated: usize,
}

/// Instala (ou atualiza) uma tradução a partir de um `.package` avulso ou de um
/// `.zip`/`.7z`/`.rar`.
///
/// `mods_dir` é a pasta `Mods` inteira, não a de traduções: a busca por uma
/// cópia já instalada precisa enxergar a árvore toda, senão a mesma tradução
/// guardada fora de `01_Traducoes` seria instalada de novo em vez de
/// atualizada. Só o que é **novo** aterrissa em `01_Traducoes`.
pub fn install_translation(
    source: &Path,
    mods_dir: &Path,
    allowed_roots: &[PathBuf],
) -> Result<TranslationInstallReport, InstallerError> {
    let staging_dir = mods_dir.join(STAGING_DIR_NAME);

    // O staging é sempre removido: em erro ele carrega os arquivos extraídos
    // pela metade, e ele mora dentro de Mods, onde o jogo iria lê-los.
    let outcome = install_into_staging(source, mods_dir, &staging_dir, allowed_roots);
    let _ = clear_staging(&staging_dir);
    outcome
}

fn install_into_staging(
    source: &Path,
    mods_dir: &Path,
    staging_dir: &Path,
    allowed_roots: &[PathBuf],
) -> Result<TranslationInstallReport, InstallerError> {
    fs::create_dir_all(translations_dir(mods_dir))?;

    prepare_staging(std::slice::from_ref(&source.to_path_buf()), staging_dir)?;
    let report = calculate_conflicts(staging_dir, mods_dir)?;

    let updated = report
        .staged_files
        .iter()
        .filter(|f| f.conflict == ConflictType::SizeDiff)
        .count();

    let (installed, skipped) = crate::engine::installer::execute_installation(
        &report,
        mods_dir,
        TRANSLATIONS_DIR,
        allowed_roots,
    )?;

    Ok(TranslationInstallReport {
        installed,
        skipped,
        updated,
    })
}

/// Lista o que está em `01_Traducoes`, em ordem alfabética.
///
/// Subpastas são listadas como uma entrada só, com o tamanho somado: uma
/// tradução distribuída em vários `.package` chega assim, e quebrá-la em linhas
/// soltas na UI tiraria do usuário a noção do que é um pacote.
pub fn list_installed(mods_dir: &Path) -> Vec<InstalledTranslation> {
    let dir = translations_dir(mods_dir);
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };

    let mut items: Vec<InstalledTranslation> = entries
        .filter_map(|e| e.ok())
        .map(|entry| {
            let path = entry.path();
            let size = if path.is_dir() {
                dir_size(&path)
            } else {
                entry.metadata().map(|m| m.len()).unwrap_or(0)
            };
            InstalledTranslation {
                name: entry.file_name().to_string_lossy().to_string(),
                path,
                size,
            }
        })
        .collect();

    items.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    items
}

fn dir_size(dir: &Path) -> u64 {
    walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| e.metadata().ok())
        .map(|m| m.len())
        .sum()
}

/// Remove uma tradução pelo nome exibido na lista.
///
/// O nome vem da UI, então nunca é concatenado direto: resolvemos contra a
/// listagem real e a remoção passa por `core::safety`. Sem isso, um nome como
/// `../../algo` sairia de `Mods`.
pub fn remove_translation(
    mods_dir: &Path,
    name: &str,
    allowed_roots: &[PathBuf],
) -> Result<(), SafetyError> {
    let target = list_installed(mods_dir)
        .into_iter()
        .find(|item| item.name == name)
        .ok_or_else(|| SafetyError::InvalidPath(name.to_string()))?;

    if target.path.is_dir() {
        safe_remove_dir_all(&target.path, allowed_roots)
    } else {
        safe_remove_file(&target.path, allowed_roots)
    }
}
