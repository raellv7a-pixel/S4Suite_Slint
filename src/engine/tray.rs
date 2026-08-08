use crate::core::safety::{safe_remove_file, unique_dest_path};
use md5::{Digest, Md5};
use regex::bytes::Regex;
use rusqlite::{params, Connection};
use sha2::Sha256;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
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

    pub fn update_file_md5(&self, filepath: &Path, mtime: f64, md5: &str) -> Result<(), rusqlite::Error> {
        let path_str = filepath.display().to_string();
        self.conn.execute(
            "INSERT INTO mod_files (filepath, mtime, md5) VALUES (?1, ?2, ?3)
             ON CONFLICT(filepath) DO UPDATE SET mtime=?2, md5=?3",
            params![path_str, mtime, md5],
        )?;
        Ok(())
    }

    pub fn find_exact_duplicate(&self, package_path: &Path) -> Result<Option<PathBuf>, Box<dyn std::error::Error + Send + Sync>> {
        let fast_hash = fast_md5(package_path)?;
        let mut stmt = self.conn.prepare("SELECT filepath FROM mod_files WHERE md5=?1")?;
        let rows = stmt.query_map(params![fast_hash], |row| row.get::<_, String>(0))?;

        let candidate_paths: Vec<PathBuf> = rows.filter_map(|r| r.ok()).map(PathBuf::from).collect();
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
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 1024 * 1024];

    loop {
        let n = file.read(&mut buffer)?;
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
        let name = String::from_utf8_lossy(captures.get(1)?.as_bytes()).trim().to_string();
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

    let tray_exts = ["householdbinary", "trayitem", "sgi", "hhi", "bpi", "blueprint", "room", "rmi"];

    for entry in WalkDir::new(source_root).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        let fname = path.file_name().unwrap_or_default().to_string_lossy().to_string();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();

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
pub fn prepare_tray_candidates(
    sources: &[PathBuf],
    work_root: &Path,
    cache_db: Option<&TrayCacheDb>,
) -> Result<(Vec<TrayImportCandidate>, Vec<(String, String)>), Box<dyn std::error::Error + Send + Sync>> {
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
                failures.push((label, "nenhum arquivo de Sim, Lote ou CC encontrado".to_string()));
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
        path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase().as_str(),
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

/// Importa um item para o jogo: os binários de Tray vão para a pasta `Tray`, e
/// o CC para `cc_dir`.
pub fn import_tray_item(
    analysis: &TrayAnalysisResult,
    tray_dir: &Path,
    cc_dir: &Path,
    allowed_roots: &[PathBuf],
) -> Result<(usize, usize, usize), Box<dyn std::error::Error + Send + Sync>> {
    fs::create_dir_all(tray_dir)?;
    let dest_mods_folder = cc_dir.to_path_buf();
    fs::create_dir_all(&dest_mods_folder)?;

    let mut moved_files: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut tray_installed = 0;
    let mut cc_installed = 0;
    let mut skipped_duplicates = 0;

    let mut execute_moves = || -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        for file_entry in &analysis.tray_files {
            let dest = unique_dest_path(tray_dir, &file_entry.rel_name);
            fs::copy(&file_entry.src_path, &dest)?;
            moved_files.push((file_entry.src_path.clone(), dest));
            tray_installed += 1;
        }

        for file_entry in &analysis.package_files {
            if file_entry.is_duplicate {
                skipped_duplicates += 1;
                continue;
            }
            let dest = unique_dest_path(&dest_mods_folder, &file_entry.rel_name);
            fs::copy(&file_entry.src_path, &dest)?;
            moved_files.push((file_entry.src_path.clone(), dest));
            cc_installed += 1;
        }

        Ok(())
    };

    if let Err(e) = execute_moves() {
        // Rollback
        for (_, created_dest) in moved_files {
            let _ = safe_remove_file(&created_dest, allowed_roots);
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
}
