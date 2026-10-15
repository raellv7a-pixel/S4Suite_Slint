use crate::core::safety::{safe_remove_file, unique_dest_path};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, Read};
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
        let dest = staging_dir.join(&identity);
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
    let mut exact_matches = 0;
    let mut updates = 0;
    let mut new_files = 0;

    // O staging e os backups moram dentro de Mods. Sem excluí-los aqui, os
    // arquivos recém-extraídos apareceriam como "já instalados" e toda a fila
    // seria classificada como ExactMatch.

    let mut existing_map: HashMap<String, Vec<ExistingCandidate>> = HashMap::new();
    for entry in crate::engine::walk_user_mods(mods_dir, usize::MAX) {
        if entry.path().is_file() {
            let fname = entry.file_name().to_string_lossy().to_string();
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            existing_map.entry(fname).or_default().push(ExistingCandidate {
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

        let candidates = existing_map.get(&filename).map(|v| v.as_slice()).unwrap_or(&[]);

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
            let rel = file.rel_path.to_string_lossy().replace('\\', "/").to_lowercase();
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

/// Copia de forma atômica usando um arquivo temporário no mesmo diretório de destino.
/// Garante que interrupções ou erros não deixem arquivos parciais corrompidos no destino.
pub fn atomic_copy(src: &Path, dest: &Path) -> io::Result<()> {
    let parent = dest.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "Destino sem pasta pai")
    })?;
    fs::create_dir_all(parent)?;
    let filename = dest.file_name().unwrap_or_default().to_string_lossy();
    let tmp_dest = unique_dest_path(parent, &format!(".{}.s4tmp", filename));

    if let Err(e) = fs::copy(src, &tmp_dest) {
        let _ = fs::remove_file(&tmp_dest);
        return Err(e);
    }

    if let Err(e) = fs::rename(&tmp_dest, dest) {
        let _ = fs::remove_file(&tmp_dest);
        return Err(e);
    }

    Ok(())
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
    let backup_dir = mods_dir.join(BACKUP_DIR_NAME);
    fs::create_dir_all(&backup_dir)?;

    let mut installed_count = 0;
    let mut skipped_count = 0;
    let mut backup_copies: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut backed_up_targets: HashSet<PathBuf> = HashSet::new();
    let mut newly_created_files: Vec<PathBuf> = Vec::new();
    let mut updated_files: Vec<PathBuf> = Vec::new();

    let mut execute = || -> Result<(), io::Error> {
        for file in &report.staged_files {
            // Byte-por-byte idêntico já presente: copiar de novo só gasta I/O.
            if file.conflict == ConflictType::ExactMatch {
                skipped_count += 1;
                continue;
            }

            let target_dest = file.destination(mods_dir, managed_base);

            // Se o arquivo já existe no destino, faça backup atômico antes de sobrescrever
            if target_dest.exists() {
                if backed_up_targets.insert(target_dest.clone()) {
                    let backup_file = unique_dest_path(&backup_dir, &file.filename);
                    atomic_copy(&target_dest, &backup_file)?;
                    backup_copies.push((target_dest.clone(), backup_file));
                }
                updated_files.push(target_dest.clone());
            } else {
                newly_created_files.push(target_dest.clone());
            }

            // Instalação atômica via arquivo temporário para evitar arquivos parciais
            atomic_copy(&file.src_path, &target_dest)?;
            installed_count += 1;
        }
        Ok(())
    };

    if let Err(e) = execute() {
        // Rollback robusto: não engole erros silenciosamente e restaura arquivos de forma atômica
        let mut rollback_errors = Vec::new();

        // 1. Remove apenas os arquivos verdadeiramente novos criados nesta instalação
        for dest in newly_created_files {
            if dest.exists() {
                if let Err(err) = safe_remove_file(&dest, allowed_roots) {
                    rollback_errors.push(format!(
                        "falha ao remover arquivo novo {}: {}",
                        dest.display(),
                        err
                    ));
                }
            }
        }

        // 2. Restaura os arquivos anteriores a partir do backup
        for (target_dest, backup_file) in backup_copies {
            if backup_file.exists() {
                if let Err(err) = atomic_copy(&backup_file, &target_dest) {
                    rollback_errors.push(format!(
                        "falha ao restaurar {} de {}: {}",
                        target_dest.display(),
                        backup_file.display(),
                        err
                    ));
                }
            } else {
                rollback_errors.push(format!(
                    "backup ausente para restauração de {}",
                    target_dest.display()
                ));
            }
        }

        if !rollback_errors.is_empty() {
            return Err(InstallerError::RollbackFailed(format!(
                "Erro na instalação: {}. Erros adicionais no rollback: {}",
                e,
                rollback_errors.join("; ")
            )));
        }

        return Err(InstallerError::Io(e));
    }

    Ok((installed_count, skipped_count))
}

