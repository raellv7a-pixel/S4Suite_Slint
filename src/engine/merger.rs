use crate::core::safety::{same_file_identity, AnchoredDir, ReadLease};
use crate::engine::dbpf::{DBPFReader, DBPFWriter, PackageResource, ResourceKey};
use crate::engine::disabled::DisabledManager;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergeReport {
    pub success: bool,
    pub partial: bool,
    pub total_files_found: usize,
    pub success_count: usize,
    pub failed_files: Vec<(String, String)>,
    pub output_packages: Vec<String>,
    #[serde(skip, default)]
    pub original_snapshots: Vec<OriginalSnapshot>,
}

/// In-memory evidence only: deserializing a report never authorizes destruction.
#[derive(Debug, Clone)]
pub struct OriginalSnapshot {
    path: PathBuf,
    metadata: fs::Metadata,
    sha256: [u8; 32],
    outputs: Arc<Vec<(PathBuf, [u8; 32], fs::Metadata)>>,
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
    let output_dir = AnchoredDir::create(&task.output_dir)?;
    let workspace = tempfile::Builder::new()
        .prefix(".s4merge-")
        .tempdir_in(output_dir.path())?;
    let workspace_dir =
        output_dir.child(workspace.path().file_name().ok_or("invalid workspace")?)?;

    let total_files = task.input_files.len();
    if total_files == 0 {
        return Ok(MergeReport {
            success: false,
            partial: false,
            total_files_found: 0,
            success_count: 0,
            failed_files: Vec::new(),
            output_packages: Vec::new(),
            original_snapshots: Vec::new(),
        });
    }

    let mut original_snapshots = Vec::new();
    let mut file_cache: HashMap<PathBuf, BufReader<File>> = HashMap::new();
    let mut seen_inputs = HashSet::new();
    // O prefixo é resolvido **antes** de escrever qualquer parte: duas tarefas
    // da fila que terminam no mesmo segundo produziam o mesmo nome, e a segunda
    // gravava por cima da primeira — com os originais já consumidos pela
    // pós-ação, o conteúdo da primeira ia embora sem nenhum erro na tela.
    let stem = unique_output_stem(&output_dir.path(), task.name.as_deref());
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

        if !seen_inputs.insert(crate::core::safety::normalize_path(fpath)) {
            failed_files.push((fpath.display().to_string(), "Entrada duplicada".into()));
            continue;
        }
        match snapshot_original(fpath) {
            Ok((snapshot, file)) => {
                let mut reader = BufReader::with_capacity(512 * 1024, file);
                match DBPFReader::read_index(&mut reader) {
                    Ok((_header, index)) => {
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
                        original_snapshots.push(snapshot);
                        file_cache.insert(fpath.clone(), reader);
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
            original_snapshots,
        });
    }
    // Sort resources by TGI for engine load times
    let mut sorted_keys: Vec<ResourceKey> = global_resources.keys().copied().collect();
    sorted_keys.sort();

    let mut chunk_index = 1;
    let mut current_chunk_resources: Vec<PackageResource> = Vec::new();
    let mut current_chunk_bytes = 0u64;
    let mut output_packages = Vec::new();
    let mut pending_chunks: Vec<(PathBuf, PathBuf)> = Vec::new();

    let safe_limit = ((task.max_size_bytes as f64) * 0.90) as u64;

    let write_result = (|| -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
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

            let mut data = vec![0u8; *size as usize];
            let mut read_failed = false;
            if let Some(reader) = file_cache.get_mut(src_path) {
                if let Err(e) = reader.seek(SeekFrom::Start(*offset as u64)) {
                    let err_msg = format!("falha no seek do recurso {}: {}", key, e);
                    if !failed_files
                        .iter()
                        .any(|(p, _)| p == &src_path.display().to_string())
                    {
                        failed_files.push((src_path.display().to_string(), err_msg));
                        success_count = success_count.saturating_sub(1);
                    }
                    read_failed = true;
                } else if let Err(e) = reader.read_exact(&mut data) {
                    let err_msg = format!(
                        "falha na leitura do recurso {}: arquivo truncado ou erro de I/O ({})",
                        key, e
                    );
                    if !failed_files
                        .iter()
                        .any(|(p, _)| p == &src_path.display().to_string())
                    {
                        failed_files.push((src_path.display().to_string(), err_msg));
                        success_count = success_count.saturating_sub(1);
                    }
                    read_failed = true;
                }
            } else {
                read_failed = true;
            }

            if read_failed {
                continue;
            }

            let res_len = data.len() as u64;
            if current_chunk_bytes + res_len > safe_limit && !current_chunk_resources.is_empty() {
                let part_name = format!("{}_Part{:03}.package", stem, chunk_index);
                let tmp_name = format!(".{}_Part{:03}.package.s4tmp", stem, chunk_index);
                let part_path = output_dir.path().join(&part_name);
                let tmp_path = workspace.path().join(&tmp_name);

                write_package_chunk(&tmp_path, &current_chunk_resources)?;
                validate_package_chunk(&tmp_path, current_chunk_resources.len())?;
                pending_chunks.push((tmp_path, part_path));

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
            let tmp_name = format!(".{}_Part{:03}.package.s4tmp", stem, chunk_index);
            let part_path = output_dir.path().join(&part_name);
            let tmp_path = workspace.path().join(&tmp_name);

            write_package_chunk(&tmp_path, &current_chunk_resources)?;
            validate_package_chunk(&tmp_path, current_chunk_resources.len())?;
            pending_chunks.push((tmp_path, part_path));
        }

        Ok(())
    })();

    if let Err(e) = write_result {
        return match workspace.close() {
            Ok(()) => Err(e),
            Err(cleanup) => Err(format!("{e}; limpeza do workspace falhou: {cleanup}").into()),
        };
    }

    let hashes = match publish_chunks(&pending_chunks, &workspace_dir, &output_dir, |_, _| Ok(())) {
        Ok(hashes) => hashes,
        Err((error, preserve_workspace)) => {
            if preserve_workspace {
                let recovery = workspace.keep();
                return Err(
                    format!("{error}; arquivos preservados em {}", recovery.display()).into(),
                );
            }
            return match workspace.close() {
                Ok(()) => Err(error.into()),
                Err(cleanup) => {
                    Err(format!("{error}; limpeza do workspace falhou: {cleanup}").into())
                }
            };
        }
    };
    let mut outputs = Vec::new();
    for ((_, final_path), hash) in pending_chunks.iter().zip(hashes) {
        let name = final_path.file_name().ok_or("invalid output name")?;
        let path = task.output_dir.join(name);
        let file = output_dir.open_regular(name)?;
        let meta = file.metadata()?;
        outputs.push((path.clone(), hash, meta));
        output_packages.push(path.display().to_string());
    }
    let outputs = Arc::new(outputs);
    for snapshot in &mut original_snapshots {
        snapshot.outputs = Arc::clone(&outputs);
    }

    let report = MergeReport {
        success: failed_files.is_empty()
            && success_count == total_files
            && !output_packages.is_empty(),
        partial: !failed_files.is_empty() && success_count > 0 && !output_packages.is_empty(),
        total_files_found: total_files,
        success_count,
        failed_files,
        output_packages,
        original_snapshots,
    };
    let report_name = format!("{}_report.json", stem);
    let tmp_report = workspace.path().join(&report_name);
    let json = serde_json::to_vec_pretty(&report)?;
    fs::write(&tmp_report, json)?;
    let report_publish_res = workspace_dir.publish_noreplace(
        OsStr::new(&report_name),
        &output_dir,
        OsStr::new(&report_name),
    );
    if let Err(error) = report_publish_res {
        // Rollback das partes DBPF já publicadas: evita que partes órfãs ativas
        // sobrevivam no diretório de saída sem relatório JSON correspondente.
        let mut rollback_failures = Vec::new();
        for (_, final_path) in pending_chunks.iter().rev() {
            if let Some(name) = final_path.file_name() {
                let recovery_name = format!("rollback-part-{}", name.to_string_lossy());
                if let Err(rb_err) =
                    output_dir.publish_noreplace(name, &workspace_dir, OsStr::new(&recovery_name))
                {
                    rollback_failures.push(format!("{}: {rb_err}", final_path.display()));
                }
            }
        }
        if rollback_failures.is_empty() {
            let _ = workspace.close();
            return Err(format!(
                "Falha na publicação do relatório ({error}); partes publicadas sofreram rollback com sucesso"
            )
            .into());
        } else {
            let recovery = workspace.keep();
            return Err(format!(
                "Falha na publicação do relatório ({error}); rollback parcial das partes falhou: {}; arquivos preservados em {}",
                rollback_failures.join("; "),
                recovery.display()
            )
            .into());
        }
    }
    output_dir.sync_all()?;
    workspace.close()?;

    Ok(report)
}

fn hash_reader(reader: &mut impl Read) -> io::Result<[u8; 32]> {
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 128 * 1024];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            return Ok(hash.finalize().into());
        }
        hash.update(&buffer[..count]);
    }
}

fn unchanged_metadata(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    if !same_file_identity(a, b) || a.len() != b.len() || a.modified().ok() != b.modified().ok() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        a.ctime() == b.ctime() && a.ctime_nsec() == b.ctime_nsec()
    }
    #[cfg(not(unix))]
    {
        false
    }
}

fn open_original(path: &Path) -> io::Result<(AnchoredDir, File)> {
    let parent = AnchoredDir::open(path.parent().unwrap_or(Path::new(".")))?;
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::other("Original sem nome"))?;
    let file = parent.open_regular(name)?;
    Ok((parent, file))
}

fn snapshot_original(path: &Path) -> io::Result<(OriginalSnapshot, File)> {
    snapshot_original_with(path, || Ok(()))
}

fn snapshot_original_with(
    path: &Path,
    after_copy: impl FnOnce() -> io::Result<()>,
) -> io::Result<(OriginalSnapshot, File)> {
    let (_parent, mut source) = open_original(path)?;
    let before = source.metadata()?;
    let mut snapshot = tempfile::tempfile()?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 128 * 1024];
    loop {
        let count = source.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        snapshot.write_all(&buffer[..count])?;
        hash.update(&buffer[..count]);
    }
    let sha256: [u8; 32] = hash.finalize().into();
    after_copy()?;
    source.seek(SeekFrom::Start(0))?;
    if hash_reader(&mut source)? != sha256 || !unchanged_metadata(&before, &source.metadata()?) {
        return Err(io::Error::other(
            "Original alterado durante captura do merge",
        ));
    }
    snapshot.seek(SeekFrom::Start(0))?;
    Ok((
        OriginalSnapshot {
            path: path.to_path_buf(),
            metadata: before,
            sha256,
            outputs: Arc::default(),
        },
        snapshot,
    ))
}

/// The hook is private and allows deterministic publication/rollback faults in unit tests.
fn publish_chunks(
    chunks: &[(PathBuf, PathBuf)],
    workspace: &AnchoredDir,
    output: &AnchoredDir,
    mut before_publish: impl FnMut(usize, &Path) -> io::Result<()>,
) -> Result<Vec<[u8; 32]>, (io::Error, bool)> {
    let mut published: Vec<(&Path, fs::Metadata, [u8; 32])> = Vec::new();
    for (index, (temporary, destination)) in chunks.iter().enumerate() {
        let result = (|| -> io::Result<()> {
            before_publish(index, destination)?;
            let mut file = workspace.open_regular(
                temporary
                    .file_name()
                    .ok_or_else(|| io::Error::other("temporary name"))?,
            )?;
            let metadata = file.metadata()?;
            let hash = hash_reader(&mut file)?;
            workspace.publish_noreplace(
                temporary
                    .file_name()
                    .ok_or_else(|| io::Error::other("temporary name"))?,
                output,
                destination
                    .file_name()
                    .ok_or_else(|| io::Error::other("destination name"))?,
            )?;
            published.push((destination, metadata, hash));
            Ok(())
        })();
        if let Err(error) = result {
            let mut failures = Vec::new();
            for (rollback_index, (path, metadata, hash)) in published.iter().enumerate().rev() {
                let name = path.file_name().expect("validated name");
                let recovery_name = format!("rollback-{rollback_index}");
                let recovery_name = OsStr::new(&recovery_name);
                let rollback = (|| -> io::Result<()> {
                    output.publish_noreplace(name, workspace, recovery_name)?;
                    let verified = (|| -> io::Result<bool> {
                        let mut recovered = workspace.open_regular(recovery_name)?;
                        Ok(same_file_identity(metadata, &recovered.metadata()?)
                            && hash_reader(&mut recovered)? == *hash)
                    })();
                    if !matches!(verified, Ok(true)) {
                        workspace.publish_noreplace(recovery_name, output, name)?;
                        return Err(io::Error::other(
                            "Arquivo publicado mudou; rollback recusado",
                        ));
                    }
                    workspace.remove_file(recovery_name)
                })();
                if let Err(rollback_error) = rollback {
                    failures.push(format!("{}: {rollback_error}", path.display()));
                }
            }
            if failures.is_empty() {
                return Err((error, false));
            }
            return Err((
                io::Error::other(format!("{error}; rollback falhou: {}", failures.join("; "))),
                true,
            ));
        }
    }
    Ok(published.into_iter().map(|(_, _, hash)| hash).collect())
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
pub struct MergeJob {
    pub name: String,
    pub input_files: Vec<PathBuf>,
    pub output_dir: PathBuf,
    pub max_size_bytes: u64,
    pub post_action: PostMergeAction,
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
    jobs: &[MergeJob],
    disabled_mgr: &DisabledManager,
    on_job: J,
    on_progress: P,
) -> Vec<JobOutcome>
where
    J: Fn(usize, JobStatus) + Send + Sync,
    P: Fn(usize, usize, usize, &str) + Send + Sync,
{
    let mut outcomes = Vec::new();

    for (index, job) in jobs.iter().enumerate() {
        on_job(index, JobStatus::Running);

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
            Ok(report)
                if report.success
                    && report.failed_files.is_empty()
                    && !report.partial
                    && !report.output_packages.is_empty() =>
            {
                let post = common_ancestor(&job.input_files).map(|root| {
                    apply_post_merge_action_validated(
                        job.post_action,
                        &report,
                        &job.input_files,
                        &root,
                        disabled_mgr,
                    )
                });
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
/// Executa a ação pós-merge somente após verificar que o relatório é de sucesso integral.
pub fn apply_post_merge_action_validated(
    action: PostMergeAction,
    report: &MergeReport,
    originals: &[PathBuf],
    input_root: &Path,
    disabled_mgr: &DisabledManager,
) -> PostMergeOutcome {
    if action == PostMergeAction::Keep {
        return PostMergeOutcome::default();
    }
    match post_merge_transaction(action, report, originals, input_root, disabled_mgr, || {
        Ok(())
    }) {
        Ok(outcome) => outcome,
        Err(error) => PostMergeOutcome {
            error: Some(format!("Ação pós-merge bloqueada: {error}")),
            ..Default::default()
        },
    }
}

struct CapturedOriginal<'a> {
    evidence: &'a OriginalSnapshot,
    parent: AnchoredDir,
    lease: ReadLease,
    staged_name: String,
    staged: bool,
}

fn verify_published_outputs(outputs: &[(PathBuf, [u8; 32], fs::Metadata)]) -> io::Result<()> {
    for (path, expected_hash, expected_meta) in outputs {
        let (_, mut file) = open_original(path).map_err(|e| {
            io::Error::other(format!(
                "Package publicado '{}' não pôde ser verificado no caminho de saída: {e}",
                path.display()
            ))
        })?;
        let current_meta = file.metadata()?;
        if !same_file_identity(expected_meta, &current_meta) {
            return Err(io::Error::other(format!(
                "Package publicado '{}' foi desvinculado ou substituído no caminho de saída",
                path.display()
            )));
        }
        if !unchanged_metadata(expected_meta, &current_meta) {
            return Err(io::Error::other(format!(
                "Package publicado '{}' teve metadados modificados no caminho de saída",
                path.display()
            )));
        }
        let current_hash = hash_reader(&mut file)?;
        if current_hash != *expected_hash {
            return Err(io::Error::other(format!(
                "Package publicado '{}' teve conteúdo alterado",
                path.display()
            )));
        }
    }
    Ok(())
}

fn post_merge_transaction(
    action: PostMergeAction,
    report: &MergeReport,
    originals: &[PathBuf],
    input_root: &Path,
    disabled_mgr: &DisabledManager,
    after_validation: impl FnOnce() -> io::Result<()>,
) -> io::Result<PostMergeOutcome> {
    post_merge_transaction_with(
        action,
        report,
        originals,
        input_root,
        disabled_mgr,
        after_validation,
        |_, _| Ok(()),
    )
}

pub(crate) fn post_merge_transaction_with(
    action: PostMergeAction,
    report: &MergeReport,
    originals: &[PathBuf],
    input_root: &Path,
    disabled_mgr: &DisabledManager,
    after_validation: impl FnOnce() -> io::Result<()>,
    mut before_original_action: impl FnMut(usize, &Path) -> io::Result<()>,
) -> io::Result<PostMergeOutcome> {
    if !report.success
        || report.partial
        || !report.failed_files.is_empty()
        || originals.is_empty()
        || report.success_count != originals.len()
        || report.total_files_found != originals.len()
        || report.original_snapshots.len() != originals.len()
    {
        return Err(io::Error::other(
            "Relatório sem evidência de sucesso integral",
        ));
    }
    let authorized: HashSet<&Path> = originals.iter().map(PathBuf::as_path).collect();
    let processed: HashSet<&Path> = report
        .original_snapshots
        .iter()
        .map(|s| s.path.as_path())
        .collect();
    if authorized.len() != originals.len()
        || processed.len() != originals.len()
        || authorized != processed
    {
        return Err(io::Error::other(
            "Originais autorizados não correspondem exatamente aos processados",
        ));
    }
    let outputs = &report.original_snapshots[0].outputs;
    if outputs.is_empty()
        || outputs.len() != report.output_packages.len()
        || !outputs
            .iter()
            .zip(&report.output_packages)
            .all(|((path, _, _), expected)| path.to_string_lossy() == *expected)
        || !report
            .original_snapshots
            .iter()
            .all(|s| Arc::ptr_eq(&s.outputs, outputs))
    {
        return Err(io::Error::other(
            "Evidência das partes publicadas ausente ou divergente",
        ));
    }
    let root = AnchoredDir::open(input_root)?;
    let _operation_lock = root.lock_exclusive()?;
    let normalized_root = crate::core::safety::normalize_path(input_root);
    let mut output_leases = Vec::new();
    for (path, expected_hash, expected_meta) in outputs.iter() {
        let (_, file) = open_original(path)?;
        let mut lease = ReadLease::acquire(file)?;
        if !same_file_identity(expected_meta, &lease.file.metadata()?) {
            return Err(io::Error::other(format!(
                "Package publicado '{}' foi substituído antes da aquisição da lease",
                path.display()
            )));
        }
        if hash_reader(&mut lease.file)? != *expected_hash {
            return Err(io::Error::other("Parte publicada ausente ou modificada"));
        }
        lease.check()?;
        output_leases.push(lease);
    }
    verify_published_outputs(outputs)?;
    let mut captured = Vec::new();
    for (index, evidence) in report.original_snapshots.iter().enumerate() {
        let normalized = crate::core::safety::normalize_path(&evidence.path);
        if normalized == normalized_root || !normalized.starts_with(&normalized_root) {
            return Err(io::Error::other("Original fora da raiz autorizada"));
        }
        let (parent, file) = open_original(&evidence.path)?;
        let mut lease = ReadLease::acquire(file)?;
        if !unchanged_metadata(&evidence.metadata, &lease.file.metadata()?)
            || hash_reader(&mut lease.file)? != evidence.sha256
        {
            return Err(io::Error::other(format!(
                "Original '{}' modificado após a realização do merge",
                evidence.path.display()
            )));
        }
        lease.check()?;
        captured.push(CapturedOriginal {
            evidence,
            parent,
            lease,
            staged_name: format!("{index}.original"),
            staged: false,
        });
    }
    after_validation()?;
    let quarantine = tempfile::Builder::new()
        .prefix(".s4-post-merge-")
        .tempdir_in(root.path())?;
    let quarantine_dir = root.child(
        quarantine
            .path()
            .file_name()
            .ok_or_else(|| io::Error::other("quarantine name"))?,
    )?;
    let mut outcome = PostMergeOutcome::default();
    let result = (|| -> io::Result<()> {
        // Capture names before final validation. Never unlink the user-facing
        // path after hashing: a replacement is restored, never destroyed.
        for original in &mut captured {
            original.parent.publish_noreplace(
                original
                    .evidence
                    .path
                    .file_name()
                    .ok_or_else(|| io::Error::other("original name"))?,
                &quarantine_dir,
                OsStr::new(&original.staged_name),
            )?;
            original.staged = true;
        }
        for original in &mut captured {
            original.lease.check()?;
            let mut file = quarantine_dir.open_regular(OsStr::new(&original.staged_name))?;
            if !same_file_identity(&original.evidence.metadata, &file.metadata()?)
                || hash_reader(&mut file)? != original.evidence.sha256
            {
                return Err(io::Error::other(
                    "Original substituído entre validação e pós-ação",
                ));
            }
        }
        verify_published_outputs(outputs)?;
        for lease in &output_leases {
            lease.check()?;
        }
        if action == PostMergeAction::Backup {
            outcome.backup_path = Some(create_zip_from_captured(
                &mut captured,
                input_root,
                &root,
                &quarantine_dir,
                |_| Ok(()),
            )?);
        }
        for (idx, original) in captured.iter_mut().enumerate() {
            before_original_action(idx, &original.evidence.path)?;
            verify_published_outputs(outputs)?;
            for lease in &output_leases {
                lease.check()?;
            }
            original.lease.check()?;
            match action {
                PostMergeAction::Disable => {
                    let name = original
                        .evidence
                        .path
                        .file_name()
                        .ok_or_else(|| io::Error::other("original name"))?;
                    let disabled_name = format!("{}.disabled", name.to_string_lossy());
                    let destination = loop {
                        let candidate = crate::core::safety::unique_dest_path(
                            &original.parent.path(),
                            &disabled_name,
                        );
                        let candidate_name = candidate
                            .file_name()
                            .ok_or_else(|| io::Error::other("disabled name"))?;
                        match quarantine_dir.publish_noreplace(
                            OsStr::new(&original.staged_name),
                            &original.parent,
                            candidate_name,
                        ) {
                            Ok(()) => break original.evidence.path.with_file_name(candidate_name),
                            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                            Err(error) => return Err(error),
                        }
                    };
                    original.staged = false;
                    outcome.affected += 1;
                    disabled_mgr.record_disabled(
                        &original.evidence.path,
                        &destination,
                        "merge",
                        "Original substituído por package unificado",
                    )?;
                }
                PostMergeAction::Delete | PostMergeAction::Backup => {
                    quarantine_dir.remove_file(OsStr::new(&original.staged_name))?;
                    original.staged = false;
                    outcome.affected += 1;
                    outcome.freed_bytes += original.evidence.metadata.len();
                }
                PostMergeAction::Keep => {}
            }
        }
        Ok(())
    })();
    if let Err(error) = result {
        let mut failures = Vec::new();
        let mut restored_count = 0;
        for original in captured.iter_mut().rev().filter(|o| o.staged) {
            let name = original
                .evidence
                .path
                .file_name()
                .expect("validated original name");
            match quarantine_dir.publish_noreplace(
                OsStr::new(&original.staged_name),
                &original.parent,
                name,
            ) {
                Ok(()) => {
                    restored_count += 1;
                }
                Err(restore) => {
                    failures.push(format!("{}: {restore}", original.evidence.path.display()));
                }
            }
        }
        outcome.failed = originals.len().saturating_sub(outcome.affected);
        if failures.is_empty() {
            if outcome.affected > 0 {
                outcome.error = Some(format!(
                    "Falha parcial durante pós-ação: {} arquivo(s) foram processados irreversivelmente antes do erro; {} arquivo(s) restantes foram restaurados da quarentena; erro: {error}",
                    outcome.affected,
                    restored_count
                ));
            } else {
                outcome.error = Some(format!(
                    "Ação pós-merge abortada; originais restaurados: {error}"
                ));
            }
            quarantine.close()?;
        } else {
            let recovery = quarantine.keep();
            outcome.error = Some(format!(
                "Falha na pós-ação ({error}); {} arquivo(s) processados irreversivelmente; restauração de {} arquivo(s) falhou: {}; arquivos preservados em {}",
                outcome.affected,
                failures.len(),
                failures.join("; "),
                recovery.display()
            ));
        }
        return Ok(outcome);
    }
    quarantine.close()?;
    Ok(outcome)
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

/// ZIP bytes are streamed from leased, captured originals, then verified against
/// merge-time SHA-256 before no-replace publication. No original is deleted here.
fn create_zip_from_captured(
    originals: &mut [CapturedOriginal<'_>],
    base_dir: &Path,
    output: &AnchoredDir,
    workspace: &AnchoredDir,
    mut before_publish: impl FnMut(&Path) -> io::Result<()>,
) -> io::Result<PathBuf> {
    let temporary_name = OsStr::new("originals.zip");
    let file = workspace.create_new(temporary_name)?;
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for original in originals.iter_mut() {
        original.lease.check()?;
        let name = original
            .evidence
            .path
            .strip_prefix(base_dir)
            .map_err(io::Error::other)?;
        zip.start_file(name.to_string_lossy(), options)?;
        original.lease.file.seek(SeekFrom::Start(0))?;
        io::copy(&mut original.lease.file, &mut zip)?;
    }
    zip.finish()?.sync_all()?;
    let mut archive = zip::ZipArchive::new(workspace.open_regular(temporary_name)?)?;
    if archive.len() != originals.len() {
        return Err(io::Error::other("Backup incompleto"));
    }
    for (index, original) in originals.iter().enumerate() {
        let mut entry = archive.by_index(index)?;
        if hash_reader(&mut entry)? != original.evidence.sha256 {
            return Err(io::Error::other(
                "SHA-256 do backup difere do original utilizado no merge",
            ));
        }
        original.lease.check()?;
    }
    drop(archive);
    let zip_name = format!("originais_pre_merge_{}.zip", chrono_like_timestamp());
    loop {
        let path = crate::core::safety::unique_dest_path(&output.path(), &zip_name);
        let name = path
            .file_name()
            .ok_or_else(|| io::Error::other("backup name"))?;
        before_publish(&path)?;
        match workspace.publish_noreplace(temporary_name, output, name) {
            Ok(()) => {
                output.sync_all()?;
                return Ok(base_dir.join(name));
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
}

fn validate_package_chunk(
    part_path: &Path,
    expected_count: usize,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut file = File::open(part_path)?;
    let (header, index) = DBPFReader::read_index(&mut file)?;

    if header.major != 2 {
        return Err(format!("Cabeçalho DBPF inválido: versão major {}", header.major).into());
    }
    if index.len() != expected_count {
        return Err(format!(
            "Inconsistência de recursos na parte gerada: esperado {} recursos, encontrado {}",
            expected_count,
            index.len()
        )
        .into());
    }

    let file_len = file.seek(SeekFrom::End(0))?;
    for entry in &index {
        let end_offset = entry.location_offset as u64 + entry.file_size as u64;
        if end_offset > file_len {
            return Err(format!(
                "Recurso {} corrompido: offset {} + tamanho {} excede tamanho do arquivo {}",
                entry.key, entry.location_offset, entry.file_size, file_len
            )
            .into());
        }
    }

    Ok(())
}

fn write_package_chunk(
    part_path: &Path,
    resources: &[PackageResource],
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let file = File::create(part_path)?;
    let mut writer = BufWriter::with_capacity(4 * 1024 * 1024, file);
    DBPFWriter::write_package(&mut writer, resources)?;
    let inner = writer.into_inner()?;
    inner.sync_all()?;
    Ok(())
}

fn chrono_like_timestamp() -> String {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}", now.as_secs())
}

/// Deixa o nome do grupo utilizável como nome de arquivo, sem descaracterizá-lo.
///
/// Só os separadores de caminho e os controles saem; acento, espaço e emoji
/// ficam, porque é assim que o usuário nomeou a pasta de onde os packages vieram.
fn sanitize_stem(raw: &str) -> String {
    let limpo: String = raw
        .chars()
        .map(|c| {
            if c.is_control() || "/\\:*?\"<>|".contains(c) {
                '_'
            } else {
                c
            }
        })
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

    let stem_is_free = |stem: &str| -> bool {
        if let Ok(entries) = fs::read_dir(output_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                // Bloqueia se existir qualquer parte (Part001, Part002, etc.), relatório ou temporário
                if name.starts_with(&format!("{}_Part", stem))
                    || name == format!("{}_report.json", stem)
                    || name.starts_with(&format!(".{}_", stem))
                {
                    return false;
                }
            }
        }
        true
    };

    if stem_is_free(&base) {
        return base;
    }
    for n in 2..1000 {
        let tentativa = format!("{}_{}", base, n);
        if stem_is_free(&tentativa) {
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

    fn hash_file(path: &Path) -> [u8; 32] {
        hash_reader(&mut File::open(path).unwrap()).unwrap()
    }

    #[test]
    fn p0_publication_failure_after_first_part_rolls_back_and_preserves_collision() {
        let temp = tempdir().unwrap();
        let out_path = temp.path().join("out");
        let work_path = temp.path().join("work");
        let output = AnchoredDir::create(&out_path).unwrap();
        let workspace = AnchoredDir::create(&work_path).unwrap();
        let chunks: Vec<_> = (0..3)
            .map(|i| {
                let source = work_path.join(format!("part-{i}.tmp"));
                fs::write(&source, format!("new-{i}")).unwrap();
                (source, out_path.join(format!("part-{i}.package")))
            })
            .collect();
        let previous = out_path.join("previous.package");
        fs::write(&previous, b"previous bytes").unwrap();
        let previous_hash = hash_file(&previous);
        let mut observed_first = false;
        let result = publish_chunks(&chunks, &workspace, &output, |index, destination| {
            if index == 1 {
                assert_eq!(fs::read(&chunks[0].1)?, b"new-0");
                observed_first = true;
                fs::write(destination, b"concurrent file")?;
            }
            Ok(())
        });
        let (error, preserve) = result.unwrap_err();
        assert!(observed_first);
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert!(!preserve);
        assert!(!chunks[0].1.exists());
        assert!(!chunks[2].1.exists());
        assert_eq!(fs::read(&chunks[1].1).unwrap(), b"concurrent file");
        assert_eq!(hash_file(&previous), previous_hash);
    }

    #[test]
    fn p0_publication_reports_rollback_failure_without_deleting_replacement() {
        let temp = tempdir().unwrap();
        let out_path = temp.path().join("out");
        let work_path = temp.path().join("work");
        let output = AnchoredDir::create(&out_path).unwrap();
        let workspace = AnchoredDir::create(&work_path).unwrap();
        let chunks: Vec<_> = (0..2)
            .map(|i| {
                let source = work_path.join(format!("part-{i}.tmp"));
                fs::write(&source, format!("new-{i}")).unwrap();
                (source, out_path.join(format!("part-{i}.package")))
            })
            .collect();
        let saved = out_path.join("saved-first.package");
        let result = publish_chunks(&chunks, &workspace, &output, |index, destination| {
            if index == 1 {
                assert_eq!(fs::read(&chunks[0].1)?, b"new-0");
                fs::rename(&chunks[0].1, &saved)?;
                fs::create_dir(&chunks[0].1)?;
                fs::write(chunks[0].1.join("keep"), b"replacement contents")?;
                fs::write(destination, b"collision")?;
            }
            Ok(())
        });
        let (error, preserve) = result.unwrap_err();
        assert!(preserve);
        assert!(error.to_string().contains("rollback falhou"));
        assert_eq!(fs::read(&saved).unwrap(), b"new-0");
        assert_eq!(
            fs::read(chunks[0].1.join("keep")).unwrap(),
            b"replacement contents"
        );
        assert_eq!(fs::read(&chunks[1].1).unwrap(), b"collision");
    }

    fn post_fixture() -> (tempfile::TempDir, PathBuf, PathBuf, MergeReport) {
        let temp = tempdir().unwrap();
        let source = temp.path().join("original.package");
        let replacement = temp.path().join("replacement.package");
        let resource = PackageResource {
            key: ResourceKey {
                type_id: 1,
                group_id: 0,
                instance_ex: 0,
                instance_low: 1,
            },
            data: b"merged bytes".to_vec(),
            mem_size: 12,
            compressed: 0,
        };
        DBPFWriter::write_package(&mut File::create(&source).unwrap(), &[resource]).unwrap();
        fs::write(&replacement, b"unmerged replacement contents").unwrap();
        let report = merge_sims4_packages(
            &MergeTask {
                input_files: vec![source.clone()],
                output_dir: temp.path().join("output"),
                max_size_bytes: 1_000_000,
                name: Some("safe".into()),
            },
            |_, _, _| {},
        )
        .unwrap();
        (temp, source, replacement, report)
    }

    #[test]
    fn p0_replacement_between_validation_and_action_is_restored_intact() {
        for action in [
            PostMergeAction::Delete,
            PostMergeAction::Disable,
            PostMergeAction::Backup,
        ] {
            let (temp, source, replacement, report) = post_fixture();
            let expected = hash_file(&replacement);
            let outcome = post_merge_transaction(
                action,
                &report,
                &[source.clone()],
                temp.path(),
                &DisabledManager::new(&temp.path().join("config")),
                || fs::rename(&replacement, &source),
            )
            .unwrap();
            assert!(outcome.error.is_some());
            assert_eq!(outcome.affected, 0);
            assert_eq!(hash_file(&source), expected);
        }
    }

    #[test]
    fn p0_existing_writer_blocks_destructive_action() {
        let (temp, source, _, report) = post_fixture();
        let expected = hash_file(&source);
        let _writer = File::options().write(true).open(&source).unwrap();
        let outcome = apply_post_merge_action_validated(
            PostMergeAction::Delete,
            &report,
            &[source.clone()],
            temp.path(),
            &DisabledManager::new(&temp.path().join("config")),
        );
        assert!(outcome.error.is_some());
        assert_eq!(hash_file(&source), expected);
    }

    #[test]
    fn p0_modified_merge_output_cannot_authorize_original_deletion() {
        let (temp, source, _, report) = post_fixture();
        let expected = hash_file(&source);
        fs::write(&report.output_packages[0], b"corrupt output").unwrap();
        let outcome = apply_post_merge_action_validated(
            PostMergeAction::Delete,
            &report,
            &[source.clone()],
            temp.path(),
            &DisabledManager::new(&temp.path().join("config")),
        );
        assert!(outcome.error.is_some());
        assert_eq!(hash_file(&source), expected);
    }

    #[test]
    fn p0_published_package_unlink_or_rename_after_validation_blocks_destructive_action() {
        for is_unlink in [true, false] {
            let (temp, source, _, report) = post_fixture();
            let source_expected = hash_file(&source);
            let published_pkg = PathBuf::from(&report.output_packages[0]);
            let rogue = temp.path().join("rogue.package");
            fs::write(&rogue, b"rogue unverified content").unwrap();

            let outcome = post_merge_transaction(
                PostMergeAction::Delete,
                &report,
                &[source.clone()],
                temp.path(),
                &DisabledManager::new(&temp.path().join("config")),
                || {
                    if is_unlink {
                        fs::remove_file(&published_pkg)?;
                    } else {
                        fs::rename(&rogue, &published_pkg)?;
                    }
                    Ok(())
                },
            )
            .unwrap();

            assert!(
                outcome.error.is_some(),
                "is_unlink={is_unlink}: a pós-ação destrutiva DEVE ser bloqueada se o package publicado foi alterado no diretório"
            );
            assert_eq!(
                outcome.affected, 0,
                "is_unlink={is_unlink}: nenhum original pode ser apagado"
            );
            assert_eq!(
                hash_file(&source),
                source_expected,
                "is_unlink={is_unlink}: o arquivo original DEVE ser preservado intacto"
            );
        }
    }

    #[test]
    fn p0_mutation_during_original_capture_is_rejected() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("source.package");
        fs::write(&path, b"original").unwrap();
        let result = snapshot_original_with(&path, || fs::write(&path, b"mutation"));
        assert!(result.is_err());
        assert_eq!(fs::read(&path).unwrap(), b"mutation");
    }

    #[test]
    fn p0_backup_collision_between_selection_and_publication_preserves_both() {
        let (temp, source, _, report) = post_fixture();
        let expected = hash_file(&source);
        let output = AnchoredDir::open(temp.path()).unwrap();
        let work = tempfile::tempdir_in(temp.path()).unwrap();
        let workspace = AnchoredDir::open(work.path()).unwrap();
        let mut originals = vec![CapturedOriginal {
            evidence: &report.original_snapshots[0],
            parent: output.try_clone().unwrap(),
            lease: ReadLease::acquire(File::open(&source).unwrap()).unwrap(),
            staged_name: String::new(),
            staged: false,
        }];
        let mut collision = None;
        let backup =
            create_zip_from_captured(&mut originals, temp.path(), &output, &workspace, |path| {
                if collision.is_none() {
                    fs::write(path, b"preexisting ZIP bytes")?;
                    collision = Some(temp.path().join(path.file_name().unwrap()));
                }
                Ok(())
            })
            .unwrap();
        let collision = collision.unwrap();
        assert_ne!(backup, collision);
        assert_eq!(fs::read(&collision).unwrap(), b"preexisting ZIP bytes");
        assert_eq!(hash_file(&source), expected);
        let mut zip = zip::ZipArchive::new(File::open(backup).unwrap()).unwrap();
        assert_eq!(
            hash_reader(&mut zip.by_index(0).unwrap()).unwrap(),
            expected
        );
    }

    #[test]
    fn p0_report_publication_failure_rolls_back_published_parts() {
        let fx = post_fixture();
        let (temp, source, _, _) = fx;
        let out_dir = temp.path().join("output_report_fail");
        fs::create_dir_all(&out_dir).unwrap();
        let task = MergeTask {
            input_files: vec![source.clone()],
            output_dir: out_dir.clone(),
            max_size_bytes: 1_000_000,
            name: Some("ReportFail".into()),
        };
        let collision = out_dir.join("ReportFail_Merged_report.json");
        let res = merge_sims4_packages(&task, |cur, total, label| {
            if label.starts_with("Processando") && cur == total {
                fs::write(&collision, b"colisao_concorrente_no_relatorio").unwrap();
            }
        });
        assert!(
            res.is_err(),
            "merge deve falhar se o relatório não puder ser publicado"
        );
        let err_msg = res.unwrap_err().to_string();
        assert!(err_msg.contains("Falha na publicação do relatório"));
        assert_eq!(
            fs::read(&collision).unwrap(),
            b"colisao_concorrente_no_relatorio",
            "arquivo de colisão deve ser preservado"
        );
        let active_parts: Vec<_> = fs::read_dir(&out_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".package"))
            .collect();
        assert!(
            active_parts.is_empty(),
            "nenhuma parte DBPF deve sobrar ativa no diretório se o relatório falhou: {active_parts:?}"
        );
    }

    #[test]
    fn p0_sequential_deletion_failure_does_not_claim_full_rollback() {
        let temp = tempdir().unwrap();
        let mut sources = Vec::new();
        let mut hashes = Vec::new();
        for i in 1..=3 {
            let s = temp.path().join(format!("orig_{i}.package"));
            let resource = PackageResource {
                key: ResourceKey {
                    type_id: 1,
                    group_id: 0,
                    instance_ex: 0,
                    instance_low: i as u32,
                },
                data: vec![i as u8; 30],
                mem_size: 30,
                compressed: 0,
            };
            DBPFWriter::write_package(&mut File::create(&s).unwrap(), &[resource]).unwrap();
            hashes.push(hash_file(&s));
            sources.push(s);
        }
        let report = merge_sims4_packages(
            &MergeTask {
                input_files: sources.clone(),
                output_dir: temp.path().join("output"),
                max_size_bytes: 1_000_000,
                name: Some("SeqFail".into()),
            },
            |_, _, _| {},
        )
        .unwrap();

        let outcome = post_merge_transaction_with(
            PostMergeAction::Delete,
            &report,
            &sources,
            temp.path(),
            &DisabledManager::new(&temp.path().join("config")),
            || Ok(()),
            |idx, _path| {
                if idx == 1 {
                    return Err(io::Error::other("falha_injetada_no_segundo_arquivo"));
                }
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(
            outcome.affected, 1,
            "o primeiro arquivo foi irreversivelmente excluído"
        );
        assert_eq!(
            outcome.failed, 2,
            "os outros dois arquivos falharam em ser concluídos"
        );
        let err = outcome.error.expect("deve registrar o erro");
        assert!(
            err.contains("Falha parcial durante pós-ação"),
            "erro DEVE declarar falha parcial e não rollback integral: {err}"
        );
        assert!(err.contains("1 arquivo(s) foram processados irreversivelmente"));
        assert!(err.contains("2 arquivo(s) restantes foram restaurados da quarentena"));

        assert!(!sources[0].exists(), "arquivo 1 foi excluído");
        assert!(
            sources[1].exists(),
            "arquivo 2 foi restaurado da quarentena"
        );
        assert_eq!(hash_file(&sources[1]), hashes[1]);
        assert!(
            sources[2].exists(),
            "arquivo 3 foi restaurado da quarentena"
        );
        assert_eq!(hash_file(&sources[2]), hashes[2]);
    }
}
