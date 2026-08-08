use std::path::{Path, PathBuf};
use std::fs;
use std::io;

#[derive(Debug, thiserror::Error)]
pub enum SafetyError {
    #[error("Caminho não encontrado ou inválido: {0}")]
    InvalidPath(String),
    #[error("Operação bloqueada por segurança: tentativa de excluir caminho protegido '{0}'")]
    ProtectedPath(String),
    #[error("Operação bloqueada: o caminho '{0}' está fora do diretório permitido '{1}'")]
    OutsideAllowedScope(String, String),
    #[error("Erro de I/O: {0}")]
    Io(#[from] io::Error),
}

pub fn canonical_path(path: &Path) -> Option<PathBuf> {
    dunce::canonicalize(path).ok().or_else(|| {
        if path.exists() {
            Some(path.to_path_buf())
        } else if let Some(parent) = path.parent() {
            dunce::canonicalize(parent).ok().map(|p| p.join(path.file_name().unwrap_or_default()))
        } else {
            Some(path.to_path_buf())
        }
    })
}

pub fn is_path_inside(child: &Path, parent: &Path) -> bool {
    let child_canon = match canonical_path(child) {
        Some(c) => c,
        None => return false,
    };
    let parent_canon = match canonical_path(parent) {
        Some(p) => p,
        None => return false,
    };

    child_canon != parent_canon && child_canon.starts_with(&parent_canon)
}

pub fn is_safe_to_delete(target: &Path, allowed_roots: &[PathBuf]) -> Result<PathBuf, SafetyError> {
    let target_canon = canonical_path(target)
        .ok_or_else(|| SafetyError::InvalidPath(target.display().to_string()))?;

    // Block root and system paths
    let target_str = target_canon.to_string_lossy();
    if target_str == "/" || target_str.starts_with("/usr") || target_str.starts_with("/bin") || target_str.starts_with("/sbin") || target_str.starts_with("/etc") || target_str.starts_with("/boot") || target_str.starts_with("/dev") || target_str.starts_with("/proc") || target_str.starts_with("/sys") {
        return Err(SafetyError::ProtectedPath(target_str.to_string()));
    }

    if let Some(home) = dirs::home_dir() {
        if let Some(home_canon) = canonical_path(&home) {
            if target_canon == home_canon {
                return Err(SafetyError::ProtectedPath(target_str.to_string()));
            }
            // Check Documents / Desktop / Downloads if directly target_canon
            if let Some(docs) = dirs::document_dir() {
                if target_canon == docs {
                    return Err(SafetyError::ProtectedPath(target_str.to_string()));
                }
            }
        }
    }

    // Verify target is inside at least one allowed root
    let mut allowed = false;
    for root in allowed_roots {
        if let Some(root_canon) = canonical_path(root) {
            if target_canon == root_canon || is_path_inside(&target_canon, &root_canon) {
                allowed = true;
                break;
            }
        }
    }

    if !allowed {
        return Err(SafetyError::OutsideAllowedScope(
            target_str.to_string(),
            allowed_roots.iter().map(|r| r.display().to_string()).collect::<Vec<_>>().join(", "),
        ));
    }

    Ok(target_canon)
}

pub fn safe_remove_file(target: &Path, allowed_roots: &[PathBuf]) -> Result<(), SafetyError> {
    let safe_target = is_safe_to_delete(target, allowed_roots)?;
    if safe_target.is_file() || safe_target.is_symlink() {
        fs::remove_file(&safe_target)?;
    }
    Ok(())
}

pub fn safe_remove_dir_all(target: &Path, allowed_roots: &[PathBuf]) -> Result<(), SafetyError> {
    let safe_target = is_safe_to_delete(target, allowed_roots)?;
    if safe_target.is_dir() {
        fs::remove_dir_all(&safe_target)?;
    }
    Ok(())
}

pub fn unique_dest_path(dest_dir: &Path, filename: &str) -> PathBuf {
    let base_path = dest_dir.join(filename);
    if !base_path.exists() {
        return base_path;
    }

    let stem = base_path.file_stem().and_then(|s| s.to_str()).unwrap_or("file");
    let ext = base_path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let dot_ext = if ext.is_empty() { String::new() } else { format!(".{}", ext) };

    let mut counter = 1;
    loop {
        let candidate = dest_dir.join(format!("{}_{}{}", stem, counter, dot_ext));
        if !candidate.exists() {
            return candidate;
        }
        counter += 1;
    }
}
