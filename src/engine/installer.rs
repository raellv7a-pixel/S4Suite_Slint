use crate::core::safety::{safe_remove_file, unique_dest_path};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use walkdir::WalkDir;

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

pub fn extract_archive_to_staging(
    archive_path: &Path,
    staging_dir: &Path,
) -> Result<PathBuf, InstallerError> {
    scan_archive_security(archive_path)?;
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

    // Fallback 7z process
    let status = Command::new("7z")
        .arg("x")
        .arg(archive_path)
        .arg(format!("-o{}", staging_dir.display()))
        .arg("-y")
        .status();

    match status {
        Ok(s) if s.success() => Ok(staging_dir.to_path_buf()),
        _ => Err(InstallerError::ExtractionFailed("Falha ao extrair arquivo compactado".to_string())),
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
    pub filename: String,
    pub size: u64,
    pub conflict: ConflictType,
    pub existing_dest: Option<PathBuf>,
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

    let mut existing_map: HashMap<String, (PathBuf, u64)> = HashMap::new();
    for entry in WalkDir::new(mods_dir).into_iter().filter_map(|e| e.ok()) {
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

        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
        if ext != "package" && ext != "ts4script" {
            continue;
        }

        let filename = entry.file_name().to_string_lossy().to_string();
        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
        let rel_path = path.strip_prefix(staging_dir).unwrap_or(path).to_path_buf();

        if let Some((existing_dest, existing_size)) = existing_map.get(&filename) {
            if *existing_size == size {
                exact_matches += 1;
                staged_files.push(StagedFile {
                    src_path: path.to_path_buf(),
                    rel_path,
                    filename,
                    size,
                    conflict: ConflictType::ExactMatch,
                    existing_dest: Some(existing_dest.clone()),
                });
            } else {
                updates += 1;
                staged_files.push(StagedFile {
                    src_path: path.to_path_buf(),
                    rel_path,
                    filename,
                    size,
                    conflict: ConflictType::SizeDiff,
                    existing_dest: Some(existing_dest.clone()),
                });
            }
        } else {
            new_files += 1;
            staged_files.push(StagedFile {
                src_path: path.to_path_buf(),
                rel_path,
                filename,
                size,
                conflict: ConflictType::NewFile,
                existing_dest: None,
            });
        }
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

pub fn execute_installation(
    report: &ConflictReport,
    mods_dir: &Path,
    allowed_roots: &[PathBuf],
) -> Result<(usize, usize), InstallerError> {
    let backup_dir = mods_dir.join(".s4suite_backups");
    fs::create_dir_all(&backup_dir)?;

    let mut installed_count = 0;
    let mut backup_copies: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut new_destinations: Vec<PathBuf> = Vec::new();

    let mut execute = || -> Result<(), io::Error> {
        for file in &report.staged_files {
            let target_dest = match &file.existing_dest {
                Some(existing) => existing.clone(),
                None => mods_dir.join(&file.filename),
            };

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

    Ok((installed_count, report.exact_matches))
}
