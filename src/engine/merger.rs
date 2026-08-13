use crate::engine::dbpf::{DBPFReader, DBPFWriter, PackageResource, ResourceKey};
use crate::engine::disabled::DisabledManager;
use crate::engine::organizer::{remove_files, RemovalOutcome};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
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
    let mut global_resources: IndexMap<ResourceKey, (PathBuf, u32, u32, u32, u16)> = IndexMap::new();
    let mut failed_files = Vec::new();
    let mut success_count = 0;

    for (idx, fpath) in task.input_files.iter().enumerate() {
        let fname = fpath.file_name().unwrap_or_default().to_string_lossy();
        progress_cb(idx + 1, total_files, &format!("Lendo Metadados: {}/{} ({})", idx + 1, total_files, fname));

        match File::open(fpath) {
            Ok(file) => {
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
    let mut file_cache: std::collections::HashMap<PathBuf, BufReader<File>> = std::collections::HashMap::new();

    for (res_idx, key) in sorted_keys.iter().enumerate() {
        if res_idx % 100 == 0 || res_idx == sorted_keys.len() - 1 {
            progress_cb(res_idx + 1, sorted_keys.len(), &format!("Processando Recursos: {}/{}", res_idx + 1, sorted_keys.len()));
        }

        let (src_path, offset, size, mem_size, compressed) = &global_resources[key];

        if !file_cache.contains_key(src_path) {
            if let Ok(f) = File::open(src_path) {
                file_cache.insert(src_path.clone(), BufReader::with_capacity(4 * 1024 * 1024, f));
            }
        }

        let mut data = vec![0u8; *size as usize];
        if let Some(reader) = file_cache.get_mut(src_path) {
            if reader.seek(SeekFrom::Start(*offset as u64)).is_ok() {
                let _ = reader.read_exact(&mut data);
            }
        }

        let res_len = data.len() as u64;
        if current_chunk_bytes + res_len > safe_limit && !current_chunk_resources.is_empty() {
            let part_name = format!("{}_Part{:03}.package", stem, chunk_index);
            let part_path = task.output_dir.join(&part_name);
            write_package_chunk(&part_path, &current_chunk_resources)?;
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
        write_package_chunk(&part_path, &current_chunk_resources)?;
        output_packages.push(part_path.display().to_string());
    }

    let report = MergeReport {
        success: failed_files.is_empty(),
        partial: !failed_files.is_empty() && success_count > 0,
        total_files_found: total_files,
        success_count,
        failed_files,
        output_packages,
    };

    // O relatório acompanha as partes: um `merge_report.json` fixo era
    // sobrescrito pela tarefa seguinte, apagando o registro da anterior.
    let report_path = task.output_dir.join(format!("{}_report.json", stem));
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
            Ok(report) if report.success => {
                let post = common_ancestor(&job.input_files).map(|root| {
                    apply_post_merge_action(job.post_action, &job.input_files, &root, disabled_mgr)
                });
                let status = match &post {
                    Some(p) if p.failed > 0 => JobStatus::Partial,
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
                status: if report.partial { JobStatus::Partial } else { JobStatus::Failed },
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
pub fn apply_post_merge_action(
    action: PostMergeAction,
    originals: &[PathBuf],
    input_root: &Path,
    disabled_mgr: &DisabledManager,
) -> PostMergeOutcome {
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
                let _ = fs::remove_file(&zip_path);
                return PostMergeOutcome {
                    error: Some(e.to_string()),
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
            let RemovalOutcome { removed, freed_bytes, failures } =
                remove_files(originals, &allowed_roots);
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
fn create_zip_from_files(files: &[PathBuf], base_dir: &Path, zip_path: &Path) -> io::Result<()> {
    let file = File::create(zip_path)?;
    let mut zip = zip::ZipWriter::new(file);
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    for path in files {
        if !path.is_file() {
            continue;
        }
        let name = path.strip_prefix(base_dir).unwrap_or(path);
        zip.start_file(name.to_string_lossy().to_string(), options)?;
        let mut f = File::open(path)?;
        let mut buffer = Vec::new();
        f.read_to_end(&mut buffer)?;
        zip.write_all(&buffer)?;
    }

    zip.finish()?;
    Ok(())
}

fn write_package_chunk(part_path: &Path, resources: &[PackageResource]) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let file = File::create(part_path)?;
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
}
