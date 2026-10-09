use crate::core::safety::{safe_remove_file, unique_dest_path};
use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File};
use std::io;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
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
    #[error("Nenhum arquivo de mod útil (.package ou .ts4script) encontrado no arquivo compactado")]
    NoUsefulFiles,
    #[error("Erro ao descompactar: {0}")]
    ExtractionFailed(String),
    #[error("Selecione ao menos um arquivo pertencente ao grupo de possíveis variantes")]
    InvalidVariantSelection,
    #[error("Erro de I/O: {0}")]
    Io(#[from] io::Error),
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
    let trimmed = sanitized.trim().trim_matches('_').trim().trim_matches('.').trim();
    if trimmed.is_empty() {
        "mod".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Um arquivo só é instalável se for `.package` ou `.ts4script` e não for
/// resíduo de sistema de arquivos (`._foo`, `.DS_Store`).
pub fn is_valid_mod_file(filename: &str) -> bool {
    if filename.starts_with("._") || filename.eq_ignore_ascii_case(".DS_Store") {
        return false;
    }
    let lower = filename.to_lowercase();
    lower.ends_with(".package") || lower.ends_with(".ts4script")
}

fn is_archive(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase().as_str(),
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
                    return Err(InstallerError::SecurityBlocked(format!("Extensão .{} encontrada", ext)));
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
    if package_bytes.windows(12).any(|w| w == b"XML Injector" || w == b"XmlInjector") {
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
    fs::create_dir_all(staging_dir)?;

    let ext = archive_path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    if ext == "zip" {
        if let Ok(file) = File::open(archive_path) {
            if let Ok(mut archive) = zip::ZipArchive::new(file) {
                archive.extract(staging_dir).map_err(|e| InstallerError::ExtractionFailed(e.to_string()))?;
                return Ok(staging_dir.to_path_buf());
            }
        }
    }

    // Try 7z CLI or sevenz-rust
    if ext == "7z" {
        if sevenz_rust::decompress_file(archive_path, staging_dir).is_ok() {
            return Ok(staging_dir.to_path_buf());
        }
    }

    // Fallback 7z process. Usamos `output()` para capturar stdout/stderr — com
    // `status()` o 7z escreve direto no terminal que lançou a aplicação.
    let output = Command::new("7z")
        .arg("x")
        .arg(archive_path)
        .arg(format!("-o{}", staging_dir.display()))
        .arg("-y")
        .output();

    match output {
        Ok(o) if o.status.success() => Ok(staging_dir.to_path_buf()),
        Ok(_) => Err(InstallerError::ExtractionFailed(
            "formato não reconhecido ou arquivo corrompido".to_string(),
        )),
        Err(_) => Err(InstallerError::ExtractionFailed(
            "não foi possível extrair (instale o 7z para suportar .rar e .7z)".to_string(),
        )),
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
    if staging_dir.exists() {
        fs::remove_dir_all(staging_dir)?;
    }
    fs::create_dir_all(staging_dir)?;

    let mut report = StagingReport::default();

    for source in sources {
        let identity = install_identity_name(source);
        // Fontes homônimas continuam independentes, inclusive após sanitização.
        let dest = unique_dest_path(staging_dir, &identity);
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
                let _ = fs::remove_dir_all(&dest);
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

    fs::create_dir_all(dest_dir)?;
    fs::copy(source, dest_dir.join(&filename))?;
    Ok(())
}

/// Remove o staging. Seguro de chamar quando ele não existe.
pub fn clear_staging(staging_dir: &Path) -> io::Result<()> {
    if staging_dir.exists() {
        fs::remove_dir_all(staging_dir)?;
    }
    Ok(())
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
        if self.is_script() {
            managed.join(&self.filename)
        } else {
            managed.join(&self.rel_path)
        }
    }
}

pub struct ConflictReport {
    pub staged_files: Vec<StagedFile>,
    pub exact_matches: usize,
    pub updates: usize,
    pub new_files: usize,
}

impl ConflictReport {
    /// Filtra apenas este grupo; arquivos comuns e outros grupos são preservados.
    pub fn select_variants(
        &mut self,
        group: &VariantGroup,
        selected: &[PathBuf],
    ) -> Result<(), InstallerError> {
        if selected.is_empty() || selected.iter().any(|path| !group.files.contains(path)) {
            return Err(InstallerError::InvalidVariantSelection);
        }
        self.staged_files.retain(|file| {
            !group.files.contains(&file.rel_path) || selected.contains(&file.rel_path)
        });
        self.new_files = self
            .staged_files
            .iter()
            .filter(|file| file.conflict == ConflictType::NewFile)
            .count();
        self.updates = self
            .staged_files
            .iter()
            .filter(|file| file.conflict == ConflictType::SizeDiff)
            .count();
        self.exact_matches = self
            .staged_files
            .iter()
            .filter(|file| file.conflict == ConflictType::ExactMatch)
            .count();
        Ok(())
    }
}

pub fn calculate_conflicts(
    staging_dir: &Path,
    mods_dir: &Path,
) -> Result<ConflictReport, InstallerError> {
    let mut staged_files = Vec::new();
    let mut exact_matches = 0;
    let mut updates = 0;
    let mut new_files = 0;

    // O staging e os backups moram dentro de Mods. Sem excluí-los aqui, os
    // arquivos recém-extraídos apareceriam como "já instalados" e toda a fila
    // seria classificada como ExactMatch.
    let mut existing_map: HashMap<String, (PathBuf, u64)> = HashMap::new();
    for entry in crate::engine::walk_user_mods(mods_dir, usize::MAX) {
        if entry.path().is_file() {
            let fname = entry.file_name().to_string_lossy().to_string();
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            existing_map.insert(fname, (entry.path().to_path_buf(), size));
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

        let (conflict, existing_dest) = match existing_map.get(&filename) {
            Some((dest, existing_size)) if *existing_size == size => {
                exact_matches += 1;
                (ConflictType::ExactMatch, Some(dest.clone()))
            }
            Some((dest, _)) => {
                updates += 1;
                (ConflictType::SizeDiff, Some(dest.clone()))
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
        });
    }

    if staged_files.is_empty() {
        return Err(InstallerError::NoUsefulFiles);
    }

    Ok(ConflictReport {
        staged_files,
        exact_matches,
        updates,
        new_files,
    })
}

/// Um grupo heurístico, não uma incompatibilidade comprovada.
/// `folder` inclui a identidade da fonte e a pasta interna marcada como opções.
#[derive(Debug, Clone)]
pub struct VariantGroup {
    pub source: String,
    pub folder: PathBuf,
    pub files: Vec<PathBuf>,
}

fn is_variant_folder(name: &std::ffi::OsStr) -> bool {
    let normalized = name
        .to_string_lossy()
        .to_lowercase()
        .replace(['_', '-'], " ");
    let mut words = normalized.split_whitespace();
    matches!(
        (words.next(), words.next(), words.next(), words.next()),
        (
            Some("option" | "options" | "exclusive" | "exclusives"),
            None,
            None,
            None
        ) | (
            Some("choose" | "pick" | "select" | "only"),
            Some("one"),
            None,
            None
        ) | (Some("choose" | "pick"), Some("only"), Some("one"), None)
    )
}

/// Procura marcadores explícitos somente em pastas internas da mesma fonte.
/// Nome do ZIP, arquivo avulso e substrings como "Optional" não são evidência.
/// A pasta marcada mais próxima separa decisões internas do mesmo pacote.
pub fn detect_mutually_exclusive(staged: &[StagedFile]) -> Vec<VariantGroup> {
    let mut groups: BTreeMap<PathBuf, VariantGroup> = BTreeMap::new();
    for file in staged {
        let Some(parent) = file.rel_path.parent() else {
            continue;
        };
        let mut folder = PathBuf::new();
        let mut marked = None;
        for (index, component) in parent.components().enumerate() {
            folder.push(component.as_os_str());
            if index > 0 && is_variant_folder(component.as_os_str()) {
                marked = Some(folder.clone());
            }
        }
        if let Some(folder) = marked {
            groups
                .entry(folder.clone())
                .or_insert_with(|| VariantGroup {
                    source: file.mod_root.clone(),
                    folder,
                    files: Vec::new(),
                })
                .files
                .push(file.rel_path.clone());
        }
    }
    groups
        .into_values()
        .filter(|group| group.files.len() > 1)
        .map(|mut group| {
            group.files.sort();
            group
        })
        .collect()
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
            if handle.take(LIMITE_LEITURA as u64).read_to_end(&mut buffer).is_ok() {
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

pub fn execute_installation(
    report: &ConflictReport,
    mods_dir: &Path,
    managed_base: &str,
    allowed_roots: &[PathBuf],
) -> Result<(usize, usize), InstallerError> {
    let backup_dir = mods_dir.join(BACKUP_DIR_NAME);
    fs::create_dir_all(&backup_dir)?;

    let mut installed_count = 0;
    let mut skipped_count = 0;
    let mut backup_copies: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut new_destinations: Vec<PathBuf> = Vec::new();

    let mut execute = || -> Result<(), io::Error> {
        for file in &report.staged_files {
            // Byte-por-byte já presente: copiar de novo só gasta I/O.
            if file.conflict == ConflictType::ExactMatch {
                skipped_count += 1;
                continue;
            }

            let target_dest = file.destination(mods_dir, managed_base);

            if target_dest.exists() {
                let backup_file = unique_dest_path(&backup_dir, &file.filename);
                fs::copy(&target_dest, &backup_file)?;
                backup_copies.push((target_dest.clone(), backup_file));
            }

            if let Some(parent) = target_dest.parent() {
                fs::create_dir_all(parent)?;
            }

            fs::copy(&file.src_path, &target_dest)?;
            new_destinations.push(target_dest);
            installed_count += 1;
        }
        Ok(())
    };

    if let Err(e) = execute() {
        // Rollback
        for dest in new_destinations {
            let _ = safe_remove_file(&dest, allowed_roots);
        }
        for (target_dest, backup_file) in backup_copies {
            let _ = fs::copy(&backup_file, &target_dest);
        }
        return Err(InstallerError::Io(e));
    }

    Ok((installed_count, skipped_count))
}
