use crate::core::safety::is_safe_to_delete;
use crate::engine::dbpf::{DBPFReader, DBPFWriter, PackageResource, ResourceKey};
use crate::engine::disabled::DisabledManager;
use crate::engine::organizer::{remove_files, RemovalOutcome};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergeReport {
    pub success: bool,
    pub partial: bool,
    pub total_files_found: usize,
    pub success_count: usize,
    pub failed_files: Vec<(String, String)>,
    pub output_packages: Vec<String>,
}

pub struct MergeTask {
    pub input_files: Vec<PathBuf>,
    pub output_dir: PathBuf,
    pub max_size_bytes: u64,
    /// Nome do grupo, como o usuário o vê na fila. Vira o prefixo dos arquivos
    /// gerados — `None` cai no nome genérico com timestamp.
    pub name: Option<String>,
}

pub fn merge_sims4_packages<F>(
    task: &MergeTask,
    progress_cb: F,
) -> Result<MergeReport, Box<dyn std::error::Error + Send + Sync>>
where
    F: Fn(usize, usize, &str) + Send + Sync,
{
    fs::create_dir_all(&task.output_dir)?;
    let output_real = real_path(&task.output_dir, false)?;
    let output_stamp = DirectoryStamp {
        identity: Identity::of(&fs::symlink_metadata(&output_real)?),
        real: output_real,
    };

    let total_files = task.input_files.len();
    if total_files == 0 {
        return Ok(MergeReport {
            success: false,
            partial: false,
            total_files_found: 0,
            success_count: 0,
            failed_files: Vec::new(),
            output_packages: Vec::new(),
        });
    }

    // O prefixo é resolvido **antes** de escrever qualquer parte: duas tarefas
    // da fila que terminam no mesmo segundo produziam o mesmo nome, e a segunda
    // gravava por cima da primeira — com os originais já consumidos pela
    // pós-ação, o conteúdo da primeira ia embora sem nenhum erro na tela.
    let stem = unique_output_stem(&task.output_dir, task.name.as_deref());
    let mut global_resources: IndexMap<ResourceKey, (PathBuf, u32, u32, u32, u16)> =
        IndexMap::new();
    let mut failed_files = Vec::new();
    let mut success_count: usize = 0;

    for (idx, fpath) in task.input_files.iter().enumerate() {
        let fname = fpath.file_name().unwrap_or_default().to_string_lossy();
        progress_cb(
            idx + 1,
            total_files,
            &format!("Lendo Metadados: {}/{} ({})", idx + 1, total_files, fname),
        );

        match File::open(fpath) {
            Ok(file) => {
                let mut reader = BufReader::with_capacity(512 * 1024, file);
                match DBPFReader::read_index(&mut reader) {
                    Ok((_header, index)) => {
                        let file_len = reader.get_ref().metadata()?.len();
                        if index.iter().any(|entry| {
                            u64::from(entry.location_offset) + u64::from(entry.file_size) > file_len
                                || (entry.file_size == 0 && entry.mem_size != 0)
                        }) {
                            failed_files.push((
                                fpath.display().to_string(),
                                "Payload ausente ou truncado.".to_string(),
                            ));
                            continue;
                        }
                        for entry in index {
                            // Later files in task.input_files overwrite earlier ones
                            global_resources.insert(
                                entry.key,
                                (
                                    fpath.clone(),
                                    entry.location_offset,
                                    entry.file_size,
                                    entry.mem_size,
                                    entry.compressed,
                                ),
                            );
                        }
                        success_count += 1;
                    }
                    Err(e) => {
                        failed_files.push((fpath.display().to_string(), e.to_string()));
                    }
                }
            }
            Err(e) => {
                failed_files.push((fpath.display().to_string(), e.to_string()));
            }
        }
    }

    if global_resources.is_empty() {
        return Ok(MergeReport {
            success: false,
            partial: false,
            total_files_found: total_files,
            success_count: 0,
            failed_files,
            output_packages: Vec::new(),
        });
    }

    // Sort resources by TGI for engine load times
    let mut sorted_keys: Vec<ResourceKey> = global_resources.keys().copied().collect();
    sorted_keys.sort();

    let mut chunk_index = 1;
    let mut current_chunk_resources: Vec<PackageResource> = Vec::new();
    let mut current_chunk_bytes = 0u64;
    let mut output_packages = Vec::new();

    let safe_limit = ((task.max_size_bytes as f64) * 0.90) as u64;

    // Cache opened files to avoid reopening repeatedly
    let mut file_cache: std::collections::HashMap<PathBuf, BufReader<File>> =
        std::collections::HashMap::new();

    for (res_idx, key) in sorted_keys.iter().enumerate() {
        if res_idx % 100 == 0 || res_idx == sorted_keys.len() - 1 {
            progress_cb(
                res_idx + 1,
                sorted_keys.len(),
                &format!(
                    "Processando Recursos: {}/{}",
                    res_idx + 1,
                    sorted_keys.len()
                ),
            );
        }

        let (src_path, offset, size, mem_size, compressed) = &global_resources[key];

        let payload = (|| -> io::Result<Vec<u8>> {
            if !file_cache.contains_key(src_path) {
                let file = File::open(src_path)?;
                file_cache.insert(
                    src_path.clone(),
                    BufReader::with_capacity(4 * 1024 * 1024, file),
                );
            }
            let reader = file_cache
                .get_mut(src_path)
                .expect("opened resource source");
            reader.seek(SeekFrom::Start(u64::from(*offset)))?;
            let mut data = vec![0u8; *size as usize];
            reader.read_exact(&mut data)?;
            // Preserve compressed bytes; validate only the existing zlib flag.
            if *compressed == 0x5A42 {
                let decoder = flate2::read::ZlibDecoder::new(data.as_slice());
                let length =
                    io::copy(&mut decoder.take(u64::from(*mem_size) + 1), &mut io::sink())?;
                if length != u64::from(*mem_size) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "Tamanho do payload zlib inválido.",
                    ));
                }
            }
            Ok(data)
        })();
        let data = match payload {
            Ok(data) => data,
            Err(error) => {
                let source = src_path.display().to_string();
                if !failed_files.iter().any(|(path, _)| path == &source) {
                    failed_files.push((source, error.to_string()));
                    success_count = success_count.saturating_sub(1);
                }
                continue;
            }
        };

        let res_len = data.len() as u64;
        if current_chunk_bytes + res_len > safe_limit && !current_chunk_resources.is_empty() {
            let part_name = format!("{}_Part{:03}.package", stem, chunk_index);
            let part_path = task.output_dir.join(&part_name);
            write_package_chunk(&part_path, &current_chunk_resources, &output_stamp)?;
            output_packages.push(part_path.display().to_string());

            chunk_index += 1;
            current_chunk_resources.clear();
            current_chunk_bytes = 0;
        }

        current_chunk_resources.push(PackageResource {
            key: *key,
            data,
            mem_size: *mem_size,
            compressed: *compressed,
        });
        current_chunk_bytes += res_len;
    }

    if !current_chunk_resources.is_empty() {
        let part_name = format!("{}_Part{:03}.package", stem, chunk_index);
        let part_path = task.output_dir.join(&part_name);
        write_package_chunk(&part_path, &current_chunk_resources, &output_stamp)?;
        output_packages.push(part_path.display().to_string());
    }

    let report = MergeReport {
        success: failed_files.is_empty(),
        partial: !failed_files.is_empty() && !output_packages.is_empty(),
        total_files_found: total_files,
        success_count,
        failed_files,
        output_packages,
    };

    // O relatório acompanha as partes: um `merge_report.json` fixo era
    // sobrescrito pela tarefa seguinte, apagando o registro da anterior.
    let report_path = task.output_dir.join(format!("{}_report.json", stem));
    validate_directory(&task.output_dir, &output_stamp)?;
    if let Ok(json) = serde_json::to_string_pretty(&report) {
        let _ = fs::write(report_path, json);
    }

    Ok(report)
}

/// O que fazer com os arquivos originais depois de um merge bem-sucedido.
///
/// Uma tarefa da fila: um conjunto de packages, um destino e o que fazer com os
/// originais depois.
///
/// A fila existe porque unificar a pasta inteira de mods de uma vez é o caso
/// raro; o normal é ter grupos com destinos diferentes — CC de cabelo numa
/// parte, roupas em outra — e o app PyQt já trabalhava assim
/// (`merger_tab.py:80`, `MergeQueueWorker`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeJob {
    pub name: String,
    pub input_files: Vec<PathBuf>,
    pub output_dir: PathBuf,
    pub max_size_bytes: u64,
    pub post_action: PostMergeAction,
}

/// Indício para revisão humana, nunca uma incompatibilidade comprovada.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeRisk {
    pub file: PathBuf,
    pub reason: &'static str,
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct MergeReviewError(String);

impl From<io::Error> for MergeReviewError {
    fn from(error: io::Error) -> Self {
        Self(error.to_string())
    }
}

fn review_error(message: impl Into<String>) -> MergeReviewError {
    MergeReviewError(message.into())
}

#[derive(Debug, PartialEq, Eq)]
struct Identity {
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(windows)]
    created: u64,
    #[cfg(windows)]
    attributes: u32,
    #[cfg(not(any(unix, windows)))]
    created: Option<SystemTime>,
}

impl Identity {
    fn of(metadata: &Metadata) -> Self {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        #[cfg(windows)]
        use std::os::windows::fs::MetadataExt;
        Self {
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
            #[cfg(windows)]
            created: metadata.creation_time(),
            #[cfg(windows)]
            attributes: metadata.file_attributes(),
            #[cfg(not(any(unix, windows)))]
            created: metadata.created().ok(),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct FileStamp {
    configured: PathBuf,
    real: PathBuf,
    identity: Identity,
    len: u64,
    modified: SystemTime,
    #[cfg(unix)]
    change_time: (i64, i64),
    hash: [u8; 32],
}

#[derive(Debug, PartialEq, Eq)]
struct DirectoryStamp {
    real: PathBuf,
    identity: Identity,
}

/// A revisão guarda a configuração inteira e a fotografia dos arquivos.
/// `approve` apenas consome esta revisão: o I/O fica nos workers.
#[derive(Debug)]
pub struct MergeReview {
    job: MergeJob,
    risks: Vec<MergeRisk>,
    fingerprint: MergeFingerprint,
}

/// Não clonável e sem campos públicos: não pode ser atribuído a outro job.
#[derive(Debug)]
pub struct AuthorizedMergeJob {
    job: MergeJob,
    fingerprint: MergeFingerprint,
}

#[derive(Debug, PartialEq, Eq)]
struct MergeFingerprint {
    inputs: Vec<FileStamp>,
    scripts: Vec<FileStamp>,
    directories: Vec<DirectoryStamp>,
    output_real: PathBuf,
    output_identity: Option<Identity>,
    root: PathBuf,
}

impl MergeReview {
    pub fn prepare(job: MergeJob) -> Result<Self, MergeReviewError> {
        let fingerprint = MergeFingerprint::capture(&job)?;
        let mut risks = Vec::new();
        let temporary_root = dunce::canonicalize(std::env::temp_dir()).ok();
        for input in &fingerprint.inputs {
            let sensitive = sensitive_name(&input.real)
                || input
                    .real
                    .parent()
                    .filter(|parent| {
                        !is_broad_path(parent) && parent.parent() != temporary_root.as_deref()
                    })
                    .is_some_and(sensitive_name);
            if sensitive {
                risks.push(MergeRisk {
                    file: input.configured.clone(),
                    reason: "Nome ou pasta sugere mod sensível; é um indício, não incompatibilidade comprovada.",
                });
            }
            let mut reader = BufReader::new(File::open(&input.real)?);
            if let Ok((_, index)) = DBPFReader::read_index(&mut reader) {
                if index.iter().any(|entry| entry.key.type_id == 0x0333406C) {
                    risks.push(MergeRisk {
                        file: input.configured.clone(),
                        reason: "Contém recurso de tuning/XML; revise a intenção do mod antes de unificar.",
                    });
                }
            } else {
                risks.push(MergeRisk {
                    file: input.configured.clone(),
                    reason: "Índice DBPF ilegível; este arquivo pode não entrar na unificação.",
                });
            }
        }
        for script in &fingerprint.scripts {
            risks.push(MergeRisk {
                file: script.real.clone(),
                reason: "Script .ts4script no mesmo diretório de uma entrada; associação não comprovada. O script não será unificado.",
            });
        }
        // A inspeção não deve autorizar uma fotografia anterior à sua leitura.
        if MergeFingerprint::capture(&job)? != fingerprint {
            return Err(review_error(
                "Os arquivos mudaram durante a análise; revise novamente.",
            ));
        }
        Ok(Self {
            job,
            risks,
            fingerprint,
        })
    }

    pub fn job(&self) -> &MergeJob {
        &self.job
    }
    pub fn risks(&self) -> &[MergeRisk] {
        &self.risks
    }
    pub fn requires_delete_confirmation(&self) -> bool {
        self.job.post_action == PostMergeAction::Delete
    }

    pub fn approve(self, phrase: &str) -> Result<AuthorizedMergeJob, MergeReviewError> {
        if self.requires_delete_confirmation() && phrase != "EXCLUIR ORIGINAIS" {
            return Err(review_error(
                "Digite exatamente EXCLUIR ORIGINAIS para autorizar a exclusão.",
            ));
        }
        Ok(AuthorizedMergeJob {
            job: self.job,
            fingerprint: self.fingerprint,
        })
    }
}

impl AuthorizedMergeJob {
    pub fn job(&self) -> &MergeJob {
        &self.job
    }

    fn validate(&self, created: &[DirectoryStamp]) -> Result<MergeFingerprint, MergeReviewError> {
        let current = MergeFingerprint::capture(&self.job)?;
        let expected = &self.fingerprint;
        let output_matches = current.output_identity == expected.output_identity
            || (expected.output_identity.is_none()
                && created.iter().any(|directory| {
                    directory.real == current.output_real
                        && Some(&directory.identity) == current.output_identity.as_ref()
                }));
        let directories_match = expected
            .directories
            .iter()
            .all(|directory| current.directories.contains(directory))
            && current.directories.iter().all(|directory| {
                expected.directories.contains(directory) || created.contains(directory)
            });
        if current.inputs != expected.inputs
            || current.scripts != expected.scripts
            || current.root != expected.root
            || current.output_real != expected.output_real
            || !output_matches
            || !directories_match
        {
            return Err(review_error("A seleção, os scripts, a origem ou o destino mudaram; revise novamente. Originais preservados."));
        }
        Ok(current)
    }
}

fn is_broad_path(path: &Path) -> bool {
    path.parent().is_none()
        || [Path::new("/home"), Path::new("/tmp")].contains(&path)
        || dirs::home_dir()
            .and_then(|home| dunce::canonicalize(home).ok())
            .as_deref()
            == Some(path)
        || dunce::canonicalize(std::env::temp_dir()).ok().as_deref() == Some(path)
}

fn is_link(metadata: &Metadata) -> bool {
    let link = metadata.file_type().is_symlink();
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        link || metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        link
    }
}

// Reject traversal rather than normalizing it past a link.
fn real_path(path: &Path, allow_missing: bool) -> Result<PathBuf, MergeReviewError> {
    if path.as_os_str().is_empty() || path.components().any(|part| part == Component::ParentDir) {
        return Err(review_error(
            "Caminho vazio ou com travessia de diretórios.",
        ));
    }
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut checked = PathBuf::new();
    let mut missing = false;
    let mut parts = absolute.components().peekable();
    while let Some(part) = parts.next() {
        if part == Component::CurDir {
            continue;
        }
        checked.push(part.as_os_str());
        if missing {
            continue;
        }
        match fs::symlink_metadata(&checked) {
            Ok(metadata) => {
                if is_link(&metadata)
                    || (!metadata.is_file() && !metadata.is_dir())
                    || (parts.peek().is_some() && !metadata.is_dir())
                {
                    return Err(review_error(format!(
                        "Caminho não real ou tipo inválido: {}",
                        checked.display()
                    )));
                }
            }
            Err(error) if allow_missing && error.kind() == io::ErrorKind::NotFound => {
                missing = true;
            }
            Err(error) => return Err(error.into()),
        }
    }
    if missing {
        Ok(checked)
    } else {
        Ok(dunce::canonicalize(checked)?)
    }
}

fn stamp_file(path: &Path) -> Result<FileStamp, MergeReviewError> {
    let real = real_path(path, false)?;
    let before = fs::symlink_metadata(&real)?;
    if !before.is_file() {
        return Err(review_error("A entrada não é um arquivo regular."));
    }
    let mut file = File::open(&real)?;
    let opened = file.metadata()?;
    if Identity::of(&before) != Identity::of(&opened) {
        return Err(review_error("Arquivo substituído durante a leitura."));
    }
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    let after = fs::symlink_metadata(&real)?;
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;
    if Identity::of(&before) != Identity::of(&after)
        || before.len() != after.len()
        || before.modified()? != after.modified()?
    {
        return Err(review_error("Arquivo alterado durante a leitura."));
    }
    #[cfg(unix)]
    if (before.ctime(), before.ctime_nsec()) != (after.ctime(), after.ctime_nsec()) {
        return Err(review_error("Arquivo alterado durante a leitura."));
    }
    Ok(FileStamp {
        configured: path.to_path_buf(),
        real,
        identity: Identity::of(&before),
        len: before.len(),
        modified: before.modified()?,
        #[cfg(unix)]
        change_time: (before.ctime(), before.ctime_nsec()),
        hash: hash.finalize().into(),
    })
}

fn sensitive_name(path: &Path) -> bool {
    let Some(name) = path.file_stem().and_then(|name| name.to_str()) else {
        return false;
    };
    let mut previous: Option<&str> = None;
    for token in name
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
    {
        if [
            "gameplay",
            "tuning",
            "xml",
            "override",
            "overrides",
            "replacement",
            "replacements",
            "injector",
            "mccc",
            "wicked",
            "wickedwhims",
            "wonderfulwhims",
            "basemental",
            "tmex",
            "betterexceptions",
        ]
        .iter()
        .any(|keyword| token.eq_ignore_ascii_case(keyword))
            || (previous.is_some_and(|word| word.eq_ignore_ascii_case("mc"))
                && token.eq_ignore_ascii_case("cmd"))
        {
            return true;
        }
        previous = Some(token);
    }
    false
}

impl MergeFingerprint {
    fn capture(job: &MergeJob) -> Result<Self, MergeReviewError> {
        if job.input_files.is_empty() {
            return Err(review_error("Selecione ao menos um package."));
        }
        if job.max_size_bytes == 0 {
            return Err(review_error("O limite de tamanho deve ser positivo."));
        }
        let mut inputs: Vec<FileStamp> = Vec::new();
        for path in &job.input_files {
            if !path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("package"))
            {
                return Err(review_error(format!(
                    "Somente .package pode entrar na unificação: {}",
                    path.display()
                )));
            }
            let stamp = stamp_file(path)?;
            if inputs
                .iter()
                .any(|previous| previous.real == stamp.real || previous.identity == stamp.identity)
            {
                return Err(review_error("Entradas duplicadas ou aliases ambíguos."));
            }
            inputs.push(stamp);
        }
        let paths: Vec<PathBuf> = inputs.iter().map(|input| input.real.clone()).collect();
        let root = common_ancestor(&paths).ok_or_else(|| review_error("Origem comum inválida."))?;
        if is_broad_path(&root) {
            return Err(review_error("A origem comum é ampla demais."));
        }
        validate_scope(&paths, &root)?;
        let output_real = real_path(&job.output_dir, true)?;
        if is_broad_path(&output_real) {
            return Err(review_error("O destino é amplo demais."));
        }
        let output_identity = match fs::symlink_metadata(&output_real) {
            Ok(metadata) if metadata.is_dir() => Some(Identity::of(&metadata)),
            Ok(_) => return Err(review_error("O destino não é um diretório.")),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        let parents: BTreeSet<PathBuf> = paths
            .iter()
            .filter_map(|path| path.parent().map(Path::to_path_buf))
            .collect();
        let mut script_paths = BTreeSet::new();
        for parent in &parents {
            // Only immediate siblings, never a recursive walk of a UI root.
            for entry in fs::read_dir(parent)? {
                let path = entry?.path();
                if path
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("ts4script"))
                {
                    script_paths.insert(path);
                }
            }
        }
        let scripts = script_paths
            .iter()
            .map(|path| stamp_file(path))
            .collect::<Result<Vec<_>, _>>()?;
        let mut directory_paths = BTreeSet::new();
        for path in parents.iter().chain(std::iter::once(&output_real)) {
            for ancestor in path.ancestors() {
                if let Ok(metadata) = fs::symlink_metadata(ancestor) {
                    if metadata.is_dir() {
                        directory_paths.insert(real_path(ancestor, false)?);
                    }
                }
            }
        }
        let directories = directory_paths
            .into_iter()
            .map(|real| {
                let identity = Identity::of(&fs::symlink_metadata(&real)?);
                Ok(DirectoryStamp { real, identity })
            })
            .collect::<Result<Vec<_>, MergeReviewError>>()?;
        Ok(Self {
            inputs,
            scripts,
            directories,
            output_real,
            output_identity,
            root,
        })
    }
}

fn validate_scope(originals: &[PathBuf], root: &Path) -> Result<(), MergeReviewError> {
    let root = real_path(root, false)?;
    if is_broad_path(&root) || !fs::symlink_metadata(&root)?.is_dir() {
        return Err(review_error("Raiz de segurança inválida."));
    }
    for path in originals {
        let real = real_path(path, false)?;
        if !fs::symlink_metadata(&real)?.is_file() {
            return Err(review_error("Original não é arquivo regular."));
        }
        is_safe_to_delete(path, std::slice::from_ref(&root))
            .map_err(|error| review_error(error.to_string()))?;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobStatus {
    Pending,
    Running,
    Done,
    /// Terminou, mas parte dos arquivos falhou — a pós-ação não roda.
    Partial,
    Failed,
}

impl JobStatus {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Pending => "Aguardando",
            Self::Running => "Unificando...",
            Self::Done => "Concluído",
            Self::Partial => "Parcial",
            Self::Failed => "Falhou",
        }
    }
}

/// Resultado de um job depois que a fila passou por ele.
pub struct JobOutcome {
    pub name: String,
    pub status: JobStatus,
    pub report: Option<MergeReport>,
    pub post: Option<PostMergeOutcome>,
    pub error: Option<String>,
}

/// Executa a fila em ordem, um job por vez.
///
/// `on_job` é chamado a cada mudança de estado para a UI acompanhar, e
/// `on_progress` repassa o progresso de dentro do merge em andamento.
///
/// A pós-ação só roda quando o merge do job foi **completo**: com um merge
/// parcial, apagar ou desativar os originais perderia o conteúdo dos arquivos
/// que não entraram.
pub fn run_merge_queue<J, P>(
    jobs: &[AuthorizedMergeJob],
    disabled_mgr: &DisabledManager,
    on_job: J,
    on_progress: P,
) -> Vec<JobOutcome>
where
    J: Fn(usize, JobStatus) + Send + Sync,
    P: Fn(usize, usize, usize, &str) + Send + Sync,
{
    let mut outcomes = Vec::new();
    // Only directories created by this queue may satisfy another reviewed
    // job's absent destination. An external creation still invalidates it.
    let mut created_directories = Vec::new();

    for (index, authorization) in jobs.iter().enumerate() {
        let job = authorization.job();
        on_job(index, JobStatus::Running);
        let execution_fingerprint = (|| {
            let before = authorization.validate(&created_directories)?;
            fs::create_dir_all(&job.output_dir)?;
            let fingerprint = MergeFingerprint::capture(job)?;
            // Pin the newly created destination before writing any parts.
            if fingerprint.inputs != before.inputs
                || fingerprint.scripts != before.scripts
                || !before
                    .directories
                    .iter()
                    .all(|directory| fingerprint.directories.contains(directory))
            {
                return Err(review_error("As origens mudaram antes da unificação."));
            }
            for directory in &fingerprint.directories {
                if !before.directories.contains(directory) {
                    created_directories.push(DirectoryStamp {
                        real: directory.real.clone(),
                        identity: Identity::of(&fs::symlink_metadata(&directory.real)?),
                    });
                }
            }
            Ok(fingerprint)
        })();
        let execution_fingerprint = match execution_fingerprint {
            Ok(fingerprint) => fingerprint,
            Err(error) => {
                on_job(index, JobStatus::Failed);
                outcomes.push(JobOutcome {
                    name: job.name.clone(),
                    status: JobStatus::Failed,
                    report: None,
                    post: None,
                    error: Some(error.to_string()),
                });
                continue;
            }
        };

        let task = MergeTask {
            input_files: job.input_files.clone(),
            output_dir: job.output_dir.clone(),
            max_size_bytes: job.max_size_bytes,
            name: Some(job.name.clone()),
        };

        let result = merge_sims4_packages(&task, |current, total, text| {
            on_progress(index, current, total, text)
        });

        let outcome = match result {
            Ok(report) if report.success => {
                let validation = MergeFingerprint::capture(job).and_then(|current| {
                    if current != execution_fingerprint {
                        Err(review_error("Os arquivos, scripts ou diretórios mudaram durante a unificação. Originais preservados; revise novamente."))
                    } else { Ok(()) }
                });
                if let Err(error) = validation {
                    on_job(index, JobStatus::Partial);
                    outcomes.push(JobOutcome {
                        name: job.name.clone(),
                        status: JobStatus::Partial,
                        report: Some(report),
                        post: None,
                        error: Some(error.to_string()),
                    });
                    continue;
                }
                let post = Some(apply_post_merge_action(
                    job.post_action,
                    &job.input_files,
                    &execution_fingerprint.root,
                    disabled_mgr,
                ));
                let status = match &post {
                    Some(p) if p.failed > 0 || p.error.is_some() => JobStatus::Partial,
                    _ => JobStatus::Done,
                };
                JobOutcome {
                    name: job.name.clone(),
                    status,
                    report: Some(report),
                    post,
                    error: None,
                }
            }
            Ok(report) => JobOutcome {
                name: job.name.clone(),
                status: if report.partial {
                    JobStatus::Partial
                } else {
                    JobStatus::Failed
                },
                report: Some(report),
                post: None,
                error: None,
            },
            Err(e) => JobOutcome {
                name: job.name.clone(),
                status: JobStatus::Failed,
                report: None,
                post: None,
                error: Some(e.to_string()),
            },
        };

        on_job(index, outcome.status);
        outcomes.push(outcome);
    }

    outcomes
}

/// Manter os originais junto das partes unificadas duplica todo o conteúdo
/// dentro do jogo, então esta escolha não é cosmética.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostMergeAction {
    Keep,
    Disable,
    Backup,
    Delete,
}

impl PostMergeAction {
    pub fn parse(action: &str) -> Self {
        match action {
            "disable" => Self::Disable,
            "backup" => Self::Backup,
            "delete" => Self::Delete,
            _ => Self::Keep,
        }
    }
}

/// Resultado da ação pós-merge, pronto para virar mensagem na UI.
#[derive(Debug, Default)]
pub struct PostMergeOutcome {
    pub affected: usize,
    pub failed: usize,
    pub freed_bytes: u64,
    /// Caminho do zip, quando a ação foi `Backup`.
    pub backup_path: Option<PathBuf>,
    /// Preenchido quando a ação abortou sem tocar em nada.
    pub error: Option<String>,
}

/// Aplica o destino escolhido para os arquivos originais.
///
/// `input_root` é a raiz de segurança: nada fora dela é apagado ou renomeado,
/// por mais que a lista de originais diga o contrário.
fn apply_post_merge_action(
    action: PostMergeAction,
    originals: &[PathBuf],
    input_root: &Path,
    disabled_mgr: &DisabledManager,
) -> PostMergeOutcome {
    if let Err(error) = validate_scope(originals, input_root) {
        return PostMergeOutcome {
            failed: originals.len(),
            error: Some(error.to_string()),
            ..Default::default()
        };
    }
    let allowed_roots = vec![input_root.to_path_buf()];

    match action {
        PostMergeAction::Keep => PostMergeOutcome::default(),

        PostMergeAction::Disable => {
            let mut outcome = PostMergeOutcome::default();
            for path in originals {
                match disabled_mgr.disable_mod(
                    path,
                    input_root,
                    "merge",
                    "Original substituído por package unificado",
                    &allowed_roots,
                ) {
                    Ok(_) => outcome.affected += 1,
                    Err(_) => outcome.failed += 1,
                }
            }
            outcome
        }

        PostMergeAction::Backup => {
            let stamp = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let zip_path = input_root.join(format!("originais_pre_merge_{}.zip", stamp));

            if let Err(e) = create_zip_from_files(originals, input_root, &zip_path) {
                // O zip é a única cópia de segurança: sem ele, não apagamos nada.
                return PostMergeOutcome {
                    error: Some(e.to_string()),
                    ..Default::default()
                };
            }

            if let Err(error) = validate_scope(originals, input_root) {
                return PostMergeOutcome {
                    failed: originals.len(),
                    backup_path: Some(zip_path),
                    error: Some(error.to_string()),
                    ..Default::default()
                };
            }
            let removal = remove_files(originals, &allowed_roots);
            PostMergeOutcome {
                affected: removal.removed,
                failed: removal.failures.len(),
                freed_bytes: removal.freed_bytes,
                backup_path: Some(zip_path),
                error: None,
            }
        }

        PostMergeAction::Delete => {
            let RemovalOutcome {
                removed,
                freed_bytes,
                failures,
            } = remove_files(originals, &allowed_roots);
            PostMergeOutcome {
                affected: removed,
                failed: failures.len(),
                freed_bytes,
                backup_path: None,
                error: None,
            }
        }
    }
}

/// Pasta comum mais profunda que contém todos os caminhos. Serve de raiz de
/// segurança para as ações pós-merge.
pub fn common_ancestor(paths: &[PathBuf]) -> Option<PathBuf> {
    let mut iter = paths.iter().filter_map(|p| p.parent());
    let mut common = iter.next()?.to_path_buf();
    for parent in iter {
        while !parent.starts_with(&common) {
            if !common.pop() {
                return None;
            }
        }
    }
    Some(common)
}

/// Compacta uma lista específica de arquivos, preservando o caminho relativo
/// a `base_dir`.
fn create_zip_from_files(
    files: &[PathBuf],
    base_dir: &Path,
    zip_path: &Path,
) -> Result<(), MergeReviewError> {
    validate_scope(files, base_dir)?;
    let before = files
        .iter()
        .map(|path| stamp_file(path))
        .collect::<Result<Vec<_>, _>>()?;
    let base = real_path(base_dir, false)?;
    let base_identity = Identity::of(&fs::symlink_metadata(&base)?);
    if fs::symlink_metadata(zip_path).is_ok() {
        return Err(review_error(
            "O backup já existe; nenhum original foi removido.",
        ));
    }
    let mut temp = tempfile::NamedTempFile::new_in(&base)?;
    {
        let mut zip = zip::ZipWriter::new(temp.as_file_mut());
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for source in &before {
            let name = source
                .real
                .strip_prefix(&base)
                .map_err(|_| review_error("Arquivo fora do escopo do backup."))?;
            zip.start_file(name.to_string_lossy().replace('\\', "/"), options)
                .map_err(|error| review_error(error.to_string()))?;
            let mut reader = File::open(&source.real)?;
            if Identity::of(&reader.metadata()?) != source.identity {
                return Err(review_error("Original substituído antes do backup."));
            }
            io::copy(&mut reader, &mut zip)?;
        }
        zip.finish()
            .map_err(|error| review_error(error.to_string()))?;
    }
    temp.as_file_mut().flush()?;
    temp.as_file().sync_all()?;
    {
        let mut archive = zip::ZipArchive::new(File::open(temp.path())?)
            .map_err(|error| review_error(error.to_string()))?;
        if archive.len() != before.len() {
            return Err(review_error("Backup incompleto."));
        }
        let mut buffer = [0u8; 64 * 1024];
        for (index, source) in before.iter().enumerate() {
            let mut entry = archive
                .by_index(index)
                .map_err(|error| review_error(error.to_string()))?;
            let mut hash = Sha256::new();
            loop {
                let count = entry.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                hash.update(&buffer[..count]);
            }
            let digest: [u8; 32] = hash.finalize().into();
            if digest != source.hash || entry.size() != source.len {
                return Err(review_error(
                    "Conteúdo do backup não corresponde aos originais.",
                ));
            }
        }
    }
    validate_scope(files, base_dir)?;
    if files
        .iter()
        .map(|path| stamp_file(path))
        .collect::<Result<Vec<_>, _>>()?
        != before
        || real_path(base_dir, false)? != base
        || Identity::of(&fs::symlink_metadata(&base)?) != base_identity
    {
        return Err(review_error(
            "Originais alterados durante o backup; nenhum foi removido.",
        ));
    }
    temp.persist_noclobber(zip_path)
        .map_err(|error| review_error(error.error.to_string()))?;
    Ok(())
}

fn validate_directory(path: &Path, expected: &DirectoryStamp) -> Result<(), MergeReviewError> {
    let real = real_path(path, false)?;
    let metadata = fs::symlink_metadata(&real)?;
    if !metadata.is_dir() || real != expected.real || Identity::of(&metadata) != expected.identity {
        return Err(review_error(
            "O destino mudou durante a unificação; originais preservados.",
        ));
    }
    Ok(())
}

fn write_package_chunk(
    part_path: &Path,
    resources: &[PackageResource],
    output: &DirectoryStamp,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    validate_directory(
        part_path
            .parent()
            .ok_or_else(|| review_error("Destino inválido."))?,
        output,
    )?;
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(part_path)?;
    let mut writer = BufWriter::with_capacity(4 * 1024 * 1024, file);
    DBPFWriter::write_package(&mut writer, resources)?;
    let inner = writer.into_inner()?;
    inner.sync_all()?;
    Ok(())
}

fn chrono_like_timestamp() -> String {
    let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default();
    format!("{}", now.as_secs())
}

/// Deixa o nome do grupo utilizável como nome de arquivo, sem descaracterizá-lo.
///
/// Só os separadores de caminho e os controles saem; acento, espaço e emoji
/// ficam, porque é assim que o usuário nomeou a pasta de onde os packages vieram.
fn sanitize_stem(raw: &str) -> String {
    let limpo: String = raw
        .chars()
        .map(|c| if c.is_control() || "/\\:*?\"<>|".contains(c) { '_' } else { c })
        .collect();
    limpo.trim().trim_matches('_').trim().to_string()
}

/// Prefixo livre para os arquivos desta unificação dentro de `output_dir`.
///
/// Um merge produz `<prefixo>_PartNNN.package` e `<prefixo>_report.json`; o
/// prefixo precisa ser único **antes** da primeira parte ser escrita, senão uma
/// tarefa sobrescreve o resultado de outra que foi para a mesma pasta.
fn unique_output_stem(output_dir: &Path, name: Option<&str>) -> String {
    let base = name
        .map(sanitize_stem)
        .filter(|s| !s.is_empty())
        .map(|s| format!("{}_Merged", s))
        .unwrap_or_else(|| format!("Merged_Content_{}", chrono_like_timestamp()));

    let livre = |stem: &str| !output_dir.join(format!("{}_Part001.package", stem)).exists();
    if livre(&base) {
        return base;
    }
    for n in 2..1000 {
        let tentativa = format!("{}_{}", base, n);
        if livre(&tentativa) {
            return tentativa;
        }
    }
    format!("{}_{}", base, chrono_like_timestamp())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::dbpf::ResourceKey;
    use tempfile::tempdir;

    #[test]
    fn test_merger_tgi_overwrite() {
        let dir = tempdir().unwrap();
        let pkg1_path = dir.path().join("mod1.package");
        let pkg2_path = dir.path().join("mod2.package");
        let out_dir = dir.path().join("output");

        let key_shared = ResourceKey {
            type_id: 0x11111111,
            group_id: 0x0,
            instance_ex: 0x0,
            instance_low: 0x1,
        };

        let res1 = PackageResource {
            key: key_shared,
            data: b"Old Version Content from Mod1".to_vec(),
            mem_size: 29,
            compressed: 0,
        };

        let res2 = PackageResource {
            key: key_shared,
            data: b"NEW UPDATED VERSION Content from Mod2".to_vec(),
            mem_size: 37,
            compressed: 0,
        };

        let mut f1 = File::create(&pkg1_path).unwrap();
        DBPFWriter::write_package(&mut f1, &[res1]).unwrap();

        let mut f2 = File::create(&pkg2_path).unwrap();
        DBPFWriter::write_package(&mut f2, &[res2]).unwrap();

        let task = MergeTask {
            input_files: vec![pkg1_path, pkg2_path],
            output_dir: out_dir.clone(),
            max_size_bytes: 1_000_000_000,
            name: None,
        };

        let report = merge_sims4_packages(&task, |_, _, _| {}).unwrap();
        assert!(report.success);
        assert_eq!(report.output_packages.len(), 1);

        let merged_file_path = PathBuf::from(&report.output_packages[0]);
        let mut f_merged = File::open(&merged_file_path).unwrap();
        let (_hdr, index) = DBPFReader::read_index(&mut f_merged).unwrap();

        assert_eq!(index.len(), 1);
        assert_eq!(index[0].key, key_shared);
        assert_eq!(index[0].file_size, 37); // Length of NEW UPDATED VERSION from Mod2
    }

    #[test]
    fn scope_failure_aborts_every_post_action_before_touching_any_original() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("source");
        fs::create_dir(&root).unwrap();
        let inside = root.join("inside.package");
        let outside = dir.path().join("outside.package");
        fs::write(&inside, b"inside fixture").unwrap();
        fs::write(&outside, b"outside fixture").unwrap();
        let manager = DisabledManager::new(&dir.path().join("config"));
        for action in [
            PostMergeAction::Keep,
            PostMergeAction::Disable,
            PostMergeAction::Backup,
            PostMergeAction::Delete,
        ] {
            let outcome = apply_post_merge_action(
                action,
                &[inside.clone(), outside.clone()],
                &root,
                &manager,
            );
            assert_eq!(outcome.affected, 0);
            assert_eq!(outcome.failed, 2);
            assert!(outcome.error.is_some());
            assert_eq!(fs::read(&inside).unwrap(), b"inside fixture");
            assert_eq!(fs::read(&outside).unwrap(), b"outside fixture");
            assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
        }
    }

    #[test]
    fn backup_publish_collision_never_truncates_existing_archive() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("source");
        fs::create_dir(&source).unwrap();
        let original = source.join("a.package");
        let destination = source.join("previous.zip");
        fs::write(&original, b"original fixture").unwrap();
        fs::write(&destination, b"previous archive").unwrap();
        assert!(create_zip_from_files(&[original.clone()], &source, &destination).is_err());
        assert_eq!(fs::read(&destination).unwrap(), b"previous archive");
        assert_eq!(fs::read(&original).unwrap(), b"original fixture");
        assert_eq!(fs::read_dir(&source).unwrap().count(), 2);
    }
}
