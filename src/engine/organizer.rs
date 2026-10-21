use crate::core::safety::{
    is_path_inside, safe_remove_dir_all, safe_remove_file, same_file_identity, unique_dest_path,
    AnchoredDir, ReadLease, SafetyError,
};
use crate::engine::tray::{fast_md5, full_sha256};
use crate::engine::walk_user_mods;
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

/// Caracteres que não podem entrar num nome de arquivo ou pasta. Mesma lista do
/// `organizer_tab.py`: o app roda no Linux, mas a pasta `Mods` costuma ser
/// compartilhada com instalações Windows via drive comum.
pub const FORBIDDEN_NAME_CHARS: [char; 9] = ['/', '\\', ':', '*', '?', '"', '<', '>', '|'];

#[derive(Debug, thiserror::Error)]
pub enum OrganizerError {
    #[error(
        "Escolha um nome válido: '{0}' não sobrou nada depois de remover os caracteres proibidos"
    )]
    InvalidName(String),
    #[error("Já existe um item chamado '{0}' nesta pasta")]
    AlreadyExists(String),
    #[error("A raiz da pasta Mods não pode ser usada nesta operação")]
    ModsRootProtected,
    #[error("Destino inválido: '{0}' fica dentro da própria pasta que está sendo movida")]
    DestinationInsideSource(String),
    #[error(transparent)]
    Safety(#[from] SafetyError),
    #[error("Erro de I/O: {0}")]
    Io(#[from] io::Error),
}

/// Limpa um nome digitado pelo usuário. `None` quando não sobra nada de útil.
pub fn sanitize_entry_name(raw: &str) -> Option<String> {
    let clean: String = raw
        .chars()
        .filter(|c| !FORBIDDEN_NAME_CHARS.contains(c))
        .collect();
    let trimmed = clean.trim().trim_matches('.').trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn is_mods_root(path: &Path, mods_dir: &Path) -> bool {
    match (
        crate::core::safety::canonical_path(path),
        crate::core::safety::canonical_path(mods_dir),
    ) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

/// Recusa qualquer caminho que não esteja dentro de `Mods`, e opcionalmente a
/// própria raiz — renomear ou apagar a raiz de `Mods` levaria a biblioteca
/// inteira junto.
fn validate_target(path: &Path, mods_dir: &Path, allow_root: bool) -> Result<(), OrganizerError> {
    if is_mods_root(path, mods_dir) {
        return if allow_root {
            Ok(())
        } else {
            Err(OrganizerError::ModsRootProtected)
        };
    }
    if !is_path_inside(path, mods_dir) {
        return Err(SafetyError::OutsideAllowedScope(
            path.display().to_string(),
            mods_dir.display().to_string(),
        )
        .into());
    }
    Ok(())
}

fn scoped_dir(
    root: &AnchoredDir,
    mods_dir: &Path,
    path: &Path,
    create: bool,
) -> io::Result<AnchoredDir> {
    let relative = path
        .strip_prefix(mods_dir)
        .map_err(|_| io::Error::new(io::ErrorKind::PermissionDenied, "Caminho fora de Mods"))?;
    let mut current = root.try_clone()?;
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Componente de caminho inseguro",
            ));
        };
        current = if create {
            current.create_child(name)?
        } else {
            current.child(name)?
        };
    }
    Ok(current)
}

fn scoped_parent(root: &AnchoredDir, mods_dir: &Path, path: &Path) -> io::Result<AnchoredDir> {
    scoped_dir(
        root,
        mods_dir,
        path.parent()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Caminho sem pasta pai"))?,
        false,
    )
}

/// Cria uma subpasta dentro de `Mods`.
pub fn create_folder(
    parent: &Path,
    raw_name: &str,
    mods_dir: &Path,
) -> Result<PathBuf, OrganizerError> {
    validate_target(parent, mods_dir, true)?;

    let name = sanitize_entry_name(raw_name)
        .ok_or_else(|| OrganizerError::InvalidName(raw_name.to_string()))?;
    let new_dir = parent.join(&name);

    if new_dir.exists() {
        return Err(OrganizerError::AlreadyExists(name));
    }

    let root = AnchoredDir::open(mods_dir)?;
    let _lock = root.lock_exclusive()?;
    let parent = scoped_dir(&root, mods_dir, parent, false)?;
    fs::create_dir(parent.path().join(&name))?;
    Ok(new_dir)
}

/// Renomeia um arquivo ou pasta, mantendo-o no mesmo diretório.
pub fn rename_item(
    path: &Path,
    raw_name: &str,
    mods_dir: &Path,
) -> Result<PathBuf, OrganizerError> {
    validate_target(path, mods_dir, false)?;

    let name = sanitize_entry_name(raw_name)
        .ok_or_else(|| OrganizerError::InvalidName(raw_name.to_string()))?;
    let dest = path.parent().unwrap_or(mods_dir).join(&name);

    if dest == path {
        return Ok(dest);
    }
    let root = AnchoredDir::open(mods_dir)?;
    let _lock = root.lock_exclusive()?;
    let parent = scoped_parent(&root, mods_dir, path)?;
    match parent.publish_noreplace(path.file_name().unwrap_or_default(), &parent, &name) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            return Err(OrganizerError::AlreadyExists(name));
        }
        Err(e) => return Err(e.into()),
    }
    Ok(dest)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferMode {
    Move,
    Copy,
}

#[derive(Debug, Default)]
pub struct TransferOutcome {
    pub done: usize,
    pub failures: Vec<(PathBuf, String)>,
    /// Scripts que ficaram fundos demais depois da transferência. Não é erro:
    /// o arquivo foi movido, mas o jogo não vai carregá-lo onde ele está.
    pub script_warnings: Vec<PathBuf>,
}

/// Move ou copia itens para outra pasta dentro de `Mods`.
///
/// Uma falha isolada não aborta o lote — vira uma entrada em `failures`, como
/// no resto do app.
pub fn transfer_items(
    sources: &[PathBuf],
    dest_folder: &Path,
    mode: TransferMode,
    mods_dir: &Path,
) -> Result<TransferOutcome, OrganizerError> {
    validate_target(dest_folder, mods_dir, true)?;
    let root = AnchoredDir::open(mods_dir)?;
    let _lock = root.lock_exclusive()?;
    let destination = scoped_dir(&root, mods_dir, dest_folder, true)?;

    let mut outcome = TransferOutcome::default();

    for src in sources {
        if let Err(e) = validate_target(src, mods_dir, false) {
            outcome.failures.push((src.clone(), e.to_string()));
            continue;
        }

        // Mover uma pasta para dentro dela mesma apaga a origem no meio do
        // caminho: o `fs::rename` falha, mas o `copy` recursivo entraria em
        // laço criando cópias dentro de cópias.
        if src.is_dir() && (dest_folder == src.as_path() || is_path_inside(dest_folder, src)) {
            outcome.failures.push((
                src.clone(),
                OrganizerError::DestinationInsideSource(dest_folder.display().to_string())
                    .to_string(),
            ));
            continue;
        }

        let filename = src.file_name().unwrap_or_default();
        let result = scoped_parent(&root, mods_dir, src).and_then(|parent| match mode {
            TransferMode::Copy => copy_entry(&parent, filename, &destination),
            TransferMode::Move => move_entry(&parent, filename, &destination, src),
        });

        match result {
            Ok(name) => {
                outcome.done += 1;
                collect_script_depth_warnings(
                    &dest_folder.join(name),
                    mods_dir,
                    &mut outcome.script_warnings,
                );
            }
            Err(e) => outcome.failures.push((src.clone(), e.to_string())),
        }
    }

    Ok(outcome)
}

/// A publicação nunca substitui uma entrada, mesmo que ela apareça depois da
/// escolha do nome. Uma árvore copiada só fica visível quando está completa.
fn publish_unique(
    source: &AnchoredDir,
    source_name: &OsStr,
    destination: &AnchoredDir,
    filename: &OsStr,
) -> io::Result<OsString> {
    loop {
        let candidate = unique_dest_path(&destination.path(), &filename.to_string_lossy());
        let name = candidate.file_name().unwrap_or_default();
        match source.publish_noreplace(source_name, destination, name) {
            Ok(()) => return Ok(name.to_owned()),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
}

fn copy_recursive(
    source: &AnchoredDir,
    name: &OsStr,
    destination: &AnchoredDir,
    destination_name: &OsStr,
    leases: &mut Vec<ReadLease>,
) -> io::Result<()> {
    let metadata = fs::symlink_metadata(source.path().join(name))?;
    if metadata.file_type().is_symlink() {
        let target = fs::read_link(source.path().join(name))?;
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&target, destination.path().join(destination_name))?;
            return Ok(());
        }
        #[cfg(not(unix))]
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Cópia de symlink indisponível",
        ));
    }
    if metadata.is_file() {
        let lease = ReadLease::acquire(source.open_regular(name)?)?;
        let mut input = &lease.file;
        let mut output = destination.create_new(destination_name)?;
        io::copy(&mut input, &mut output)?;
        output.set_permissions(lease.file.metadata()?.permissions())?;
        output.sync_all()?;
        lease.check()?;
        leases.push(lease);
        return Ok(());
    }
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Entrada não regular na pasta de origem",
        ));
    }
    let source_dir = source.child(name)?;
    let destination_dir = destination.create_child(destination_name)?;
    for entry in fs::read_dir(source_dir.path())? {
        let name = entry?.file_name();
        copy_recursive(&source_dir, &name, &destination_dir, &name, leases)?;
    }
    Ok(())
}

fn copy_entry(
    source: &AnchoredDir,
    name: &OsStr,
    destination: &AnchoredDir,
) -> io::Result<OsString> {
    let temporary = tempfile::Builder::new()
        .prefix(".s4suite-copy-")
        .tempdir_in(destination.path())?;
    let staging = destination.child(temporary.path().file_name().unwrap())?;
    let mut leases = Vec::new();
    copy_recursive(source, name, &staging, OsStr::new("item"), &mut leases)?;
    for lease in &leases {
        lease.check()?;
    }
    publish_unique(&staging, OsStr::new("item"), destination, name)
}

fn move_entry(
    source: &AnchoredDir,
    name: &OsStr,
    destination: &AnchoredDir,
    original: &Path,
) -> io::Result<OsString> {
    match publish_unique(source, name, destination, name) {
        Ok(name) => return Ok(name),
        Err(e) if e.raw_os_error() == Some(libc::EXDEV) => {}
        Err(e) => return Err(e),
    }
    if !fs::symlink_metadata(source.path().join(name))?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Mover árvores ou symlinks entre sistemas de arquivos não é seguro; origem mantida",
        ));
    }

    // Entre sistemas de arquivos, retiramos a origem para uma quarentena
    // privada antes de copiar. Não apagamos um caminho que possa ter recebido
    // novos dados enquanto a cópia estava em andamento.
    let quarantine = tempfile::Builder::new()
        .prefix(".s4suite-move-")
        .tempdir_in(source.path())?;
    let quarantine_name = quarantine.path().file_name().unwrap().to_owned();
    let held = source.child(&quarantine_name)?;
    source.publish_noreplace(name, &held, "item")?;
    let result = (|| {
        let temporary = tempfile::Builder::new()
            .prefix(".s4suite-copy-")
            .tempdir_in(destination.path())?;
        let staging = destination.child(temporary.path().file_name().unwrap())?;
        let lease = ReadLease::acquire(held.open_regular("item")?)?;
        let mut input = &lease.file;
        let mut output = staging.create_new("item")?;
        io::copy(&mut input, &mut output)?;
        output.set_permissions(lease.file.metadata()?.permissions())?;
        output.sync_all()?;
        lease.check()?;
        let published = publish_unique(&staging, OsStr::new("item"), destination, name)?;
        lease.check()?;
        if !same_file_identity(
            &held.open_regular("item")?.metadata()?,
            &lease.file.metadata()?,
        ) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Origem substituída na quarentena",
            ));
        }
        held.remove_file("item")?;
        Ok(published)
    })();
    match result {
        Ok(name) => Ok(name),
        Err(error) => match held.publish_noreplace("item", source, name) {
            Ok(()) => Err(error),
            Err(restore) => {
                let preserved = original
                    .parent()
                    .unwrap_or(Path::new(""))
                    .join(&quarantine_name)
                    .join("item");
                let _ = quarantine.keep();
                Err(io::Error::new(
                    error.kind(),
                    format!(
                        "{}; restauração recusada: {}. Origem preservada em {}",
                        error,
                        restore,
                        preserved.display()
                    ),
                ))
            }
        },
    }
}

/// Lista os `.ts4script` que ficaram fundos demais em `path`.
///
/// O jogo só carrega scripts até um nível de subpasta de `Mods`. Mover um mod
/// para `Mods/Categoria/Autor/` desliga o script sem qualquer aviso do jogo.
pub fn collect_script_depth_warnings(path: &Path, mods_dir: &Path, out: &mut Vec<PathBuf>) {
    let candidates: Vec<PathBuf> = if path.is_file() {
        vec![path.to_path_buf()]
    } else {
        walk_user_mods(path, usize::MAX)
            .filter(|e| e.file_type().is_file())
            .map(|e| e.path().to_path_buf())
            .collect()
    };

    for file in candidates {
        if !file
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("ts4script"))
            .unwrap_or(false)
        {
            continue;
        }
        let Ok(rel) = file.strip_prefix(mods_dir) else {
            continue;
        };
        // rel inclui o próprio arquivo: profundidade 2 é Mods/Sub/x.ts4script.
        if rel.components().count() > 2 {
            out.push(file);
        }
    }
}

/// Apaga arquivos **e pastas**, sempre pela camada de segurança.
///
/// Diferente de [`remove_files`], que é usada pela limpeza de lixo e por
/// duplicatas e recusa pastas de propósito: lá, uma pasta na lista só poderia
/// ser um engano.
pub fn remove_entries(paths: &[PathBuf], allowed_roots: &[PathBuf]) -> RemovalOutcome {
    let mut outcome = RemovalOutcome::default();

    for path in paths {
        let size = entry_size(path);
        let result = if path.is_dir() {
            safe_remove_dir_all(path, allowed_roots)
        } else {
            safe_remove_file(path, allowed_roots)
        };

        match result {
            Ok(()) => {
                outcome.removed += 1;
                outcome.freed_bytes += size;
            }
            Err(e) => outcome.failures.push((path.clone(), e.to_string())),
        }
    }

    outcome
}

fn entry_size(path: &Path) -> u64 {
    if path.is_file() {
        return fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    }
    walkdir::WalkDir::new(path)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| e.metadata().ok())
        .map(|m| m.len())
        .sum()
}

#[derive(Debug, Clone)]
pub struct ModNode {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub size: u64,
    pub disabled: bool,
}

pub fn scan_mods_tree(mods_dir: &Path) -> Vec<ModNode> {
    let mut nodes = Vec::new();
    if !mods_dir.exists() {
        return nodes;
    }

    for entry in walk_user_mods(mods_dir, 5) {
        let path = entry.path();
        if path == mods_dir {
            continue;
        }

        let name = entry.file_name().to_string_lossy().to_string();
        let is_dir = entry.file_type().is_dir();
        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
        let disabled = name.ends_with(".disabled");

        nodes.push(ModNode {
            name,
            path: path.to_path_buf(),
            is_dir,
            size,
            disabled,
        });
    }

    nodes.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then(a.name.cmp(&b.name)));
    nodes
}

/// Filtra a árvore por texto, sem perder o caminho até o que casou.
///
/// A árvore é plana, mas a UI a desenha aninhada: mostrar só as linhas que
/// casam deixaria os resultados órfãos, pendurados sob pastas que sumiram da
/// lista. Por isso os ancestrais de cada acerto vêm junto.
pub fn filter_tree(nodes: &[ModNode], query: &str) -> Vec<ModNode> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return nodes.to_vec();
    }

    let mut keep: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    for node in nodes
        .iter()
        .filter(|n| n.name.to_lowercase().contains(&needle))
    {
        keep.insert(node.path.clone());
        let mut cursor = node.path.parent();
        while let Some(parent) = cursor {
            if !keep.insert(parent.to_path_buf()) {
                break;
            }
            cursor = parent.parent();
        }
    }

    nodes
        .iter()
        .filter(|n| keep.contains(&n.path))
        .cloned()
        .collect()
}

#[derive(Debug, Clone)]
pub struct ScriptDepthIssue {
    pub file_path: PathBuf,
    pub filename: String,
    pub depth: usize,
}

pub fn check_script_depth(mods_dir: &Path) -> Vec<ScriptDepthIssue> {
    let mut issues = Vec::new();
    if !mods_dir.exists() {
        return issues;
    }

    for entry in walk_user_mods(mods_dir, usize::MAX) {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        let fname = entry.file_name().to_string_lossy().to_string();
        if fname.to_lowercase().ends_with(".ts4script") {
            if let Ok(rel) = path.strip_prefix(mods_dir) {
                let depth = rel.components().count();
                if depth > 2 {
                    // Nested deeper than Mods/Subfolder/mod.ts4script (depth > 2 components)
                    issues.push(ScriptDepthIssue {
                        file_path: path.to_path_buf(),
                        filename: fname,
                        depth,
                    });
                }
            }
        }
    }

    issues
}

pub fn auto_fix_script_depth(
    issues: &[ScriptDepthIssue],
    mods_dir: &Path,
) -> Result<usize, io::Error> {
    let sources: Vec<_> = issues.iter().map(|issue| issue.file_path.clone()).collect();
    if sources.is_empty() {
        return Ok(0);
    }
    let outcome = transfer_items(
        &sources,
        &mods_dir.join("00_Scripts_Corrigidos"),
        TransferMode::Move,
        mods_dir,
    )
    .map_err(|error| io::Error::new(io::ErrorKind::PermissionDenied, error.to_string()))?;
    if !outcome.failures.is_empty() {
        return Err(io::Error::other(format!(
            "{} script(s) movidos; falhas: {}",
            outcome.done,
            outcome
                .failures
                .iter()
                .map(|(path, error)| format!("{}: {}", path.display(), error))
                .collect::<Vec<_>>()
                .join("; "),
        )));
    }
    Ok(outcome.done)
}

#[derive(Debug, Clone)]
pub struct DuplicateGroup {
    pub fast_hash: String,
    pub sha256_hash: String,
    pub file_paths: Vec<PathBuf>,
}

pub fn detect_duplicates(mods_dir: &Path) -> Vec<DuplicateGroup> {
    let mut fast_map: HashMap<String, Vec<PathBuf>> = HashMap::new();

    for entry in walk_user_mods(mods_dir, usize::MAX) {
        let path = entry.path();
        if path.is_file()
            && path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_lowercase()
                == "package"
        {
            if let Ok(hash) = fast_md5(path) {
                fast_map.entry(hash).or_default().push(path.to_path_buf());
            }
        }
    }

    let mut duplicate_groups = Vec::new();

    for (fast_hash, paths) in fast_map {
        if paths.len() > 1 {
            let mut sha_map: HashMap<String, Vec<PathBuf>> = HashMap::new();
            for p in paths {
                if let Ok(sha) = full_sha256(&p) {
                    sha_map.entry(sha).or_default().push(p);
                }
            }

            for (sha256_hash, file_paths) in sha_map {
                if file_paths.len() > 1 {
                    duplicate_groups.push(DuplicateGroup {
                        fast_hash: fast_hash.clone(),
                        sha256_hash,
                        file_paths,
                    });
                }
            }
        }
    }

    duplicate_groups
}

impl DuplicateGroup {
    /// O arquivo que fica. Escolhemos o de caminho mais curto: mods soltos na
    /// raiz de `Mods` costumam ser a cópia "oficial", e as duplicatas tendem a
    /// estar enterradas em subpastas de instalações antigas.
    pub fn keeper(&self) -> &PathBuf {
        self.file_paths
            .iter()
            .min_by_key(|p| (p.components().count(), p.as_os_str().len()))
            .unwrap_or(&self.file_paths[0])
    }

    /// As cópias redundantes — tudo menos o `keeper`.
    pub fn redundant(&self) -> Vec<PathBuf> {
        let keeper = self.keeper().clone();
        self.file_paths
            .iter()
            .filter(|p| **p != keeper)
            .cloned()
            .collect()
    }
}

/// Resultado de uma remoção em lote.
#[derive(Debug, Default)]
pub struct RemovalOutcome {
    pub removed: usize,
    pub freed_bytes: u64,
    pub failures: Vec<(PathBuf, String)>,
}

/// Revalida tanto a cópia mantida quanto os bytes de cada redundante. Uma
/// confirmação antiga nunca autoriza excluir um arquivo novo no mesmo nome.
pub fn remove_duplicates(groups: &[DuplicateGroup], mods_dir: &Path) -> RemovalOutcome {
    let mut outcome = RemovalOutcome::default();
    let root = match AnchoredDir::open(mods_dir) {
        Ok(root) => root,
        Err(error) => {
            for group in groups.iter().filter(|group| group.file_paths.len() > 1) {
                outcome.failures.extend(
                    group
                        .redundant()
                        .into_iter()
                        .map(|path| (path, error.to_string())),
                );
            }
            return outcome;
        }
    };
    let _lock = match root.lock_exclusive() {
        Ok(lock) => lock,
        Err(error) => {
            for group in groups.iter().filter(|group| group.file_paths.len() > 1) {
                outcome.failures.extend(
                    group
                        .redundant()
                        .into_iter()
                        .map(|path| (path, error.to_string())),
                );
            }
            return outcome;
        }
    };
    for group in groups.iter().filter(|group| group.file_paths.len() > 1) {
        let keeper_path = group.keeper();
        let keeper = (|| {
            let parent = scoped_parent(&root, mods_dir, keeper_path)?;
            let lease = ReadLease::acquire(
                parent.open_regular(keeper_path.file_name().unwrap_or_default())?,
            )?;
            if crate::engine::tray::sha256_reader(&lease.file)? != group.sha256_hash {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Cópia mantida foi modificada",
                ));
            }
            lease.check()?;
            Ok((parent, lease))
        })();
        let (keeper_parent, keeper_lease) = match keeper {
            Ok(keeper) => keeper,
            Err(error) => {
                outcome.failures.extend(
                    group
                        .redundant()
                        .into_iter()
                        .map(|path| (path, error.to_string())),
                );
                continue;
            }
        };
        for path in group.redundant() {
            let result = (|| {
                let parent = scoped_parent(&root, mods_dir, &path)?;
                let temporary = tempfile::Builder::new()
                    .prefix(".s4suite-duplicate-")
                    .tempdir_in(parent.path())?;
                let temporary_name = temporary.path().file_name().unwrap().to_owned();
                let held = parent.child(&temporary_name)?;
                let name = path.file_name().unwrap_or_default();
                parent.publish_noreplace(name, &held, "item")?;
                let remove = (|| {
                    let lease = ReadLease::acquire(held.open_regular("item")?)?;
                    let metadata = lease.file.metadata()?;
                    if crate::engine::tray::sha256_reader(&lease.file)? != group.sha256_hash {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "Duplicata modificada depois da análise",
                        ));
                    }
                    lease.check()?;
                    keeper_lease.check()?;
                    if !same_file_identity(
                        &keeper_parent
                            .open_regular(keeper_path.file_name().unwrap_or_default())?
                            .metadata()?,
                        &keeper_lease.file.metadata()?,
                    ) || !same_file_identity(&held.open_regular("item")?.metadata()?, &metadata)
                    {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "Arquivo substituído depois da análise",
                        ));
                    }
                    held.remove_file("item")?;
                    Ok(metadata.len())
                })();
                match remove {
                    Ok(size) => Ok(size),
                    Err(error) => match held.publish_noreplace("item", &parent, name) {
                        Ok(()) => Err(error),
                        Err(restore) => {
                            let preserved =
                                path.parent().unwrap().join(temporary_name).join("item");
                            let _ = temporary.keep();
                            Err(io::Error::new(
                                error.kind(),
                                format!(
                                    "{}; restauração recusada: {}. Arquivo preservado em {}",
                                    error,
                                    restore,
                                    preserved.display()
                                ),
                            ))
                        }
                    },
                }
            })();
            match result {
                Ok(size) => {
                    outcome.removed += 1;
                    outcome.freed_bytes += size;
                }
                Err(error) => outcome.failures.push((path, error.to_string())),
            }
        }
    }
    outcome
}

/// Apaga os arquivos indicados, sempre por [`safe_remove_file`], que recusa
/// qualquer caminho fora das raízes permitidas. Um arquivo protegido não
/// interrompe o lote: vira uma entrada em `failures`.
pub fn remove_files(files: &[PathBuf], allowed_roots: &[PathBuf]) -> RemovalOutcome {
    let mut outcome = RemovalOutcome::default();

    for file in files {
        let size = fs::metadata(file).map(|m| m.len()).unwrap_or(0);
        match safe_remove_file(file, allowed_roots) {
            Ok(()) => {
                outcome.removed += 1;
                outcome.freed_bytes += size;
            }
            Err(e) => outcome.failures.push((file.clone(), e.to_string())),
        }
    }

    outcome
}

pub fn find_junk_files(mods_dir: &Path) -> Vec<PathBuf> {
    let junk_exts = ["txt", "url", "png", "jpg", "jpeg", "db", "s4tmp"];
    let mut junk = Vec::new();

    for entry in walk_user_mods(mods_dir, usize::MAX) {
        let path = entry.path();
        if path.is_file() {
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_lowercase();
            if junk_exts.contains(&ext.as_str()) {
                junk.push(path.to_path_buf());
            }
        }
    }

    junk
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_script_depth_fixer() {
        let dir = tempdir().unwrap();
        let mods_dir = dir.path().join("Mods");
        let deep_folder = mods_dir
            .join("Category")
            .join("SubCategory")
            .join("DeepFolder");
        fs::create_dir_all(&deep_folder).unwrap();

        let script_file = deep_folder.join("nested_mod.ts4script");
        fs::write(&script_file, b"dummy script content").unwrap();

        let issues = check_script_depth(&mods_dir);
        assert_eq!(issues.len(), 1);

        let fixed = auto_fix_script_depth(&issues, &mods_dir).unwrap();
        assert_eq!(fixed, 1);

        let corrected_file = mods_dir
            .join("00_Scripts_Corrigidos")
            .join("nested_mod.ts4script");
        assert!(corrected_file.exists());
    }
}
