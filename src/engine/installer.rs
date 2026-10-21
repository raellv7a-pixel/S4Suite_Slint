use crate::core::safety::{
    is_safe_to_delete, path_exists_or_symlink, same_file_identity, unique_dest_path, AnchoredDir,
};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, Read, Seek};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use walkdir::WalkDir;

/// Pasta de trabalho onde os arquivos são extraídos antes de irem para Mods.
pub const STAGING_DIR_NAME: &str = ".s4suite_staging";
/// Cópias de segurança dos arquivos sobrescritos durante uma instalação.
pub const BACKUP_DIR_NAME: &str = ".s4suite_backups";
/// Base gerenciada pela S4Suite dentro de Mods (mesma convenção do app PyQt).
pub const MANAGED_BASE_DIR: &str = "00_Triagem_Novos";

#[derive(Debug, thiserror::Error)]
pub enum InstallerError {
    #[error("Arquivo suspeito ou executável bloqueado por segurança: '{0}'")]
    SecurityBlocked(String),
    #[error(
        "Nenhum arquivo de mod útil (.package ou .ts4script) encontrado no arquivo compactado"
    )]
    NoUsefulFiles,
    #[error("Erro ao descompactar: {0}")]
    ExtractionFailed(String),
    #[error("Erro de I/O: {0}")]
    Io(#[from] io::Error),
    #[error("Falha durante o rollback da instalação: {0}")]
    RollbackFailed(String),
}

/// Normaliza o nome de um mod para servir de identidade de pasta.
/// `"Meu Mod v2.1.zip"` -> `"Meu Mod v2.1"`.
///
/// Esta identidade vira nome de pasta dentro de `Mods`, e o usuário a lê na
/// árvore do organizador e na lista de traduções. Trocar tudo que não fosse
/// ASCII por `_` transformava `"Mod X — Tradução PT-BR"` em
/// `"mod_x___tradu__o_pt_br"`: o mod continuava funcionando, mas ninguém
/// reconhecia o que era. Saem só os caracteres que não podem estar num nome de
/// arquivo.
pub fn install_identity_name(path: &Path) -> String {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("mod");
    let sanitized: String = stem
        .chars()
        .map(|c| {
            if crate::engine::organizer::FORBIDDEN_NAME_CHARS.contains(&c) || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    let trimmed = sanitized
        .trim()
        .trim_matches('_')
        .trim()
        .trim_matches('.')
        .trim();
    if trimmed.is_empty() {
        "mod".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Um arquivo só é instalável se for `.package` ou `.ts4script` e não for
/// resíduo de sistema de arquivos (`._foo`, `.DS_Store`).
pub fn is_valid_mod_file(filename: &str) -> bool {
    if filename.starts_with("._")
        || filename.starts_with('.')
        || filename.ends_with(".s4tmp")
        || filename.eq_ignore_ascii_case(".DS_Store")
    {
        return false;
    }
    let lower = filename.to_lowercase();
    lower.ends_with(".package") || lower.ends_with(".ts4script")
}

fn is_archive(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase()
            .as_str(),
        "zip" | "7z" | "rar"
    )
}

pub fn scan_archive_security(archive_path: &Path) -> Result<(), InstallerError> {
    let blocked_exts = ["exe", "bat", "dll", "cmd", "vbs", "msi", "sh", "scr", "com"];

    if let Ok(file) = File::open(archive_path) {
        if let Ok(mut archive) = zip::ZipArchive::new(file) {
            for i in 0..archive.len() {
                if let Ok(entry) = archive.by_index(i) {
                    let name = entry.name().to_lowercase();
                    for ext in &blocked_exts {
                        if name.ends_with(&format!(".{}", ext)) {
                            return Err(InstallerError::SecurityBlocked(name));
                        }
                    }
                }
            }
            return Ok(());
        }
    }

    // Fallback using 7z l output check
    if let Ok(output) = Command::new("7z").arg("l").arg(archive_path).output() {
        if output.status.success() {
            let listing = String::from_utf8_lossy(&output.stdout).to_lowercase();
            for ext in &blocked_exts {
                if listing.contains(&format!(".{}", ext)) {
                    return Err(InstallerError::SecurityBlocked(format!(
                        "Extensão .{} encontrada",
                        ext
                    )));
                }
            }
        }
    }

    Ok(())
}

pub fn detect_dependencies(package_bytes: &[u8]) -> Vec<String> {
    let mut deps = Vec::new();
    if package_bytes.windows(5).any(|w| w == b"Lot51") {
        deps.push("Lot51 Core Library".to_string());
    }
    if package_bytes
        .windows(12)
        .any(|w| w == b"XML Injector" || w == b"XmlInjector")
    {
        deps.push("XML Injector".to_string());
    }
    if package_bytes.windows(9).any(|w| w == b"Frankosas") {
        deps.push("Frankosas Library".to_string());
    }
    deps
}

/// Extrai um arquivo compactado **após** recusá-lo se contiver executáveis.
///
/// É o caminho para conteúdo do usuário (mods). Componentes que legitimamente
/// embarcam binários — o instalador do ReShade, por exemplo — devem usar
/// [`extract_archive`] e validar o binário por assinatura.
pub fn extract_archive_to_staging(
    archive_path: &Path,
    staging_dir: &Path,
) -> Result<PathBuf, InstallerError> {
    scan_archive_security(archive_path)?;
    extract_archive(archive_path, staging_dir)
}

/// Extração crua, sem o filtro de executáveis.
pub fn extract_archive(archive_path: &Path, staging_dir: &Path) -> Result<PathBuf, InstallerError> {
    let destination = AnchoredDir::create(staging_dir)?;
    let ext = archive_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    if ext == "zip" {
        let mut archive = zip::ZipArchive::new(open_regular_path(archive_path)?)
            .map_err(|e| InstallerError::ExtractionFailed(e.to_string()))?;
        for index in 0..archive.len() {
            let mut entry = archive
                .by_index(index)
                .map_err(|e| InstallerError::ExtractionFailed(e.to_string()))?;
            if entry.is_symlink() {
                return Err(InstallerError::SecurityBlocked(format!(
                    "Link no arquivo compactado: {}",
                    entry.name()
                )));
            }
            let name = PathBuf::from(entry.name());
            let is_directory = entry.is_dir();
            let size = entry.size();
            extract_entry_at(&destination, &name, is_directory, size, &mut entry, || {
                Ok(())
            })?;
        }
        return Ok(staging_dir.to_path_buf());
    }
    if ext == "7z" {
        let mut callback_started = false;
        let result = sevenz_rust::decompress_with_extract_fn(
            open_regular_path(archive_path)?,
            destination.path(),
            |entry, reader, _| {
                callback_started = true;
                if entry.is_anti_item() {
                    return Err(sevenz_rust::Error::io(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "Entrada de exclusão 7z bloqueada",
                    )));
                }
                extract_entry_at(
                    &destination,
                    Path::new(entry.name()),
                    entry.is_directory(),
                    entry.size(),
                    reader,
                    || Ok(()),
                )
                .map_err(sevenz_rust::Error::io)?;
                Ok(true)
            },
        );
        match result {
            Ok(()) => return Ok(staging_dir.to_path_buf()),
            Err(error) if callback_started => {
                return Err(InstallerError::ExtractionFailed(error.to_string()))
            }
            Err(_) => {} // Codec não suportado antes de qualquer entrada: streaming CLI seguro.
        }
    }
    extract_with_7z_stdout(archive_path, &destination)?;
    Ok(staging_dir.to_path_buf())
}

fn extract_entry_at(
    root: &AnchoredDir,
    relative: &Path,
    is_directory: bool,
    expected_size: u64,
    reader: &mut (impl Read + ?Sized),
    finish: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("Caminho de extração bloqueado: {}", relative.display()),
        ));
    }
    let mut parent = root.try_clone()?;
    if let Some(path) = relative.parent() {
        for component in path.components() {
            if let Component::Normal(name) = component {
                parent = parent.create_child(name)?;
            }
        }
    }
    let name = relative
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Entrada sem nome"))?;
    if is_directory {
        parent.create_child(name)?;
        return Ok(());
    }
    let workspace = tempfile::Builder::new()
        .prefix(".s4suite-extract-")
        .tempdir_in(parent.path())?;
    let private = parent.child(workspace.path().file_name().unwrap())?;
    let mut output = private.create_new("payload")?;
    let result = (|| {
        let count = io::copy(reader, &mut output)?;
        output.sync_all()?;
        if count != expected_size {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Entrada extraída incompleta",
            ));
        }
        finish()?;
        private.publish_noreplace("payload", &parent, name)
    })();
    if let Err(error) = result {
        return match workspace.close() {
            Ok(()) => Err(error),
            Err(cleanup) => Err(io::Error::new(
                error.kind(),
                format!("{error}; limpeza de extração: {cleanup}"),
            )),
        };
    }
    if let Err(cleanup) = workspace.close() {
        let rollback = remove_owned_file(&parent, name, &output);
        return Err(io::Error::other(format!(
            "Limpeza de extração: {cleanup}; rollback: {rollback:?}"
        )));
    }
    Ok(())
}

struct ExternalArchiveEntry {
    path: PathBuf,
    size: u64,
    directory: bool,
}

fn parse_7z_listing(listing: &str) -> io::Result<Vec<ExternalArchiveEntry>> {
    let invalid = || {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "Listagem 7z ambígua ou insegura",
        )
    };
    let mut records = Vec::new();
    let mut fields: HashMap<&str, &str> = HashMap::new();
    for line in listing.lines().chain(std::iter::once("")) {
        if line.is_empty() {
            if fields.is_empty() {
                continue;
            }
            let path: &str = fields.get("Path").copied().ok_or_else(invalid)?;
            let path = PathBuf::from(path);
            if path.as_os_str().is_empty()
                || path
                    .components()
                    .any(|c| !matches!(c, Component::Normal(_)))
            {
                return Err(invalid());
            }
            let attributes = fields.get("Attributes").copied().unwrap_or("");
            let mode = fields.get("Mode").copied().unwrap_or("");
            let directory = fields.get("Folder").copied() == Some("+")
                || attributes.starts_with('D')
                || mode.starts_with('d');
            // Links, reparse points, streams e entradas de exclusão não são
            // materializados; atributos de Unix começam com l para symlinks.
            if fields.iter().any(|(key, value)| {
                !value.is_empty()
                    && *value != "-"
                    && matches!(
                        *key,
                        "Symbolic Link"
                            | "Hard Link"
                            | "Links"
                            | "Reparse"
                            | "NTFS AltStream"
                            | "Alternate Stream"
                            | "Anti"
                            | "Deleted"
                            | "Comment"
                    )
            }) || attributes
                .split_whitespace()
                .any(|part| part.starts_with('l'))
                || mode.starts_with('l')
                || fields.get("Encrypted").copied() == Some("+")
            {
                return Err(invalid());
            }
            if !fields.contains_key("Attributes")
                && !fields.contains_key("Folder")
                && mode.is_empty()
            {
                return Err(invalid());
            }
            let size = fields.get("Size").copied().ok_or_else(invalid)?;
            let size = if size.is_empty() && directory {
                0
            } else {
                if size.is_empty() || !size.bytes().all(|byte| byte.is_ascii_digit()) {
                    return Err(invalid());
                }
                size.parse::<u64>().map_err(|_| invalid())?
            };
            records.push(ExternalArchiveEntry {
                path,
                size,
                directory,
            });
            fields.clear();
            continue;
        }
        let (key, value) = line.split_once(" = ").ok_or_else(invalid)?;
        if !matches!(
            key,
            "Path"
                | "Folder"
                | "Size"
                | "Packed Size"
                | "Modified"
                | "Created"
                | "Accessed"
                | "Attributes"
                | "CRC"
                | "Encrypted"
                | "Method"
                | "Block"
                | "Host OS"
                | "Version"
                | "Volume Index"
                | "Offset"
                | "Solid"
                | "Comment"
                | "Characteristics"
                | "NTFS AltStream"
                | "Alternate Stream"
                | "Symbolic Link"
                | "Hard Link"
                | "Anti"
                | "Deleted"
                | "Mode"
                | "User"
                | "Group"
                | "User ID"
                | "Group ID"
                | "Links"
                | "Short Name"
                | "Reparse"
        ) || (fields.is_empty() && key != "Path")
            || fields.insert(key, value).is_some()
        {
            return Err(invalid());
        }
    }
    let mut paths = HashSet::new();
    if records.is_empty() || records.iter().any(|entry| !paths.insert(&entry.path)) {
        return Err(invalid());
    }
    Ok(records)
}

fn extract_with_7z_stdout(
    archive_path: &Path,
    destination: &AnchoredDir,
) -> Result<(), InstallerError> {
    // A listagem e todas as extrações leem o mesmo snapshot privado. Nenhum
    // argumento -o é passado: o extrator externo só entrega bytes por stdout.
    let workspace = tempfile::Builder::new()
        .prefix(".s4suite-archive-")
        .tempdir_in(destination.path())?;
    let private = destination.child(workspace.path().file_name().unwrap())?;
    let mut source = open_regular_path(archive_path)?;
    drop(copy_open_file(
        &mut source,
        &private,
        std::ffi::OsStr::new("archive"),
    )?);
    let snapshot = private.child_process_path().join("archive");
    let result = (|| -> io::Result<()> {
        let listing = Command::new("7z")
            .args(["l", "-slt", "-ba", "-sccUTF-8", "--"])
            .arg(&snapshot)
            .current_dir(private.child_process_path())
            .stdin(Stdio::null())
            .output()?;
        if !listing.status.success() {
            return Err(io::Error::other(format!(
                "7z não conseguiu listar arquivo: {}",
                listing.status
            )));
        }
        let listing = String::from_utf8(listing.stdout)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Listagem 7z não é UTF-8"))?;
        for entry in parse_7z_listing(&listing)? {
            if entry.directory {
                extract_entry_at(destination, &entry.path, true, 0, &mut io::empty(), || {
                    Ok(())
                })?;
                continue;
            }
            let mut child = Command::new("7z")
                .args([
                    "x",
                    "-so",
                    "-spd",
                    "-ssc",
                    "-sccUTF-8",
                    "-bd",
                    "-bb0",
                    "-y",
                    "--",
                ])
                .arg(&snapshot)
                .arg(&entry.path)
                .current_dir(private.child_process_path())
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()?;
            let mut stdout = child
                .stdout
                .take()
                .ok_or_else(|| io::Error::other("7z sem stdout"))?;
            let extracted = extract_entry_at(
                destination,
                &entry.path,
                false,
                entry.size,
                &mut stdout,
                || {
                    let status = child.wait()?;
                    if !status.success() {
                        return Err(io::Error::other(format!(
                            "7z falhou ao extrair entrada: {status}"
                        )));
                    }
                    Ok(())
                },
            );
            drop(stdout);
            if let Err(error) = extracted {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        }
        Ok(())
    })();
    let cleanup = workspace.close();
    match (result, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(InstallerError::Io(error)),
        (result, Err(cleanup)) => Err(InstallerError::RollbackFailed(format!(
            "Extração externa: {result:?}; limpeza do snapshot: {cleanup}"
        ))),
    }
}

/// Resultado da preparação do staging: quantas fontes entraram e quais falharam.
#[derive(Debug, Default)]
pub struct StagingReport {
    pub prepared: usize,
    pub failures: Vec<(String, String)>,
}

/// Materializa em `staging_dir` o conteúdo de todas as fontes selecionadas
/// pelo usuário, cada uma sob um subdiretório com sua identidade.
///
/// Este era o elo faltante do instalador: `calculate_conflicts` sempre leu o
/// staging, mas nada nunca o populava. Aceita tanto arquivos compactados
/// quanto `.package`/`.ts4script` avulsos.
pub fn prepare_staging(
    sources: &[PathBuf],
    staging_dir: &Path,
) -> Result<StagingReport, InstallerError> {
    let parent_path = staging_dir
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Staging sem pai"))?;
    let name = staging_dir
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Staging sem nome"))?;
    let parent = AnchoredDir::create(parent_path)?;
    let _operation = parent.lock_exclusive()?;
    clear_staging_at(&parent, name)?;
    let staging = parent.create_child(name)?;

    let mut report = StagingReport::default();

    for source in sources {
        let base_identity = install_identity_name(source);
        let dest = crate::core::safety::unique_dest_path(staging_dir, &base_identity);
        let label = source
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| source.display().to_string());
        let outcome = if is_archive(source) {
            extract_archive_to_staging(source, &dest).map(|_| ())
        } else {
            copy_loose_mod_to_staging(source, &dest)
        };

        match outcome {
            Ok(()) => report.prepared += 1,
            Err(e) => {
                // Uma fonte ruim não pode abortar o lote inteiro: descartamos
                // o que ela deixou pela metade e seguimos com as demais.
                let cleanup = staging.remove_tree(dest.file_name().unwrap());
                if let Err(cleanup) = cleanup {
                    if cleanup.kind() != io::ErrorKind::NotFound {
                        return Err(InstallerError::RollbackFailed(format!(
                            "{label}: {e}; limpeza parcial {}: {cleanup}",
                            dest.display()
                        )));
                    }
                }
                report.failures.push((label, e.to_string()));
            }
        }
    }

    if report.prepared == 0 {
        let detail = report
            .failures
            .iter()
            .map(|(name, err)| format!("{}: {}", name, err))
            .collect::<Vec<_>>()
            .join("; ");
        return Err(if detail.is_empty() {
            InstallerError::NoUsefulFiles
        } else {
            InstallerError::ExtractionFailed(detail)
        });
    }

    Ok(report)
}

fn copy_loose_mod_to_staging(source: &Path, dest_dir: &Path) -> Result<(), InstallerError> {
    let filename = source
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    if !is_valid_mod_file(&filename) {
        return Err(InstallerError::NoUsefulFiles);
    }

    atomic_copy(source, &dest_dir.join(&filename))?;
    Ok(())
}

/// Remove o staging. Seguro de chamar quando ele não existe.
pub fn clear_staging(staging_dir: &Path) -> io::Result<()> {
    let parent_path = staging_dir
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Staging sem pai"))?;
    let name = staging_dir
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Staging sem nome"))?;
    let parent = match AnchoredDir::open(parent_path) {
        Ok(parent) => parent,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let _operation = parent.lock_exclusive()?;
    clear_staging_at(&parent, name)
}

fn clear_staging_at(parent: &AnchoredDir, name: &std::ffi::OsStr) -> io::Result<()> {
    match parent.remove_tree(name) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConflictType {
    ExactMatch,
    SizeDiff,
    NewFile,
}

#[derive(Debug, Clone)]
pub struct StagedFile {
    pub src_path: PathBuf,
    pub rel_path: PathBuf,
    /// Identidade do mod de origem: primeiro componente de `rel_path`.
    pub mod_root: String,
    pub filename: String,
    pub size: u64,
    pub conflict: ConflictType,
    pub existing_dest: Option<PathBuf>,
    /// Nome alternativo atribuído quando há colisão de destino na mesma fila (ex: .ts4script de autores diferentes)
    pub target_filename: Option<String>,
}

impl StagedFile {
    pub fn is_script(&self) -> bool {
        self.filename.to_lowercase().ends_with(".ts4script")
    }

    /// Onde este arquivo será gravado dentro de `Mods`.
    ///
    /// Atualizações vão para o caminho já existente. Arquivos novos vão para
    /// `managed_base` — [`MANAGED_BASE_DIR`] para mods, mas o instalador de
    /// traduções reaproveita este mesmo fluxo apontando para a pasta delas.
    /// Scripts ficam rasos de propósito: o jogo só carrega `.ts4script` até um
    /// nível de subpasta, então `00_Triagem_Novos/x.ts4script` é o mais fundo
    /// que podemos ir sem quebrar o mod.
    pub fn destination(&self, mods_dir: &Path, managed_base: &str) -> PathBuf {
        if let Some(existing) = &self.existing_dest {
            return existing.clone();
        }
        let managed = mods_dir.join(managed_base);
        let effective_name = self.target_filename.as_deref().unwrap_or(&self.filename);
        if self.is_script() {
            managed.join(effective_name)
        } else if let Some(parent) = self.rel_path.parent() {
            managed.join(parent).join(effective_name)
        } else {
            managed.join(effective_name)
        }
    }
}

#[derive(Debug, Clone)]
struct ExistingCandidate {
    path: PathBuf,
    size: u64,
}

pub struct ConflictReport {
    pub staged_files: Vec<StagedFile>,
    pub exact_matches: usize,
    pub updates: usize,
    pub new_files: usize,
}

pub fn calculate_conflicts(
    staging_dir: &Path,
    mods_dir: &Path,
) -> Result<ConflictReport, InstallerError> {
    let mut staged_files = Vec::new();
    let mut exact_matches: usize = 0;
    let mut updates: usize = 0;
    let mut new_files: usize = 0;

    // O staging e os backups moram dentro de Mods. Sem excluí-los aqui, os
    // arquivos recém-extraídos apareceriam como "já instalados" e toda a fila
    // seria classificada como ExactMatch.

    let mut existing_map: HashMap<String, Vec<ExistingCandidate>> = HashMap::new();
    for entry in crate::engine::walk_user_mods(mods_dir, usize::MAX) {
        if entry.path().is_file() {
            let fname = entry.file_name().to_string_lossy().to_string();
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            existing_map
                .entry(fname)
                .or_default()
                .push(ExistingCandidate {
                    path: entry.path().to_path_buf(),
                    size,
                });
        }
    }

    for entry in WalkDir::new(staging_dir).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        let filename = entry.file_name().to_string_lossy().to_string();
        if !is_valid_mod_file(&filename) {
            continue;
        }

        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
        let rel_path = path.strip_prefix(staging_dir).unwrap_or(path).to_path_buf();
        let mod_root = rel_path
            .components()
            .find_map(|c| match c {
                Component::Normal(part) => Some(part.to_string_lossy().to_string()),
                _ => None,
            })
            .unwrap_or_else(|| "mod".to_string());

        let candidates = existing_map
            .get(&filename)
            .map(|v| v.as_slice())
            .unwrap_or(&[]);

        // Localiza candidato legítimo para atualização evitando substituir mods não relacionados
        let matched_dest = find_matching_existing_file(
            &rel_path,
            &mod_root,
            &filename,
            candidates,
            mods_dir,
            MANAGED_BASE_DIR,
        );

        let (conflict, existing_dest) = match matched_dest {
            Some(cand) => {
                let is_identical = if cand.size == size {
                    files_are_identical(path, &cand.path).unwrap_or(false)
                } else {
                    false
                };

                if is_identical {
                    exact_matches += 1;
                    (ConflictType::ExactMatch, Some(cand.path.clone()))
                } else {
                    updates += 1;
                    (ConflictType::SizeDiff, Some(cand.path.clone()))
                }
            }
            None => {
                new_files += 1;
                (ConflictType::NewFile, None)
            }
        };
        staged_files.push(StagedFile {
            src_path: path.to_path_buf(),
            rel_path,
            mod_root,
            filename,
            size,
            conflict,
            existing_dest,
            target_filename: None,
        });
    }

    if staged_files.is_empty() {
        return Err(InstallerError::NoUsefulFiles);
    }

    // Detecta e resolve antecipadamente colisões de destino na mesma fila
    // Especialmente .ts4script de autores diferentes ou arquivos com mesmo caminho relativo
    let mut script_counts: HashMap<String, usize> = HashMap::new();
    for f in &staged_files {
        if f.is_script() && f.existing_dest.is_none() {
            *script_counts.entry(f.filename.to_lowercase()).or_default() += 1;
        }
    }

    let mut assigned_destinations: HashSet<PathBuf> = HashSet::new();
    for f in &mut staged_files {
        if f.is_script() && f.existing_dest.is_none() {
            if let Some(&count) = script_counts.get(&f.filename.to_lowercase()) {
                if count > 1 {
                    // Há múltiplos scripts com o mesmo nome na fila: prefixa a identidade do mod para desambiguar
                    let namespaced = format!("{}_{}", f.mod_root, f.filename);
                    f.target_filename = Some(namespaced);
                }
            }
        }

        // Se o arquivo é uma atualização aprovada com existing_dest:
        if let Some(existing) = &f.existing_dest {
            if assigned_destinations.insert(existing.clone()) {
                continue;
            }
            // Outro arquivo na fila JÁ reivindicou este destino existente:
            // Desvincula para não substituir silenciosamente o primeiro arquivo
            f.existing_dest = None;
            match f.conflict {
                ConflictType::ExactMatch => exact_matches = exact_matches.saturating_sub(1),
                ConflictType::SizeDiff => updates = updates.saturating_sub(1),
                ConflictType::NewFile => {}
            }
            f.conflict = ConflictType::NewFile;
            new_files += 1;
        }

        // Para arquivos novos (ou atualizações colidentes desvinculadas):
        // Garante que o destino seja estritamente único na fila E no disco
        let mut dest = f.destination(mods_dir, MANAGED_BASE_DIR);
        let mut counter = 1;
        while assigned_destinations.contains(&dest)
            || (path_exists_or_symlink(&dest) && f.existing_dest.is_none())
        {
            counter += 1;
            let base_stem = if let Some(custom) = &f.target_filename {
                Path::new(custom)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("mod")
                    .to_string()
            } else if f.is_script() {
                format!(
                    "{}_{}",
                    f.mod_root,
                    Path::new(&f.filename)
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("script")
                )
            } else {
                Path::new(&f.filename)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("mod")
                    .to_string()
            };
            let ext = Path::new(&f.filename)
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("");
            let dot_ext = if ext.is_empty() {
                String::new()
            } else {
                format!(".{}", ext)
            };
            let unique_name = format!("{}_{}{}", base_stem, counter, dot_ext);
            f.target_filename = Some(unique_name);
            dest = f.destination(mods_dir, MANAGED_BASE_DIR);
        }
        assigned_destinations.insert(dest);
    }

    Ok(ConflictReport {
        staged_files,
        exact_matches,
        updates,
        new_files,
    })
}

/// Marcas que autores usam em nomes de pasta para dizer "escolha só um destes".
const EXCLUSIVE_KEYWORDS: [&str; 8] = [
    "option",
    "choose",
    "pick",
    "exclusive",
    "select one",
    "only one",
    "select_one",
    "only_one",
];

/// Variantes do mesmo mod que não devem ser instaladas juntas.
///
/// Um pacote com `Options/Cabelo Loiro.package` e `Options/Cabelo Ruivo.package`
/// espera que o usuário escolha uma. Instalar as duas costuma dar conflito
/// dentro do jogo, então a UI pergunta antes de gravar.
pub fn detect_mutually_exclusive(staged: &[StagedFile]) -> Vec<StagedFile> {
    let opcoes: Vec<StagedFile> = staged
        .iter()
        .filter(|file| {
            let rel = file
                .rel_path
                .to_string_lossy()
                .replace('\\', "/")
                .to_lowercase();
            // A marca tem que estar num componente do caminho, não no arquivo
            // inteiro: "description.package" contém "script", mas não é opção.
            rel.split('/')
                .any(|parte| EXCLUSIVE_KEYWORDS.iter().any(|key| parte.contains(key)))
        })
        .cloned()
        .collect();

    // Uma opção sozinha não é uma escolha.
    if opcoes.len() > 1 {
        opcoes
    } else {
        Vec::new()
    }
}

/// Maior recurso que vale a pena descomprimir à procura de uma marca.
///
/// A declaração de dependência mora na tuning, que tem alguns KB. O teto existe
/// para não gastar tempo inflando malha e textura.
const LIMITE_RECURSO_BYTES: u32 = 256 * 1024;

/// Bibliotecas que um package exige, lidas de dentro dos recursos dele.
///
/// A busca precisa acontecer **depois** de descomprimir: nos bytes crus de um
/// `.package` real a marca não aparece, porque o conteúdo vem em zlib.
///
/// Quando o arquivo não é um DBPF legível, cai para a varredura crua do começo
/// dele — é o que sobra para um package malformado, e é o que os testes de
/// unidade exercitam.
pub fn detect_dependencies_in_package(path: &Path) -> Vec<String> {
    let mut encontradas: Vec<String> = Vec::new();

    let leitura = crate::engine::dbpf::for_each_payload(path, LIMITE_RECURSO_BYTES, |dados| {
        for dep in detect_dependencies(dados) {
            if !encontradas.contains(&dep) {
                encontradas.push(dep);
            }
        }
    });

    if leitura.is_err() {
        const LIMITE_LEITURA: usize = 500_000;
        if let Ok(handle) = File::open(path) {
            use std::io::Read as _;
            let mut buffer = Vec::with_capacity(LIMITE_LEITURA);
            if handle
                .take(LIMITE_LEITURA as u64)
                .read_to_end(&mut buffer)
                .is_ok()
            {
                for dep in detect_dependencies(&buffer) {
                    if !encontradas.contains(&dep) {
                        encontradas.push(dep);
                    }
                }
            }
        }
    }

    encontradas.sort();
    encontradas
}

/// Bibliotecas que os packages da fila exigem para funcionar.
pub fn collect_dependencies(staged: &[StagedFile]) -> Vec<String> {
    let mut encontradas: Vec<String> = Vec::new();

    for file in staged.iter().filter(|f| !f.is_script()) {
        for dep in detect_dependencies_in_package(&file.src_path) {
            if !encontradas.contains(&dep) {
                encontradas.push(dep);
            }
        }
    }

    encontradas.sort();
    encontradas
}

/// Copia um arquivo regular completo e publica sem substituir qualquer entrada.
/// No Linux os diretórios ficam ancorados por descritor durante toda a cópia;
/// plataformas sem essa garantia falham fechadas.
pub fn atomic_copy(src: &Path, dest: &Path) -> io::Result<()> {
    let mut source = open_regular_path(src)?;
    let parent = dest
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Destino sem pasta pai"))?;
    let name = dest
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Destino sem nome"))?;
    copy_open_file(&mut source, &AnchoredDir::create(parent)?, name).map(|_| ())
}

pub(crate) fn atomic_copy_at(
    src_parent: &AnchoredDir,
    src_name: impl AsRef<std::ffi::OsStr>,
    dest_parent: &AnchoredDir,
    dest_name: impl AsRef<std::ffi::OsStr>,
) -> io::Result<()> {
    let mut source = src_parent.open_regular(src_name)?;
    copy_open_file(&mut source, dest_parent, dest_name.as_ref()).map(|_| ())
}

fn open_regular_path(path: &Path) -> io::Result<File> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Fonte sem pasta pai"))?;
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Fonte sem nome"))?;
    AnchoredDir::open(parent)?.open_regular(name)
}

fn unchanged_metadata(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        same_file_identity(a, b)
            && a.len() == b.len()
            && a.mtime() == b.mtime()
            && a.mtime_nsec() == b.mtime_nsec()
            && a.ctime() == b.ctime()
            && a.ctime_nsec() == b.ctime_nsec()
    }
    #[cfg(not(unix))]
    {
        let _ = (a, b);
        false
    }
}

fn copy_open_file(
    source: &mut File,
    parent: &AnchoredDir,
    name: &std::ffi::OsStr,
) -> io::Result<File> {
    let before = source.metadata()?;
    if !before.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Fonte não é arquivo regular",
        ));
    }
    source.rewind()?;
    let workspace = tempfile::Builder::new()
        .prefix(".s4suite-copy-")
        .tempdir_in(parent.path())?;
    let private = parent.child(workspace.path().file_name().unwrap())?;
    let mut output = private.create_new("payload")?;
    let result = (|| {
        let count = io::copy(source, &mut output)?;
        output.sync_all()?;
        if count != before.len() || !unchanged_metadata(&before, &source.metadata()?) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Fonte mudou ou cópia ficou incompleta",
            ));
        }
        private.publish_noreplace("payload", parent, name)
    })();
    if let Err(error) = result {
        return match workspace.close() {
            Ok(()) => Err(error),
            Err(cleanup) => Err(io::Error::new(
                error.kind(),
                format!("{error}; falha ao limpar cópia privada: {cleanup}"),
            )),
        };
    }
    if let Err(cleanup) = workspace.close() {
        let rollback = remove_owned_file(parent, name, &output);
        return Err(io::Error::other(format!(
            "Falha ao limpar cópia privada: {cleanup}; rollback: {rollback:?}"
        )));
    }
    Ok(output)
}

/// Captura a entrada em quarentena antes de conferir identidade. Nunca exclui
/// um substituto concorrente: restaura sem sobrescrever ou preserva quarentena.
fn remove_owned_file(parent: &AnchoredDir, name: &std::ffi::OsStr, owned: &File) -> io::Result<()> {
    let workspace = tempfile::Builder::new()
        .prefix(".s4suite-rollback-")
        .tempdir_in(parent.path())?;
    let quarantine = parent.child(workspace.path().file_name().unwrap())?;
    parent.publish_noreplace(name, &quarantine, "captured")?;
    let captured = quarantine.open_regular("captured");
    let matches = captured
        .as_ref()
        .ok()
        .and_then(|file| file.metadata().ok())
        .zip(owned.metadata().ok())
        .map(|(a, b)| same_file_identity(&a, &b))
        .unwrap_or(false);
    if !matches {
        let restore = quarantine.publish_noreplace("captured", parent, name);
        if let Err(error) = restore {
            let preserved = workspace.keep();
            return Err(io::Error::other(format!(
                "Entrada concorrente preservada em {}: {error}",
                preserved.display()
            )));
        }
        workspace.close()?;
        return Err(io::Error::other(
            "Rollback bloqueado: entrada substituída por outro processo",
        ));
    }
    quarantine.remove_file("captured")?;
    workspace.close()
}

/// Compara byte a byte o conteúdo de dois arquivos.
pub fn files_are_identical(a: &Path, b: &Path) -> io::Result<bool> {
    let meta_a = fs::metadata(a)?;
    let meta_b = fs::metadata(b)?;
    if meta_a.len() != meta_b.len() {
        return Ok(false);
    }
    if meta_a.len() == 0 {
        return Ok(true);
    }
    let mut file_a = File::open(a)?;
    let mut file_b = File::open(b)?;
    let mut buf_a = [0u8; 64 * 1024];
    let mut buf_b = [0u8; 64 * 1024];
    loop {
        let n_a = file_a.read(&mut buf_a)?;
        let n_b = file_b.read(&mut buf_b)?;
        if n_a != n_b || buf_a[..n_a] != buf_b[..n_b] {
            return Ok(false);
        }
        if n_a == 0 {
            return Ok(true);
        }
    }
}

fn find_matching_existing_file<'a>(
    rel_path: &Path,
    mod_root: &str,
    filename: &str,
    candidates: &'a [ExistingCandidate],
    mods_dir: &Path,
    managed_base: &str,
) -> Option<&'a ExistingCandidate> {
    if candidates.is_empty() {
        return None;
    }

    let is_script = filename.to_lowercase().ends_with(".ts4script");
    let expected_dest = if is_script {
        mods_dir.join(managed_base).join(filename)
    } else {
        mods_dir.join(managed_base).join(rel_path)
    };

    // 1. Caminho exato de destino já existente
    if let Some(exact) = candidates.iter().find(|c| c.path == expected_dest) {
        return Some(exact);
    }

    // 2. Caminho relativo coincide dentro de alguma subpasta de Mods
    if !is_script {
        if let Some(rel_match) = candidates.iter().find(|c| c.path.ends_with(rel_path)) {
            return Some(rel_match);
        }
    }

    // 3. A pasta pai do candidato tem o mesmo nome da identidade do mod (mod_root)
    if let Some(parent_match) = candidates.iter().find(|c| {
        c.path
            .parent()
            .and_then(|p| p.file_name())
            .map(|name| name.to_string_lossy() == mod_root)
            .unwrap_or(false)
    }) {
        return Some(parent_match);
    }

    // 4. Caso o mod consista de um único arquivo principal cujo stem coincida com o mod_root
    //    (ex: alpha.package originado de alpha.zip / alpha.package avulso)
    //    E exista EXATAMENTE um candidato em toda a pasta Mods.
    let file_stem = Path::new(filename)
        .file_stem()
        .map(|s| s.to_string_lossy())
        .unwrap_or_default();
    if candidates.len() == 1 && file_stem.eq_ignore_ascii_case(mod_root) {
        return Some(&candidates[0]);
    }

    // Se nenhum critério de identidade confirmou que é o mesmo mod,
    // trata como arquivo novo para impedir a substituição de mods de outros autores.
    None
}

pub fn execute_installation(
    report: &ConflictReport,
    mods_dir: &Path,
    managed_base: &str,
    allowed_roots: &[PathBuf],
) -> Result<(usize, usize), InstallerError> {
    let mods = AnchoredDir::open(mods_dir)?;
    let _operation = mods.lock_exclusive()?;
    let absolute_mods = if mods_dir.is_absolute() {
        mods_dir.to_path_buf()
    } else {
        std::env::current_dir()?.join(mods_dir)
    };
    let mut destinations = HashSet::new();
    let mut prepared = Vec::new();
    // Valida o lote inteiro antes de criar backups ou publicar qualquer arquivo.
    for file in &report.staged_files {
        let target = file.destination(&absolute_mods, managed_base);
        is_safe_to_delete(&target, allowed_roots)
            .map_err(|e| InstallerError::SecurityBlocked(e.to_string()))?;
        let relative = target
            .strip_prefix(&absolute_mods)
            .map_err(|_| InstallerError::SecurityBlocked(target.display().to_string()))?;
        if relative
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
            || !destinations.insert(relative.to_path_buf())
        {
            return Err(InstallerError::SecurityBlocked(format!(
                "Destino inválido ou duplicado: {}",
                target.display()
            )));
        }
        let source = open_regular_path(&file.src_path)?;
        if source.metadata()?.len() != file.size {
            return Err(InstallerError::Io(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Tamanho da fonte mudou: {}", file.src_path.display()),
            )));
        }
        prepared.push((file, source, relative.to_path_buf()));
    }
    let backup_dir = mods.create_child(BACKUP_DIR_NAME)?;
    struct TransactionEntry {
        parent: AnchoredDir,
        name: std::ffi::OsString,
        backup: Option<std::ffi::OsString>,
        published: Option<File>,
    }
    let mut journal: Vec<TransactionEntry> = Vec::new();
    let mut installed_count = 0;
    let mut skipped_count = 0;
    let result = (|| -> io::Result<()> {
        for (file, mut source, relative) in prepared {
            let mut parent = mods.try_clone()?;
            // Caminha a partir da raiz já aberta, não por um path re-resolvido.
            if let Some(relative_parent) = relative.parent() {
                for component in relative_parent.components() {
                    if let Component::Normal(name) = component {
                        parent = parent.create_child(name)?;
                    }
                }
            }
            let name = relative.file_name().unwrap().to_os_string();
            if file.conflict == ConflictType::ExactMatch {
                let mut existing = parent.open_regular(&name)?;
                if !open_files_are_identical(&mut source, &mut existing)? {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "Correspondência exata mudou após o relatório",
                    ));
                }
                skipped_count += 1;
                continue;
            }
            let backup = if file.existing_dest.is_some() && file.conflict == ConflictType::SizeDiff
            {
                let original = parent.open_regular(&name)?;
                let before = original.metadata()?;
                let backup_name = loop {
                    let candidate = unique_dest_path(&backup_dir.path(), &file.filename);
                    let candidate = candidate.file_name().unwrap().to_os_string();
                    match parent.publish_noreplace(&name, &backup_dir, &candidate) {
                        Ok(()) => break candidate,
                        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                        Err(e) => return Err(e),
                    }
                };
                let captured = backup_dir
                    .open_regular(&backup_name)
                    .and_then(|file| file.metadata());
                if !captured
                    .as_ref()
                    .map(|meta| same_file_identity(&before, meta))
                    .unwrap_or(false)
                {
                    let restored = backup_dir.publish_noreplace(&backup_name, &parent, &name);
                    return Err(io::Error::other(format!(
                        "Entrada mudou durante captura de backup {}; restauração: {restored:?}",
                        backup_dir.path().join(&backup_name).display()
                    )));
                }
                Some(backup_name)
            } else {
                None
            };
            journal.push(TransactionEntry {
                parent,
                name,
                backup,
                published: None,
            });
            let entry = journal.last_mut().unwrap();
            entry.published = Some(copy_open_file(&mut source, &entry.parent, &entry.name)?);
            installed_count += 1;
        }
        Ok(())
    })();
    if let Err(error) = result {
        let mut rollback_errors = Vec::new();
        for entry in journal.into_iter().rev() {
            if let Some(published) = entry.published {
                if let Err(e) = remove_owned_file(&entry.parent, &entry.name, &published) {
                    rollback_errors.push(format!("remoção {}: {e}", entry.name.to_string_lossy()));
                    continue;
                }
            }
            if let Some(backup) = entry.backup {
                let restored = backup_dir
                    .open_regular(&backup)
                    .and_then(|mut source| copy_open_file(&mut source, &entry.parent, &entry.name));
                if let Err(e) = restored {
                    rollback_errors.push(format!(
                        "restauração {} (backup {}): {e}",
                        entry.name.to_string_lossy(),
                        backup.to_string_lossy()
                    ));
                }
            }
        }
        if !rollback_errors.is_empty() {
            return Err(InstallerError::RollbackFailed(format!(
                "{error}; {}",
                rollback_errors.join("; ")
            )));
        }
        return Err(InstallerError::Io(error));
    }
    Ok((installed_count, skipped_count))
}

fn open_files_are_identical(a: &mut File, b: &mut File) -> io::Result<bool> {
    a.rewind()?;
    b.rewind()?;
    let before_a = a.metadata()?;
    let before_b = b.metadata()?;
    if before_a.len() != before_b.len() {
        return Ok(false);
    }
    let mut buf_a = [0u8; 64 * 1024];
    let mut buf_b = [0u8; 64 * 1024];
    loop {
        let count_a = a.read(&mut buf_a)?;
        let count_b = b.read(&mut buf_b)?;
        if count_a != count_b || buf_a[..count_a] != buf_b[..count_b] {
            return Ok(false);
        }
        if count_a == 0 {
            return Ok(unchanged_metadata(&before_a, &a.metadata()?)
                && unchanged_metadata(&before_b, &b.metadata()?));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn p0_listagem_7z_recusa_ambiguidade_links_e_traversal() {
        let valid = "Path = mod.package\nSize = 4\nFolder = -\nAttributes = A_ -rw-r--r--\n";
        assert_eq!(parse_7z_listing(valid).unwrap().len(), 1);
        for listing in [
            "Path = ../outside\nSize = 4\nFolder = -\n",
            "Path = /outside\nSize = 4\nFolder = -\n",
            "Path = mod.package\nSize = 4\nSize = 8\nFolder = -\n",
            "Path = link\nSize = 4\nFolder = -\nSymbolic Link = outside\n",
            "Path = link\nSize = 4\nFolder = -\nHard Link = outside\n",
            "Path = mod.package\nnewline-in-name\nSize = 4\nFolder = -\n",
        ] {
            assert!(
                parse_7z_listing(listing).is_err(),
                "listagem insegura aceita: {listing:?}"
            );
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn p0_entrada_extraida_incompleta_ou_processo_falho_nao_e_publicada() {
        let root = tempfile::tempdir().unwrap();
        let destination = AnchoredDir::open(root.path()).unwrap();
        let incomplete = extract_entry_at(
            &destination,
            Path::new("incomplete.package"),
            false,
            20,
            &mut io::Cursor::new(b"short"),
            || Ok(()),
        );
        assert!(incomplete.is_err());
        assert!(!root.path().join("incomplete.package").exists());
        let failed = extract_entry_at(
            &destination,
            Path::new("failed.package"),
            false,
            5,
            &mut io::Cursor::new(b"whole"),
            || Err(io::Error::other("extrator terminou com falha")),
        );
        assert!(failed.is_err());
        assert!(!root.path().join("failed.package").exists());
    }
}
