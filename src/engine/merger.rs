use crate::core::safety::{same_file_identity, AnchoredDir, ReadLease};
use crate::engine::dbpf::{DBPFReader, DBPFWriter, IndexEntry, PackageResource, ResourceKey};
use crate::engine::manifest::{MergedFolder, MergedPackage, MergedPackageManifest, S4S_MANIFEST_KEY};
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

/// Teto observado no PackageMergeUtility.Merge do Sims 4 Studio 3.2.6.5.
/// Mantido como política de compatibilidade; não é prova do limite interno do jogo.
pub const MAX_SAFE_PACKAGE_SIZE: u64 = 2_147_483_647;

pub const TYPE_CAS_THUMBNAIL: u32 = 0x220557DA;
pub const TYPE_OBJECT_THUMBNAIL: u32 = 0x0D6E66E5;
pub const TYPE_OBJECT_LARGE_THUMBNAIL: u32 = 0x5B522BB0;
pub const TYPE_SIM_THUMBNAIL: u32 = 0xCD87CF8A;

/// Recursos puramente cosméticos de catálogo/ícone compartilhados por arquétipos clonados no TS4.
pub fn is_thumbnail_resource(type_id: u32) -> bool {
    matches!(
        type_id,
        TYPE_CAS_THUMBNAIL | TYPE_OBJECT_THUMBNAIL | TYPE_OBJECT_LARGE_THUMBNAIL | TYPE_SIM_THUMBNAIL
    )
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergeReport {
    pub success: bool,
    pub partial: bool,
    pub total_files_found: usize,
    pub success_count: usize,
    pub failed_files: Vec<(String, String)>,
    pub output_packages: Vec<String>,
    /// TGIs descartados como duplicatas somente após igualdade de payload e metadados.
    #[serde(default)]
    pub identical_resources: Vec<IdenticalResource>,
    #[serde(skip, default)]
    pub original_snapshots: Vec<OriginalSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdenticalResource {
    pub key: String,
    pub retained_from: String,
    pub duplicate_from: String,
}

#[derive(Debug, Clone)]
struct ResourceLocation {
    path: PathBuf,
    offset: u32,
    file_size: u32,
    mem_size: u32,
    compressed: u16,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnmergeTask {
    /// Arquivos .package mesclados que devem ser desunificados.
    pub input_packages: Vec<PathBuf>,
    /// Diretório onde os packages originais extraídos serão gravados.
    pub output_dir: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnmergeReport {
    pub success: bool,
    pub partial: bool,
    pub total_merged_packages: usize,
    pub total_extracted_packages: usize,
    pub extracted_files: Vec<String>,
    pub failed_packages: Vec<(String, String)>,
}
/// Filename markers complement binary manifest detection. Exact tokens avoid
/// matching unrelated words such as "emergency" or "merger".
pub(crate) fn premerged_marker(path: &Path) -> Option<&'static str> {
    fn classify(token: &str, all_in_one_state: &mut u8) -> Option<&'static str> {
        if token.eq_ignore_ascii_case("MERGE") {
            return Some("MERGE");
        }
        if token.eq_ignore_ascii_case("MERGED") {
            return Some("MERGED");
        }
        if token.eq_ignore_ascii_case("COMBINED") {
            return Some("COMBINED");
        }
        if token.eq_ignore_ascii_case("ALLINONE") {
            return Some("ALL-IN-ONE");
        }

        *all_in_one_state = if token.eq_ignore_ascii_case("ALL") {
            1
        } else if token.eq_ignore_ascii_case("IN") && *all_in_one_state == 1 {
            2
        } else if token.eq_ignore_ascii_case("ONE") && *all_in_one_state == 2 {
            return Some("ALL-IN-ONE");
        } else {
            0
        };
        None
    }

    let name = path.file_name()?.to_string_lossy();
    let stem = Path::new(name.as_ref()).file_stem()?.to_string_lossy();
    if stem.ends_with("MERGED") {
        return Some("MERGED");
    }

    let mut token_start = None;
    let mut previous = None;
    let mut all_in_one_state = 0;
    for (offset, current) in name.char_indices() {
        let boundary = !current.is_ascii_alphanumeric()
            || previous.is_some_and(|prior: char| {
                (prior.is_ascii_lowercase() && current.is_ascii_uppercase())
                    || (prior.is_ascii_alphabetic() != current.is_ascii_alphabetic())
            });
        if boundary {
            if let Some(start) = token_start.take() {
                if let Some(marker) = classify(&name[start..offset], &mut all_in_one_state) {
                    return Some(marker);
                }
            }
            if current.is_ascii_alphanumeric() {
                token_start = Some(offset);
            }
        } else if token_start.is_none() {
            token_start = Some(offset);
        }
        previous = current.is_ascii_alphanumeric().then_some(current);
    }
    if let Some(start) = token_start {
        if let Some(marker) = classify(&name[start..], &mut all_in_one_state) {
            return Some(marker);
        }
    }
    None
}

pub fn merge_sims4_packages<F>(
    task: &MergeTask,
    progress_cb: F,
) -> Result<MergeReport, Box<dyn std::error::Error + Send + Sync>>
where
    F: Fn(usize, usize, &str) + Send + Sync,
{
    merge_sims4_packages_with(task, progress_cb, |_, _| Ok(()))
}

fn merge_sims4_packages_with<F, H>(
    task: &MergeTask,
    progress_cb: F,
    after_parts_hook: H,
) -> Result<MergeReport, Box<dyn std::error::Error + Send + Sync>>
where
    F: Fn(usize, usize, &str) + Send + Sync,
    H: FnOnce(&Path, &AnchoredDir) -> Result<(), Box<dyn std::error::Error + Send + Sync>>,
{
    let all_premerged = !task.input_files.is_empty()
        && task.input_files.iter().all(|path| {
            MergedPackageManifest::is_s4s_merged_package(path)
                || premerged_marker(path).is_some()
        });
    if all_premerged {
        if let Some(path) = task
            .input_files
            .iter()
            .find(|path| MergedPackageManifest::is_s4s_merged_package(path))
        {
            return Err(format!(
                "Entrada já contém manifesto de merge do Sims 4 Studio / S4Suite: {}. Desunifique o package antes de mesclar.",
                path.display()
            )
            .into());
        }
        if let Some((path, marker)) = task
            .input_files
            .iter()
            .find_map(|path| premerged_marker(path).map(|marker| (path, marker)))
        {
            return Err(format!(
                "Entrada suspeita de merge preexistente ({marker}): {}",
                path.display()
            )
            .into());
        }
    }
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
            identical_resources: Vec::new(),
            original_snapshots: Vec::new(),
        });
    }

    let mut original_snapshots = Vec::new();
    let mut file_cache: HashMap<PathBuf, BufReader<File>> = HashMap::new();
    let mut seen_inputs = HashSet::new();
    // O prefixo é resolvido antes de escrever qualquer parte para evitar colisões
    // entre tarefas iniciadas no mesmo segundo.
    let stem = unique_output_stem(&output_dir.path(), task.name.as_deref());
    let mut global_resources: IndexMap<ResourceKey, ResourceLocation> = IndexMap::new();
    let mut identical_resources = Vec::new();
    let mut failed_files = Vec::new();
    let mut success_count: usize = 0;
    let input_root = common_ancestor(&task.input_files).unwrap_or_else(|| PathBuf::from("."));
    let mut original_packages: Vec<(PathBuf, Vec<ResourceKey>)> = Vec::new();

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
                        if index.iter().any(|entry| entry.key == S4S_MANIFEST_KEY) {
                            failed_files.push((
                                fpath.display().to_string(),
                                "Entrada já contém manifesto de merge do Sims 4 Studio / S4Suite (desunifique antes de mesclar)".into(),
                            ));
                            continue;
                        }
                        let unique: HashSet<_> = index.iter().map(|entry| entry.key).collect();
                        if unique.len() != index.len() {
                            failed_files.push((
                                fpath.display().to_string(),
                                "TGI repetido internamente no mesmo package".into(),
                            ));
                            continue;
                        }
                        let relative = fpath.strip_prefix(&input_root).unwrap_or(fpath);
                        validate_relative_package_path(relative)?;
                        let keys = index.iter().map(|entry| entry.key).collect();
                        original_packages.push((relative.to_path_buf(), keys));
                        file_cache.insert(fpath.clone(), reader);
                        for entry in index {
                            let current = ResourceLocation {
                                path: fpath.clone(),
                                offset: entry.location_offset,
                                file_size: entry.file_size,
                                mem_size: entry.mem_size,
                                compressed: entry.compressed,
                            };
                            if let Some(previous) = global_resources.get(&entry.key) {
                                let is_thumb = is_thumbnail_resource(entry.key.type_id);
                                if !is_thumb
                                    && (previous.file_size != current.file_size
                                        || previous.mem_size != current.mem_size
                                        || previous.compressed != current.compressed
                                        || !resource_payloads_equal(
                                            &mut file_cache,
                                            previous,
                                            &current,
                                        )?)
                                {
                                    return Err(format!(
                                        "Colisão de TGI com conteúdo ou metadados diferentes: {} em {} e {}",
                                        entry.key,
                                        previous.path.display(),
                                        current.path.display()
                                    )
                                    .into());
                                }
                                identical_resources.push(IdenticalResource {
                                    key: entry.key.to_string(),
                                    retained_from: previous.path.display().to_string(),
                                    duplicate_from: current.path.display().to_string(),
                                });
                            } else {
                                global_resources.insert(entry.key, current);
                            }
                        }
                        success_count += 1;
                        original_snapshots.push(snapshot);
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
            identical_resources,
            original_snapshots,
        });
    }
    // Every part carries the complete ownership manifest. Our unmerge resolves
    // it against all selected parts, detecting an incomplete set rather than
    // silently writing a partial original.
    let manifest = manifest_for_originals(&original_packages)?;
    let manifest_resource = manifest.to_compressed_resource()?;
    let max_size = task.max_size_bytes.min(MAX_SAFE_PACKAGE_SIZE);
    let overhead = 96 + 4 + 32 + manifest_resource.data.len() as u64;
    if max_size <= overhead {
        return Err(format!("Limite de tamanho insuficiente para cabeçalho e manifesto ({overhead} bytes)").into());
    }
    let target_size = max_size - max_size / 10;
    let mut keys: Vec<_> = global_resources.keys().copied().collect();
    keys.sort();
    let mut chunks: Vec<Vec<ResourceKey>> = Vec::new();
    let mut chunk = Vec::new();
    let mut size = overhead;
    for key in keys {
        let added = global_resources[&key].file_size as u64 + 32;
        if overhead + added > max_size {
            return Err(format!("Recurso {key} não cabe no limite configurado ({max_size} bytes), incluindo manifesto e índice").into());
        }
        if !chunk.is_empty() && size + added > target_size {
            chunks.push(std::mem::take(&mut chunk));
            size = overhead;
        }
        chunk.push(key);
        size += added;
    }
    if !chunk.is_empty() {
        chunks.push(chunk);
    }
    let mut output_packages = Vec::new();
    let mut pending_chunks = Vec::new();
    for (i, chunk) in chunks.iter().enumerate() {
        progress_cb(i + 1, chunks.len(), &format!("Processando Recursos: parte {}/{}", i + 1, chunks.len()));
        let part_name = format!("{stem}_Part{:03}.package", i + 1);
        let temp_path = workspace.path().join(format!(".{part_name}.s4tmp"));
        let sources: Vec<_> = chunk.iter().map(|key| (*key, global_resources[key].clone())).collect();
        write_streamed_chunk(&temp_path, &sources, Some(&manifest_resource), &mut file_cache)?;
        if fs::metadata(&temp_path)?.len() > max_size {
            return Err("Parte gerada excede limite configurado".into());
        }
        pending_chunks.push((temp_path, output_dir.path().join(part_name)));
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
        identical_resources,
        original_snapshots,
    };
    let report_step = (|| -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        after_parts_hook(&task.output_dir, &output_dir)?;
        let report_name = format!("{}_report.json", stem);
        let tmp_report = workspace.path().join(&report_name);
        let json = serde_json::to_vec_pretty(&report)?;
        fs::write(&tmp_report, json)?;
        workspace_dir.publish_noreplace(
            OsStr::new(&report_name),
            &output_dir,
            OsStr::new(&report_name),
        )?;
        output_dir.sync_all()?;
        Ok(())
    })();

    if let Err(error) = report_step {
        let published_refs: Vec<(&Path, fs::Metadata, [u8; 32])> = outputs
            .iter()
            .map(|(path, hash, meta)| (path.as_path(), meta.clone(), *hash))
            .collect();
        let rollback_res = rollback_published_parts(&published_refs, &workspace_dir, &output_dir);
        match rollback_res {
            Ok(()) => {
                let close_res = workspace.close();
                match close_res {
                    Ok(()) => {
                        return Err(format!(
                            "Falha na publicação do relatório JSON ({error}); partes publicadas sofreram rollback com sucesso"
                        )
                        .into());
                    }
                    Err(cleanup_err) => {
                        return Err(format!(
                            "Falha na publicação do relatório JSON ({error}); partes sofreram rollback mas limpeza do workspace falhou: {cleanup_err}"
                        )
                        .into());
                    }
                }
            }
            Err(failures) => {
                let recovery = workspace.keep();
                return Err(format!(
                    "Falha na publicação do relatório JSON ({error}); rollback das partes falhou: {}; arquivos preservados em {}",
                    failures.join("; "),
                    recovery.display()
                )
                .into());
            }
        }
    }

    workspace.close()?;
    Ok(report)
}

/// Inspeciona um package mesclado, retornando o manifesto do Sims 4 Studio / S4Suite se presente.
pub fn inspect_merged_package(
    path: &Path,
) -> Result<Option<MergedPackageManifest>, Box<dyn std::error::Error + Send + Sync>> {
    MergedPackageManifest::read_from_package_file(path)
}

/// Desunifica um único package mesclado para a pasta de destino.
pub fn unmerge_single_package<F>(
    merged_package_path: &Path,
    output_dir: &Path,
    progress_cb: F,
) -> Result<UnmergeReport, Box<dyn std::error::Error + Send + Sync>>
where
    F: Fn(usize, usize, &str) + Send + Sync,
{
    unmerge_sims4_packages(
        &UnmergeTask {
            input_packages: vec![merged_package_path.to_path_buf()],
            output_dir: output_dir.to_path_buf(),
        },
        progress_cb,
    )
}

/// Reconstructs resource ownership from manifests, not original DBPF layout.
/// A missing TGI aborts the batch: partial packages must never look successful.
pub fn unmerge_sims4_packages<F>(
    task: &UnmergeTask,
    progress_cb: F,
) -> Result<UnmergeReport, Box<dyn std::error::Error + Send + Sync>>
where
    F: Fn(usize, usize, &str) + Send + Sync,
{
    if task.input_packages.is_empty() {
        return Err("Selecione os packages mesclados e todas as partes do lote".into());
    }
    let mut readers = HashMap::new();
    let mut resources: IndexMap<ResourceKey, ResourceLocation> = IndexMap::new();
    let mut packages: IndexMap<PathBuf, Vec<ResourceKey>> = IndexMap::new();
    for (i, path) in task.input_packages.iter().enumerate() {
        progress_cb(i + 1, task.input_packages.len(), "Lendo manifestos e índices");
        // Immutable snapshots avoid reading a manifest from one version of the
        // file and payloads from another version.
        let (_, file) = snapshot_original(path)?;
        let mut reader = BufReader::new(file);
        let manifest = MergedPackageManifest::read_from_reader(&mut reader)?
            .ok_or("Package sem manifesto S4S; não é possível recuperar seus originais")?;
        let (_, index) = DBPFReader::read_index(&mut reader)?;
        if readers.insert(path.clone(), reader).is_some() {
            return Err("Package de entrada repetido".into());
        }
        for entry in index {
            if entry.key == S4S_MANIFEST_KEY {
                continue;
            }
            let current = ResourceLocation {
                path: path.clone(), offset: entry.location_offset, file_size: entry.file_size,
                mem_size: entry.mem_size, compressed: entry.compressed,
            };
            if let Some(previous) = resources.get(&entry.key) {
                let is_thumb = is_thumbnail_resource(entry.key.type_id);
                if !is_thumb
                    && (previous.file_size != current.file_size
                        || previous.mem_size != current.mem_size
                        || previous.compressed != current.compressed
                        || !resource_payloads_equal(&mut readers, previous, &current)?)
                {
                    return Err(format!("Colisão de TGI divergente no unmerge: {}", entry.key).into());
                }
            } else {
                resources.insert(entry.key, current);
            }
        }
        let mut names = HashSet::new();
        for (folder, package) in manifest.flatten_packages() {
            validate_manifest_component(&package.name)?;
            let relative = PathBuf::from(folder).join(format!("{}.package", package.name));
            validate_relative_package_path(&relative)?;
            if !names.insert(relative.clone()) {
                return Err(format!("Nome duplicado no manifesto: {}", relative.display()).into());
            }
            if package.resources.iter().any(|key| *key == S4S_MANIFEST_KEY) {
                return Err("Manifesto não pode pertencer ao conteúdo de um original".into());
            }
            if let Some(previous) = packages.get(&relative) {
                // Repeated complete manifests are expected for a multipart
                // merge; distinct package identities must not silently combine.
                if previous != &package.resources {
                    return Err(format!("Manifestos incompatíveis para {}", relative.display()).into());
                }
            } else {
                packages.insert(relative, package.resources.clone());
            }
        }
    }
    if packages.is_empty() {
        return Err("Manifesto não declara nenhum package".into());
    }
    for (path, keys) in &packages {
        let mut seen = HashSet::new();
        for key in keys {
            if !seen.insert(key) {
                return Err(format!("TGI repetido no manifesto: {key}").into());
            }
            if !resources.contains_key(key) {
                return Err(format!("Recurso {key} ausente de {}; selecione todas as partes. Nenhum arquivo publicado.", path.display()).into());
            }
        }
    }
    let output = AnchoredDir::create(&task.output_dir)?;
    let workspace = tempfile::Builder::new().prefix(".s4merge-unmerge-").tempdir_in(output.path())?;
    let staging = output.child(workspace.path().file_name().ok_or("workspace sem nome")?)?;
    let batch = staging.create_child("result")?;
    for (i, (relative, keys)) in packages.iter().enumerate() {
        progress_cb(i + 1, packages.len(), &format!("Extraindo {}", relative.display()));
        let mut parents: Vec<AnchoredDir> = Vec::new();
        // Each component has been validated; anchored creation refuses links.
        if let Some(folder) = relative.parent() {
            for component in folder.components() {
                let parent = parents.last().unwrap_or(&batch);
                let child = parent.create_child(component.as_os_str())?;
                parents.push(child);
            }
        }
        let parent = parents.last().unwrap_or(&batch);
        let path = parent.path().join(relative.file_name().ok_or("package sem nome")?);
        let mut sources: Vec<_> = keys.iter().map(|key| (*key, resources[key].clone())).collect();
        sources.sort_by_key(|(key, _)| *key);
        write_streamed_chunk(&path, &sources, None, &mut readers)?;
        parent.sync_all()?;
    }
    batch.sync_all()?;
    let base = format!("Unmerged_{}", chrono_like_timestamp());
    let mut suffix = 0usize;
    let published_name = loop {
        let name = if suffix == 0 { base.clone() } else { format!("{base}_{suffix}") };
        match staging.publish_noreplace("result", &output, &name) {
            Ok(()) => break name,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => suffix += 1,
            Err(e) => return Err(e.into()),
        }
    };
    output.sync_all()?;
    workspace.close()?;
    let extracted_files = packages.keys().map(|path| {
        task.output_dir.join(&published_name).join(path).display().to_string()
    }).collect::<Vec<_>>();
    Ok(UnmergeReport {
        success: true, partial: false, total_merged_packages: task.input_packages.len(),
        total_extracted_packages: extracted_files.len(), extracted_files, failed_packages: Vec::new(),
    })
}

fn validate_manifest_component(name: &str) -> io::Result<()> {
    if name.is_empty() || name == "." || name == ".."
        || name.chars().any(|c| c.is_control() || "/\\:".contains(c))
    {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "Componente de caminho inseguro no manifesto"));
    }
    Ok(())
}

fn validate_relative_package_path(path: &Path) -> io::Result<()> {
    if path.as_os_str().is_empty() {
        return Err(io::Error::other("Caminho vazio no manifesto"));
    }
    let text = path.to_str().ok_or_else(|| io::Error::other("Nome de package não é UTF-8"))?;
    // Check raw separators as well: Path::components normalizes away ".".
    for name in text.split('/') {
        validate_manifest_component(name)?;
    }
    if path.components().any(|c| !matches!(c, std::path::Component::Normal(_))) {
        return Err(io::Error::other("Caminho absoluto ou traversal no manifesto"));
    }
    Ok(())
}

fn manifest_for_originals(
    originals: &[(PathBuf, Vec<ResourceKey>)],
) -> io::Result<MergedPackageManifest> {
    let mut manifest = MergedPackageManifest::new();
    for (path, resources) in originals {
        validate_relative_package_path(path)?;
        let mut folder = &mut manifest.root;
        if let Some(parent) = path.parent() {
            for name in parent.components() {
                let name = name.as_os_str().to_str().ok_or_else(|| io::Error::other("Nome não UTF-8"))?;
                let index = match folder.folders.iter().position(|f| f.name == name) {
                    Some(i) => i,
                    None => {
                        folder.folders.push(MergedFolder { name: name.into(), ..Default::default() });
                        folder.folders.len() - 1
                    }
                };
                folder = &mut folder.folders[index];
            }
        }
        let name = path.file_stem().and_then(OsStr::to_str).ok_or_else(|| io::Error::other("Nome inválido"))?;
        validate_manifest_component(name)?;
        if folder.packages.iter().any(|p| p.name == name) {
            return Err(io::Error::other("Packages com mesmo caminho relativo"));
        }
        folder.packages.push(MergedPackage { name: name.into(), resources: resources.clone() });
    }
    Ok(manifest)
}

fn write_streamed_chunk(
    path: &Path,
    sources: &[(ResourceKey, ResourceLocation)],
    manifest: Option<&PackageResource>,
    readers: &mut HashMap<PathBuf, BufReader<File>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut entries = Vec::with_capacity(sources.len() + usize::from(manifest.is_some()));
    for (key, source) in sources {
        entries.push(IndexEntry {
            key: *key, location_offset: 0, file_size: source.file_size,
            mem_size: source.mem_size, compressed: source.compressed,
        });
    }
    if let Some(manifest) = manifest {
        entries.push(IndexEntry {
            key: manifest.key, location_offset: 0, file_size: manifest.data.len() as u32,
            mem_size: manifest.mem_size, compressed: manifest.compressed,
        });
    }
    let size = 100 + entries.len() as u64 * 32
        + entries.iter().map(|entry| entry.file_size as u64).sum::<u64>();
    if size > MAX_SAFE_PACKAGE_SIZE {
        return Err("Package reconstruído excede teto de compatibilidade de 2 GiB".into());
    }
    let mut writer = BufWriter::new(fs::OpenOptions::new().write(true).create_new(true).open(path)?);
    DBPFWriter::write_package_stream(&mut writer, &entries, |i, writer| {
        if i == sources.len() {
            return writer.write_all(&manifest.ok_or_else(|| io::Error::other("Manifesto ausente"))?.data);
        }
        let source = &sources[i].1;
        let reader = readers.get_mut(&source.path).ok_or_else(|| io::Error::other("Snapshot ausente"))?;
        reader.seek(SeekFrom::Start(source.offset as u64))?;
        let copied = io::copy(&mut reader.take(source.file_size as u64), writer)?;
        if copied != source.file_size as u64 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "Snapshot truncado"));
        }
        Ok(())
    })?;
    writer.into_inner()?.sync_all()?;
    let mut output = File::open(path)?;
    let (_, actual) = DBPFReader::read_index(&mut output)?;
    if actual.len() != entries.len() || output.metadata()?.len() > MAX_SAFE_PACKAGE_SIZE {
        return Err("Parte inválida após escrita".into());
    }
    let expected: HashMap<_, _> = entries.iter().map(|entry| (entry.key, entry)).collect();
    for entry in actual {
        let expected = expected.get(&entry.key).ok_or("TGI inesperado após escrita")?;
        if entry.file_size != expected.file_size || entry.mem_size != expected.mem_size || entry.compressed != expected.compressed {
            return Err("Metadados divergentes após escrita".into());
        }
    }
    Ok(())
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
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 128 * 1024];
    loop {
        let count = source.read(&mut buffer)?;
        if count == 0 {
            break;
        }
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
    source.seek(SeekFrom::Start(0))?;
    let reader_file = source.try_clone()?;
    Ok((
        OriginalSnapshot {
            path: path.to_path_buf(),
            metadata: before,
            sha256,
            outputs: Arc::default(),
        },
        reader_file,
    ))
}

fn rollback_published_parts(
    published: &[(&Path, fs::Metadata, [u8; 32])],
    workspace: &AnchoredDir,
    output: &AnchoredDir,
) -> Result<(), Vec<String>> {
    let mut failures = Vec::new();
    for (rollback_index, (path, metadata, hash)) in published.iter().enumerate().rev() {
        let name = path.file_name().expect("validated name");
        let recovery_name = format!("rollback-{rollback_index}");
        let recovery_name = OsStr::new(&recovery_name);
        let rollback = (|| -> io::Result<()> {
            // Confere se a entrada acessível pelo caminho ainda é comprovadamente a nossa parte publicada
            let is_intact_output = (|| -> io::Result<bool> {
                let mut current = match output.open_regular(name) {
                    Ok(f) => f,
                    Err(_) => return Ok(false),
                };
                let current_meta = current.metadata()?;
                if !same_file_identity(metadata, &current_meta) {
                    return Ok(false);
                }
                if hash_reader(&mut current)? != *hash {
                    return Ok(false);
                }
                Ok(true)
            })()?;

            if !is_intact_output {
                return Err(io::Error::other(
                    "Arquivo publicado foi substituído ou removido; rollback recusado para proteger dados externos",
                ));
            }

            output.publish_noreplace(name, workspace, recovery_name)?;
            let verified = (|| -> io::Result<bool> {
                let mut recovered = workspace.open_regular(recovery_name)?;
                Ok(same_file_identity(metadata, &recovered.metadata()?)
                    && hash_reader(&mut recovered)? == *hash)
            })();
            if !matches!(verified, Ok(true)) {
                let _ = workspace.publish_noreplace(recovery_name, output, name);
                return Err(io::Error::other(
                    "Conteúdo divergente detectado após mover para workspace; descartabilidade recusada",
                ));
            }
            workspace.remove_file(recovery_name)
        })();
        if let Err(rollback_error) = rollback {
            failures.push(format!("{}: {rollback_error}", path.display()));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures)
    }
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
            match rollback_published_parts(&published, workspace, output) {
                Ok(()) => return Err((error, false)),
                Err(failures) => {
                    return Err((
                        io::Error::other(format!(
                            "{error}; rollback falhou: {}",
                            failures.join("; ")
                        )),
                        true,
                    ));
                }
            }
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
                if job.post_action != PostMergeAction::Keep {
                    on_progress(index, 1, 1, "Aplicando ação pós-merge nos arquivos originais...");
                }
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
                on_progress(index, 1, 1, "Concluído!");
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


fn resource_payloads_equal(
    readers: &mut HashMap<PathBuf, BufReader<File>>,
    left: &ResourceLocation,
    right: &ResourceLocation,
) -> io::Result<bool> {
    let mut left_buffer = [0u8; 64 * 1024];
    let mut right_buffer = [0u8; 64 * 1024];
    let mut remaining = left.file_size as usize;

    let mut offset = 0u64;
    while remaining > 0 {
        let count = remaining.min(left_buffer.len());
        let reader = readers.get_mut(&left.path)
            .ok_or_else(|| io::Error::other("reader de recurso ausente"))?;
        reader.seek(SeekFrom::Start(left.offset as u64 + offset))?;
        reader.read_exact(&mut left_buffer[..count])?;
        let reader = readers.get_mut(&right.path)
            .ok_or_else(|| io::Error::other("reader de recurso ausente"))?;
        reader.seek(SeekFrom::Start(right.offset as u64 + offset))?;
        reader.read_exact(&mut right_buffer[..count])?;
        offset += count as u64;
        if left_buffer[..count] != right_buffer[..count] {
            return Ok(false);
        }
        remaining -= count;
    }
    Ok(true)
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

    fn shared_key() -> ResourceKey {
        ResourceKey {
            type_id: 0x11111111,
            group_id: 0,
            instance_ex: 0,
            instance_low: 1,
        }
    }

    fn write_one_resource(path: &Path, data: &[u8], mem_size: u32, compressed: u16) {
        let mut file = File::create(path).unwrap();
        DBPFWriter::write_package(
            &mut file,
            &[PackageResource {
                key: shared_key(),
                data: data.to_vec(),
                mem_size,
                compressed,
            }],
        )
        .unwrap();
    }

    #[test]
    fn tgi_collision_with_different_payload_is_rejected_without_output() {
        let dir = tempdir().unwrap();
        let first = dir.path().join("first.package");
        let second = dir.path().join("second.package");
        let output = dir.path().join("output");
        write_one_resource(&first, b"payload A", 9, 0);
        write_one_resource(&second, b"payload B", 9, 0);
        let task = MergeTask {
            input_files: vec![first, second],
            output_dir: output.clone(),
            max_size_bytes: 1_000_000,
            name: Some("collision".into()),
        };

        let error = merge_sims4_packages(&task, |_, _, _| {}).unwrap_err();
        assert!(error.to_string().contains("Colisão de TGI"));
        assert!(fs::read_dir(output).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".package")));
    }

    #[test]
    fn tgi_collision_with_different_metadata_is_rejected() {
        let dir = tempdir().unwrap();
        let first = dir.path().join("first.package");
        let second = dir.path().join("second.package");
        write_one_resource(&first, b"same", 4, 0);
        write_one_resource(&second, b"same", 3, 0);
        let task = MergeTask {
            input_files: vec![first, second],
            output_dir: dir.path().join("output"),
            max_size_bytes: 1_000_000,
            name: Some("metadata-collision".into()),
        };

        let error = merge_sims4_packages(&task, |_, _, _| {}).unwrap_err();
        assert!(error.to_string().contains("Colisão de TGI"));
    }

    #[test]
    fn merge_rejects_filename_marker_before_creating_output() {
        let dir = tempdir().unwrap();
        let input = dir.path().join("existing_merged.package");
        write_one_resource(&input, b"payload", 7, 0);
        let output = dir.path().join("output");
        let task = MergeTask {
            input_files: vec![input],
            output_dir: output.clone(),
            max_size_bytes: 1_000_000,
            name: None,
        };

        assert!(merge_sims4_packages(&task, |_, _, _| {}).is_err());
        assert!(!output.exists());
    }

    #[test]
    fn identical_tgi_payload_and_metadata_are_deduplicated_and_reported() {
        let dir = tempdir().unwrap();
        let first = dir.path().join("first.package");
        let second = dir.path().join("second.package");
        write_one_resource(&first, b"same payload", 12, 0);
        write_one_resource(&second, b"same payload", 12, 0);
        let task = MergeTask {
            input_files: vec![first, second],
            output_dir: dir.path().join("output"),
            max_size_bytes: 1_000_000,
            name: Some("identical".into()),
        };

        let report = merge_sims4_packages(&task, |_, _, _| {}).unwrap();
        assert!(report.success);
        assert_eq!(report.identical_resources.len(), 1);
        let output = File::open(&report.output_packages[0]).unwrap();
        let (_header, index) = DBPFReader::read_index(&mut BufReader::new(output)).unwrap();
        let content: Vec<_> = index.iter().filter(|e| e.key != S4S_MANIFEST_KEY).collect();
        assert_eq!(content.len(), 1);
        assert_eq!(content[0].key, shared_key());
        assert_eq!(content[0].mem_size, 12);
    }


    #[test]
    fn premerged_markers_match_tokens_and_explicit_merged_suffix_only() {
        for name in [
            "hair MERGED.package",
            "HairMerge.package",
            "set-ALL-IN-ONE.package",
            "CollectionCombined.package",
            "BodyPresetsMERGED.package",
        ] {
            assert!(premerged_marker(Path::new(name)).is_some(), "{name}");
        }
        for name in ["emergency.package", "merger.package", "mergeable.package"] {
            assert!(premerged_marker(Path::new(name)).is_none(), "{name}");
        }
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
        assert!(err_msg.contains("relatório JSON"));
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
    fn p0_report_failure_with_replaced_published_part_preserves_external_file_and_directory() {
        for replace_with_dir in [false, true] {
            let fx = post_fixture();
            let (temp, source, _, _) = fx;
            let out_dir = temp
                .path()
                .join(format!("output_rep_replace_{replace_with_dir}"));
            fs::create_dir_all(&out_dir).unwrap();
            let task = MergeTask {
                input_files: vec![source.clone()],
                output_dir: out_dir.clone(),
                max_size_bytes: 1_000_000,
                name: Some("RepReplace".into()),
            };
            let external_file_bytes = b"CONTEUDO_EXTERNO_QUE_NUNCA_PODE_SER_DESTRUIDO";
            let external_dir_file_bytes = b"DADOS_DENTRO_DO_DIRETORIO_SUBSTITUTO";

            let res = merge_sims4_packages_with(
                &task,
                |_, _, _| {},
                |output_path, _| {
                    let part_name = "RepReplace_Merged_Part001.package";
                    let part_path = output_path.join(part_name);
                    assert!(
                        part_path.exists(),
                        "parte 1 deve ter sido publicada antes do gancho de relatório"
                    );
                    fs::remove_file(&part_path)?;

                    if replace_with_dir {
                        fs::create_dir(&part_path)?;
                        fs::write(part_path.join("interno.txt"), external_dir_file_bytes)?;
                    } else {
                        fs::write(&part_path, external_file_bytes)?;
                    }

                    // Injeta a falha simulada na gravação/criação do relatório JSON
                    Err(io::Error::other("falha_injetada_na_criacao_do_relatorio").into())
                },
            );

            assert!(
                res.is_err(),
                "merge deve falhar devido à falha de relatório"
            );
            let err_msg = res.unwrap_err().to_string();
            assert!(
                err_msg.contains("rollback das partes falhou")
                    || err_msg.contains("arquivos preservados em"),
                "erro DEVE reportar falha de rollback e caminho preservado: {err_msg}"
            );

            let part_path = out_dir.join("RepReplace_Merged_Part001.package");
            assert!(
                part_path.exists(),
                "o arquivo ou diretório substituto DEVE ser preservado intacto"
            );

            if replace_with_dir {
                assert!(
                    part_path.is_dir(),
                    "o diretório substituto deve permanecer como diretório"
                );
                let inner = part_path.join("interno.txt");
                assert_eq!(fs::read(&inner).unwrap(), external_dir_file_bytes);
            } else {
                assert!(
                    part_path.is_file(),
                    "o arquivo substituto deve permanecer como arquivo"
                );
                assert_eq!(fs::read(&part_path).unwrap(), external_file_bytes);
                assert_eq!(
                    hash_file(&part_path),
                    <[u8; 32]>::from(Sha256::digest(external_file_bytes))
                );
            }
        }
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
