use crate::core::safety::{same_file_identity, unique_dest_path, AnchoredDir, ReadLease};
use md5::{Digest, Md5};
use regex::bytes::Regex;
use rusqlite::{params, Connection};
use sha2::Sha256;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;
use walkdir::WalkDir;

pub struct TrayCacheDb {
    conn: Connection,
}

impl TrayCacheDb {
    pub fn open(db_path: &Path) -> Result<Self, rusqlite::Error> {
        if let Some(parent) = db_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let conn = Connection::open(db_path)?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             CREATE TABLE IF NOT EXISTS mod_files (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 filepath TEXT UNIQUE,
                 mtime REAL,
                 md5 TEXT
             );
             CREATE TABLE IF NOT EXISTS mod_resources (
                 type INTEGER,
                 group_id INTEGER,
                 instance_ex INTEGER,
                 instance_low INTEGER,
                 file_id INTEGER,
                 FOREIGN KEY(file_id) REFERENCES mod_files(id)
             );
             CREATE INDEX IF NOT EXISTS idx_resource ON mod_resources (type, group_id, instance_ex, instance_low);
             CREATE INDEX IF NOT EXISTS idx_md5 ON mod_files (md5);",
        )?;
        Ok(Self { conn })
    }

    pub fn update_file_md5(
        &self,
        filepath: &Path,
        mtime: f64,
        md5: &str,
    ) -> Result<(), rusqlite::Error> {
        let path_str = filepath.display().to_string();
        self.conn.execute(
            "INSERT INTO mod_files (filepath, mtime, md5) VALUES (?1, ?2, ?3)
             ON CONFLICT(filepath) DO UPDATE SET mtime=?2, md5=?3",
            params![path_str, mtime, md5],
        )?;
        Ok(())
    }

    pub fn find_exact_duplicate(
        &self,
        package_path: &Path,
    ) -> Result<Option<PathBuf>, Box<dyn std::error::Error + Send + Sync>> {
        let fast_hash = fast_md5(package_path)?;
        let mut stmt = self
            .conn
            .prepare("SELECT filepath FROM mod_files WHERE md5=?1")?;
        let rows = stmt.query_map(params![fast_hash], |row| row.get::<_, String>(0))?;

        let candidate_paths: Vec<PathBuf> =
            rows.filter_map(|r| r.ok()).map(PathBuf::from).collect();
        if candidate_paths.is_empty() {
            return Ok(None);
        }

        let target_sha = full_sha256(package_path)?;
        for existing in candidate_paths {
            if existing.exists() {
                if let Ok(existing_sha) = full_sha256(&existing) {
                    if existing_sha == target_sha {
                        return Ok(Some(existing));
                    }
                }
            }
        }

        Ok(None)
    }
}

pub fn fast_md5(path: &Path) -> io::Result<String> {
    let metadata = fs::metadata(path)?;
    let size = metadata.len();

    let mut hasher = Md5::new();
    hasher.update(size.to_string().as_bytes());

    let mut file = File::open(path)?;
    let mut buffer = vec![0u8; 1024 * 1024]; // First 1MB
    let read_bytes = file.read(&mut buffer)?;
    hasher.update(&buffer[..read_bytes]);

    Ok(format!("{:x}", hasher.finalize()))
}

pub fn full_sha256(path: &Path) -> io::Result<String> {
    sha256_reader(File::open(path)?)
}

pub(crate) fn sha256_reader(mut reader: impl Read) -> io::Result<String> {
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 1024 * 1024];

    loop {
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }

    Ok(format!("{:x}", hasher.finalize()))
}

pub fn parse_tray_name(trayitem_path: &Path) -> Option<String> {
    let mut file = File::open(trayitem_path).ok()?;
    let mut data = Vec::new();
    file.read_to_end(&mut data).ok()?;

    // Search for XML tag <N>...</N>
    let re_xml = Regex::new(r"<N>(.*?)</N>").unwrap();
    if let Some(captures) = re_xml.captures(&data) {
        let match_bytes = captures.get(1)?.as_bytes();
        if match_bytes.contains(&0x00) {
            let (decoded, _, _) = encoding_rs::UTF_16LE.decode(match_bytes);
            let name = decoded.trim().to_string();
            if !name.is_empty() {
                return Some(name);
            }
        } else {
            let name = String::from_utf8_lossy(match_bytes).trim().to_string();
            if !name.is_empty() {
                return Some(name);
            }
        }
    }

    // Fallback: search for long text strings
    let re_fallback = Regex::new(r"\x00\x00\x00([A-Z][a-z]{2,20} [A-Z][a-z]{2,20})\x00").unwrap();
    if let Some(captures) = re_fallback.captures(&data) {
        let name = String::from_utf8_lossy(captures.get(1)?.as_bytes())
            .trim()
            .to_string();
        if !name.is_empty() {
            return Some(name);
        }
    }

    None
}

/// Reduz o nome detectado a algo seguro como nome de pasta.
///
/// Segue o `s4tray.py`: caracteres fora do conjunto permitido são **removidos**,
/// não substituídos. Remover deixa buracos — `"Maria / Silva"` virava
/// `"Maria  Silva"`, com espaço duplo no nome da pasta importada — então os
/// espaços resultantes são colapsados em um só.
pub fn sanitize_import_name(name: Option<&str>) -> String {
    static ALLOWED: OnceLock<regex::Regex> = OnceLock::new();
    static SPACES: OnceLock<regex::Regex> = OnceLock::new();

    let allowed = ALLOWED.get_or_init(|| regex::Regex::new(r"[^a-zA-Z0-9\-_ ]").unwrap());
    let spaces = SPACES.get_or_init(|| regex::Regex::new(r" {2,}").unwrap());

    let stripped = allowed.replace_all(name.unwrap_or(""), "");
    let clean = spaces.replace_all(stripped.trim(), " ").to_string();
    if clean.is_empty() {
        "Imported_Item".to_string()
    } else {
        clean
    }
}

#[derive(Debug, Clone)]
pub struct TrayFileEntry {
    pub src_path: PathBuf,
    pub rel_name: String,
    pub is_tray_binary: bool,
    pub is_package: bool,
    pub is_duplicate: bool,
}

#[derive(Debug, Clone)]
pub struct TrayAnalysisResult {
    pub detected_name: String,
    pub tray_files: Vec<TrayFileEntry>,
    pub package_files: Vec<TrayFileEntry>,
    pub duplicate_count: usize,
}

pub fn analyze_tray_source(
    source_root: &Path,
    cache_db: Option<&TrayCacheDb>,
) -> Result<TrayAnalysisResult, Box<dyn std::error::Error + Send + Sync>> {
    let mut detected_name = None;
    let mut tray_files = Vec::new();
    let mut package_files = Vec::new();
    let mut duplicate_count = 0;

    let tray_exts = [
        "householdbinary",
        "trayitem",
        "sgi",
        "hhi",
        "bpi",
        "blueprint",
        "room",
        "rmi",
    ];

    for entry in WalkDir::new(source_root).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        let fname = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        if ext == "trayitem" && detected_name.is_none() {
            detected_name = parse_tray_name(path);
        }

        if tray_exts.contains(&ext.as_str()) {
            tray_files.push(TrayFileEntry {
                src_path: path.to_path_buf(),
                rel_name: fname,
                is_tray_binary: true,
                is_package: false,
                is_duplicate: false,
            });
        } else if ext == "package" {
            let mut is_dup = false;
            if let Some(db) = cache_db {
                if let Ok(Some(_)) = db.find_exact_duplicate(path) {
                    is_dup = true;
                    duplicate_count += 1;
                }
            }
            package_files.push(TrayFileEntry {
                src_path: path.to_path_buf(),
                rel_name: fname,
                is_tray_binary: false,
                is_package: true,
                is_duplicate: is_dup,
            });
        }
    }

    Ok(TrayAnalysisResult {
        detected_name: sanitize_import_name(detected_name.as_deref()),
        tray_files,
        package_files,
        duplicate_count,
    })
}

/// Diretório de trabalho onde as fontes do tray são extraídas antes da
/// importação. Fica fora de Mods e do Tray, e é limpo a cada preparação.
pub const TRAY_WORK_DIR: &str = "tray_staging";

/// Uma fonte selecionada pelo usuário, já extraída e analisada.
pub struct TrayImportCandidate {
    pub source: PathBuf,
    pub analysis: TrayAnalysisResult,
    /// Para onde vai o CC deste item. `None` usa [`default_cc_dir`].
    pub cc_target: Option<PathBuf>,
    /// Item marcado para não ser importado. Continua na fila à vista, para o
    /// usuário poder voltar atrás antes de confirmar.
    pub skipped: bool,
}

impl TrayImportCandidate {
    pub fn total_files(&self) -> usize {
        self.analysis.tray_files.len() + self.analysis.package_files.len()
    }

    /// Destino efetivo do CC, resolvendo o padrão quando nada foi escolhido.
    pub fn cc_dir(&self, mods_dir: &Path) -> PathBuf {
        self.cc_target
            .clone()
            .unwrap_or_else(|| default_cc_dir(mods_dir, &self.analysis.detected_name))
    }

    /// Rótulo do tipo de conteúdo, a partir do que foi realmente encontrado.
    pub fn kind_label(&self) -> &'static str {
        match (
            self.analysis.tray_files.is_empty(),
            self.analysis.package_files.is_empty(),
        ) {
            (false, false) => "Sim / Lote + CC",
            (false, true) => "Sim / Lote",
            (true, false) => "Somente CC",
            (true, true) => "Vazio",
        }
    }
}

/// Extrai e analisa cada fonte escolhida pelo usuário.
///
/// Aceita `.zip`/`.7z`/`.rar` e também arquivos de tray soltos. Uma fonte que
/// falhe não derruba as demais: ela vira uma entrada na lista de erros.
#[allow(clippy::type_complexity)]
pub fn prepare_tray_candidates(
    sources: &[PathBuf],
    work_root: &Path,
    cache_db: Option<&TrayCacheDb>,
) -> Result<
    (Vec<TrayImportCandidate>, Vec<(String, String)>),
    Box<dyn std::error::Error + Send + Sync>,
> {
    if work_root.exists() {
        fs::remove_dir_all(work_root)?;
    }
    fs::create_dir_all(work_root)?;

    let mut candidates = Vec::new();
    let mut failures = Vec::new();

    for (idx, source) in sources.iter().enumerate() {
        let label = source
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| source.display().to_string());

        // O índice evita colisão entre dois arquivos de mesmo nome.
        let dest = work_root.join(format!("{:03}_{}", idx, sanitize_import_name(Some(&label))));

        let prepared = if is_tray_archive(source) {
            crate::engine::installer::extract_archive(source, &dest)
                .map(|_| ())
                .map_err(|e| e.to_string())
        } else {
            copy_loose_tray_file(source, &dest).map_err(|e| e.to_string())
        };

        if let Err(e) = prepared {
            let _ = fs::remove_dir_all(&dest);
            failures.push((label, e));
            continue;
        }

        match analyze_tray_source(&dest, cache_db) {
            Ok(analysis) if analysis.tray_files.is_empty() && analysis.package_files.is_empty() => {
                let _ = fs::remove_dir_all(&dest);
                failures.push((
                    label,
                    "nenhum arquivo de Sim, Lote ou CC encontrado".to_string(),
                ));
            }
            Ok(analysis) => candidates.push(TrayImportCandidate {
                source: source.clone(),
                analysis,
                cc_target: None,
                skipped: false,
            }),
            Err(e) => {
                let _ = fs::remove_dir_all(&dest);
                failures.push((label, e.to_string()));
            }
        }
    }

    Ok((candidates, failures))
}

fn is_tray_archive(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase()
            .as_str(),
        "zip" | "7z" | "rar"
    )
}

fn copy_loose_tray_file(source: &Path, dest_dir: &Path) -> io::Result<()> {
    let filename = source
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    fs::create_dir_all(dest_dir)?;
    fs::copy(source, dest_dir.join(filename))?;
    Ok(())
}

/// Remove o diretório de trabalho. Seguro de chamar quando ele não existe.
pub fn clear_tray_work_dir(work_root: &Path) -> io::Result<()> {
    if work_root.exists() {
        fs::remove_dir_all(work_root)?;
    }
    Ok(())
}

/// Pasta padrão do CC que acompanha um Sim ou Lote importado.
///
/// O usuário pode trocá-la item a item na fila — um Sim que só traz cabelos
/// costuma ir para a pasta de cabelos, não para uma pasta com o nome do Sim.
pub fn default_cc_dir(mods_dir: &Path, detected_name: &str) -> PathBuf {
    mods_dir.join("Imported_Sims").join(detected_name)
}

fn allowed_import_root<'a>(path: &Path, roots: &'a [PathBuf]) -> io::Result<&'a Path> {
    if path
        .components()
        .any(|part| matches!(part, Component::ParentDir))
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Destino com componente inseguro",
        ));
    }
    roots
        .iter()
        .filter(|root| path.starts_with(root))
        .max_by_key(|root| root.components().count())
        .map(PathBuf::as_path)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Destino fora das raízes permitidas",
            )
        })
}

fn import_directory(root: &AnchoredDir, root_path: &Path, path: &Path) -> io::Result<AnchoredDir> {
    let mut directory = root.try_clone()?;
    for component in path
        .strip_prefix(root_path)
        .map_err(|_| io::Error::new(io::ErrorKind::PermissionDenied, "Destino fora da raiz"))?
        .components()
    {
        let Component::Normal(name) = component else {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Destino inválido",
            ));
        };
        directory = directory.create_child(name)?;
    }
    Ok(directory)
}

fn copy_tray_entry(
    entry: &TrayFileEntry,
    staging: &AnchoredDir,
    destination: &AnchoredDir,
) -> io::Result<(OsString, ReadLease)> {
    let source_parent = AnchoredDir::open(
        entry
            .src_path
            .parent()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Fonte sem pai"))?,
    )?;
    crate::engine::installer::atomic_copy_at(
        &source_parent,
        entry.src_path.file_name().unwrap_or_default(),
        staging,
        "item",
    )?;
    let lease = ReadLease::acquire(staging.open_regular("item")?)?;
    loop {
        let candidate = unique_dest_path(&destination.path(), &entry.rel_name);
        let name = candidate.file_name().unwrap_or_default();
        lease.check()?;
        match staging.publish_noreplace("item", destination, name) {
            Ok(()) => return Ok((name.to_owned(), lease)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
}

fn rollback_tray_entry(
    destination: &AnchoredDir,
    directory_path: &Path,
    name: &std::ffi::OsStr,
    lease: &ReadLease,
) -> io::Result<()> {
    let temporary = tempfile::Builder::new()
        .prefix(".s4suite-tray-rollback-")
        .tempdir_in(destination.path())?;
    let temporary_name = temporary.path().file_name().unwrap().to_owned();
    let held = destination.child(&temporary_name)?;
    destination.publish_noreplace(name, &held, "item")?;
    let validation = lease.check().and_then(|()| {
        if same_file_identity(
            &held.open_regular("item")?.metadata()?,
            &lease.file.metadata()?,
        ) {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Destino substituído; rollback recusado",
            ))
        }
    });
    match validation {
        Ok(()) => held.remove_file("item"),
        Err(error) => match held.publish_noreplace("item", destination, name) {
            Ok(()) => Err(error),
            Err(restore) => {
                let preserved = directory_path.join(temporary_name).join("item");
                let _ = temporary.keep();
                Err(io::Error::new(
                    error.kind(),
                    format!(
                        "{}; restauração recusada: {}. Arquivo preservado em {}",
                        error,
                        restore,
                        preserved.display(),
                    ),
                ))
            }
        },
    }
}

/// Importa um item para o jogo: os binários de Tray vão para a pasta `Tray`, e
/// o CC para `cc_dir`.
pub fn import_tray_item(
    analysis: &TrayAnalysisResult,
    tray_dir: &Path,
    cc_dir: &Path,
    allowed_roots: &[PathBuf],
) -> Result<(usize, usize, usize), Box<dyn std::error::Error + Send + Sync>> {
    // Valida ambos antes de criar qualquer pasta, inclusive as próprias raízes.
    let tray_root_path = allowed_import_root(tray_dir, allowed_roots)?;
    let cc_root_path = allowed_import_root(cc_dir, allowed_roots)?;
    for entry in analysis.tray_files.iter().chain(&analysis.package_files) {
        let mut components = Path::new(&entry.rel_name).components();
        if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Nome de arquivo de Tray inválido",
            )
            .into());
        }
    }
    let tray_root = AnchoredDir::create(tray_root_path)?;
    let _tray_lock = tray_root.lock_exclusive()?;
    let cc_root = if cc_root_path == tray_root_path {
        tray_root.try_clone()?
    } else {
        AnchoredDir::create(cc_root_path)?
    };
    let _cc_lock = if cc_root_path == tray_root_path {
        None
    } else {
        Some(cc_root.lock_exclusive()?)
    };
    let tray_destination = import_directory(&tray_root, tray_root_path, tray_dir)?;
    let cc_destination = import_directory(&cc_root, cc_root_path, cc_dir)?;
    let tray_temporary = tempfile::Builder::new()
        .prefix(".s4suite-tray-")
        .tempdir_in(tray_destination.path())?;
    let cc_temporary = tempfile::Builder::new()
        .prefix(".s4suite-tray-")
        .tempdir_in(cc_destination.path())?;
    let tray_staging = tray_destination.child(tray_temporary.path().file_name().unwrap())?;
    let cc_staging = cc_destination.child(cc_temporary.path().file_name().unwrap())?;
    let mut moved_files: Vec<(bool, OsString, ReadLease)> = Vec::new();
    let mut tray_installed = 0;
    let mut cc_installed = 0;
    let mut skipped_duplicates = 0;

    let mut execute_moves = || -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        for file_entry in &analysis.tray_files {
            let (name, lease) = copy_tray_entry(file_entry, &tray_staging, &tray_destination)?;
            moved_files.push((true, name, lease));
            tray_installed += 1;
        }

        for file_entry in &analysis.package_files {
            if file_entry.is_duplicate {
                skipped_duplicates += 1;
                continue;
            }
            let (name, lease) = copy_tray_entry(file_entry, &cc_staging, &cc_destination)?;
            moved_files.push((false, name, lease));
            cc_installed += 1;
        }

        Ok(())
    };

    if let Err(e) = execute_moves() {
        let mut rollback_errors = Vec::new();
        for (is_tray, name, lease) in moved_files.into_iter().rev() {
            let (destination, path) = if is_tray {
                (&tray_destination, tray_dir)
            } else {
                (&cc_destination, cc_dir)
            };
            if let Err(error) = rollback_tray_entry(destination, path, &name, &lease) {
                rollback_errors.push(format!("{}: {}", path.join(name).display(), error));
            }
        }
        if !rollback_errors.is_empty() {
            return Err(format!(
                "Falha na importação: {}. Erros adicionais no rollback: {}",
                e,
                rollback_errors.join("; ")
            )
            .into());
        }
        return Err(e);
    }

    Ok((tray_installed, cc_installed, skipped_duplicates))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_fast_md5_sha256() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("sample.package");
        fs::write(&file_path, b"Test Package Binary Content Data").unwrap();

        let md5_hash = fast_md5(&file_path).expect("Fast MD5 failed");
        let sha256_hash = full_sha256(&file_path).expect("Full SHA-256 failed");

        assert_eq!(md5_hash.len(), 32);
        assert_eq!(sha256_hash.len(), 64);
    }

    #[test]
    fn p0_rollback_nao_apaga_arquivo_que_substituiu_a_publicacao() {
        let temporary = tempdir().unwrap();
        let path = temporary.path().join("sim.trayitem");
        let relocated = temporary.path().join("publicacao_movida.trayitem");
        fs::write(&path, b"PUBLICACAO_DESTA_IMPORTACAO").unwrap();
        let directory = AnchoredDir::open(temporary.path()).unwrap();
        let lease = ReadLease::acquire(directory.open_regular("sim.trayitem").unwrap()).unwrap();
        fs::rename(&path, &relocated).unwrap();
        fs::write(&path, b"NOVO_CONTEUDO_DO_USUARIO").unwrap();
        let new_hash = full_sha256(&path).unwrap();
        let relocated_hash = full_sha256(&relocated).unwrap();

        let result = rollback_tray_entry(
            &directory,
            temporary.path(),
            OsString::from("sim.trayitem").as_os_str(),
            &lease,
        );

        assert!(result.is_err(), "rollback aceitou inode diferente");
        assert_eq!(fs::read(&path).unwrap(), b"NOVO_CONTEUDO_DO_USUARIO");
        assert_eq!(full_sha256(&path).unwrap(), new_hash);
        assert_eq!(
            fs::read(&relocated).unwrap(),
            b"PUBLICACAO_DESTA_IMPORTACAO"
        );
        assert_eq!(full_sha256(&relocated).unwrap(), relocated_hash);
        assert_eq!(fs::read_dir(temporary.path()).unwrap().count(), 2);
    }
}
