//! Instalação e gerência das traduções de mods.
//!
//! No app PyQt a aba de traduções chamava o **mesmo** `install_mod()` do
//! instalador, só apontando o destino para `01_Traducoes`
//! (`legacy/ui/translations_tab.py:166`). Este módulo mantém essa decisão: uma
//! tradução é um mod como outro qualquer, e reaproveitar o instalador traz de
//! graça a varredura de segurança, a detecção de duplicata, o backup do arquivo
//! substituído e o rollback. O que sobra aqui é só o que é específico de
//! tradução — a pasta de destino e a listagem.

use crate::core::safety::{is_safe_to_delete, safe_remove_dir_all, safe_remove_file, SafetyError};
use crate::engine::installer::{
    calculate_conflicts, clear_staging, prepare_staging, ConflictReport, ConflictType,
    InstallerError, STAGING_DIR_NAME,
};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

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
    let mut report = calculate_conflicts(staging_dir, mods_dir)?;
    achatar_traducao_avulsa(&mut report, source);

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

/// Um `.package` avulso é a tradução inteira, e não precisa de pasta.
///
/// O staging agrupa cada fonte sob a identidade dela para preservar a estrutura
/// interna de um `.zip`. Num arquivo solto não há estrutura a preservar: a pasta
/// só acrescentava um nível que a lista da aba exibia no lugar do nome do
/// arquivo, e era nela que o botão de excluir batia.
fn achatar_traducao_avulsa(report: &mut ConflictReport, source: &Path) {
    let avulso = source
        .file_name()
        .map(|n| crate::engine::installer::is_valid_mod_file(&n.to_string_lossy()))
        .unwrap_or(false);
    if !avulso {
        return;
    }
    for file in &mut report.staged_files {
        file.rel_path = PathBuf::from(&file.filename);
    }
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

/// Uma exclusão autorizada no preparo, vinculada à raiz e ao item vistos ali.
///
/// Não modifica o disco até `execute`. A UI deve também comparar `mods_dir()`
/// com a configuração atual antes de confirmar: mudar a configuração não muda
/// o destino desta operação. Links, substituições e alterações do alvo invalidam
/// a confirmação, em vez de autorizar uma nova entrada com o mesmo nome.
#[derive(Debug)]
pub struct PendingTranslationRemoval {
    name: String,
    configured_mods_dir: PathBuf,
    mods_dir: PathBuf,
    directory: PathBuf,
    target: PathBuf,
    mods_identity: EntryIdentity,
    directory_identity: EntryIdentity,
    target_fingerprint: TargetFingerprint,
    contents: Vec<(PathBuf, TargetFingerprint)>,
}

#[derive(Debug, PartialEq, Eq)]
struct EntryIdentity {
    created: Option<SystemTime>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

impl EntryIdentity {
    fn read(metadata: &fs::Metadata) -> Result<Self, SafetyError> {
        let created = metadata.created().ok();
        // Sem um identificador de inode, a data de criação é obrigatória.
        #[cfg(not(unix))]
        if created.is_none() {
            return Err(SafetyError::InvalidPath(
                "Não foi possível identificar o alvo com segurança".to_string(),
            ));
        }
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Ok(Self {
            created,
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
struct TargetFingerprint {
    identity: EntryIdentity,
    is_dir: bool,
    len: u64,
    modified: SystemTime,
    #[cfg(unix)]
    changed: (i64, i64),
}

impl TargetFingerprint {
    fn read(metadata: &fs::Metadata) -> Result<Self, SafetyError> {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Ok(Self {
            identity: EntryIdentity::read(metadata)?,
            is_dir: metadata.is_dir(),
            len: metadata.len(),
            modified: metadata.modified()?,
            #[cfg(unix)]
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        })
    }
}

fn real_metadata(path: &Path) -> Result<fs::Metadata, SafetyError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => SafetyError::InvalidPath(path.display().to_string()),
        _ => SafetyError::Io(error),
    })?;
    if metadata.file_type().is_symlink() || (!metadata.is_file() && !metadata.is_dir()) {
        return Err(SafetyError::InvalidPath(path.display().to_string()));
    }
    Ok(metadata)
}

fn translation_contents(
    path: &Path,
    is_dir: bool,
) -> Result<Vec<(PathBuf, TargetFingerprint)>, SafetyError> {
    let mut contents = Vec::new();
    if is_dir {
        for entry in walkdir::WalkDir::new(path).min_depth(1).follow_links(false) {
            let entry = entry.map_err(|error| SafetyError::Io(error.into()))?;
            let metadata = real_metadata(entry.path())?;
            let relative = entry
                .path()
                .strip_prefix(path)
                .map_err(|_| SafetyError::InvalidPath(entry.path().display().to_string()))?;
            contents.push((relative.to_path_buf(), TargetFingerprint::read(&metadata)?));
        }
        contents.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    }
    Ok(contents)
}

impl PendingTranslationRemoval {
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Raiz canônica capturada, para conferir a configuração na confirmação.
    pub fn mods_dir(&self) -> &Path {
        &self.mods_dir
    }

    pub fn execute(&self) -> Result<(), SafetyError> {
        let configured_directory = translations_dir(&self.configured_mods_dir);
        let configured_target = configured_directory.join(
            self.target
                .file_name()
                .ok_or_else(|| SafetyError::InvalidPath(self.target.display().to_string()))?,
        );
        let changed = || SafetyError::ChangedTarget(self.target.display().to_string());

        // Usa os caminhos originais, não só os canônicos: uma pasta substituída
        // por link depois do preparo não pode redirecionar a exclusão.
        let target = is_safe_to_delete(
            &configured_target,
            std::slice::from_ref(&configured_directory),
        )?;
        let mods_dir = dunce::canonicalize(&self.configured_mods_dir)?;
        let directory = dunce::canonicalize(&configured_directory)?;
        if mods_dir != self.mods_dir || directory != self.directory || target != self.target {
            return Err(changed());
        }
        let mods_metadata = real_metadata(&self.configured_mods_dir)?;
        let directory_metadata = real_metadata(&configured_directory)?;
        let target_metadata = real_metadata(&configured_target)?;
        if !mods_metadata.is_dir()
            || !directory_metadata.is_dir()
            || EntryIdentity::read(&mods_metadata)? != self.mods_identity
            || EntryIdentity::read(&directory_metadata)? != self.directory_identity
            || TargetFingerprint::read(&target_metadata)? != self.target_fingerprint
        {
            return Err(changed());
        }
        // Alterações em níveis internos não mudam necessariamente o mtime da
        // pasta selecionada; a autorização inclui também seus descendentes.
        if translation_contents(&configured_target, target_metadata.is_dir())? != self.contents {
            return Err(changed());
        }

        if target_metadata.is_dir() {
            safe_remove_dir_all(&configured_target, std::slice::from_ref(&self.directory))
        } else {
            safe_remove_file(&configured_target, std::slice::from_ref(&self.directory))
        }
    }
}

/// Resolve o nome contra entradas reais, sem confiar em um caminho vindo da UI.
/// O escopo permitido é estritamente o interior da pasta real de traduções.
pub fn prepare_translation_removal(
    mods_dir: &Path,
    name: &str,
) -> Result<PendingTranslationRemoval, SafetyError> {
    let mut components = Path::new(name).components();
    if !matches!(components.next(), Some(std::path::Component::Normal(_)))
        || components.next().is_some()
    {
        return Err(SafetyError::InvalidPath(name.to_string()));
    }
    let configured_mods_dir = if mods_dir.is_absolute() {
        mods_dir.to_path_buf()
    } else {
        std::env::current_dir()?.join(mods_dir)
    };
    let configured_directory = translations_dir(&configured_mods_dir);
    let directory = is_safe_to_delete(
        &configured_directory,
        std::slice::from_ref(&configured_mods_dir),
    )?;
    let mods_metadata = real_metadata(&configured_mods_dir)?;
    let directory_metadata = real_metadata(&configured_directory)?;
    if !mods_metadata.is_dir() || !directory_metadata.is_dir() {
        return Err(SafetyError::InvalidPath(
            configured_directory.display().to_string(),
        ));
    }

    // Não calcula tamanhos da listagem nem aceita aliases de nomes Unicode
    // inválidos: a autorização corresponde a uma entrada exata do diretório.
    let mut selected = None;
    for entry in fs::read_dir(&configured_directory)? {
        let entry = entry?;
        if entry.file_name().to_str() == Some(name) {
            selected = Some(entry.path());
            break;
        }
    }
    let selected = selected.ok_or_else(|| SafetyError::InvalidPath(name.to_string()))?;
    let target = is_safe_to_delete(&selected, std::slice::from_ref(&configured_directory))?;
    if target.parent() != Some(directory.as_path()) {
        return Err(SafetyError::OutsideAllowedScope(
            target.display().to_string(),
            directory.display().to_string(),
        ));
    }
    let metadata = real_metadata(&selected)?;
    let contents = translation_contents(&selected, metadata.is_dir())?;
    Ok(PendingTranslationRemoval {
        name: name.to_string(),
        mods_dir: dunce::canonicalize(&configured_mods_dir)?,
        configured_mods_dir,
        directory,
        target,
        mods_identity: EntryIdentity::read(&mods_metadata)?,
        directory_identity: EntryIdentity::read(&directory_metadata)?,
        target_fingerprint: TargetFingerprint::read(&metadata)?,
        contents,
    })
}

/// Remoção imediata para consumidores sem diálogo de confirmação.
///
/// Mantém o escopo adicional informado pelo chamador; a UI usa a operação
/// pendente para não reconstruir a autorização a partir do nome ao confirmar.
pub fn remove_translation(
    mods_dir: &Path,
    name: &str,
    allowed_roots: &[PathBuf],
) -> Result<(), SafetyError> {
    let pending = prepare_translation_removal(mods_dir, name)?;
    is_safe_to_delete(&pending.target, allowed_roots)?;
    pending.execute()
}
