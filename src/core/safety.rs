use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum SafetyError {
    #[error("Caminho não encontrado ou inválido: {0}")]
    InvalidPath(String),
    #[error("Operação bloqueada por segurança: tentativa de excluir caminho protegido '{0}'")]
    ProtectedPath(String),
    #[error("Operação bloqueada: o caminho '{0}' está fora do diretório permitido '{1}'")]
    OutsideAllowedScope(String, String),
    #[error("Operação bloqueada: o alvo mudou desde a confirmação: {0}")]
    ChangedTarget(String),
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

/// Resolve somente caminhos existentes, sem seguir links no alvo ou ancestrais.
///
/// A raiz configurada também precisa ser real: aliases por symlink não concedem
/// autorização. `canonical_path` continua disponível para operações não
/// destrutivas, mas seu fallback nunca é usado para autorizar uma exclusão.
fn real_path(path: &Path) -> Result<PathBuf, SafetyError> {
    if path.as_os_str().is_empty() {
        return Err(SafetyError::InvalidPath(String::new()));
    }
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut checked = PathBuf::new();
    let mut components = absolute.components().peekable();
    while let Some(component) = components.next() {
        if component == Component::CurDir {
            continue;
        }
        checked.push(component.as_os_str());
        let metadata = fs::symlink_metadata(&checked)?;
        let is_link = metadata.file_type().is_symlink();
        #[cfg(windows)]
        let is_link = {
            use std::os::windows::fs::MetadataExt;
            // Junctions e outros reparse points também podem redirecionar I/O.
            is_link || metadata.file_attributes() & 0x400 != 0
        };
        if is_link {
            return Err(SafetyError::ProtectedPath(checked.display().to_string()));
        }
        if components.peek().is_some() && !metadata.is_dir() {
            return Err(SafetyError::InvalidPath(checked.display().to_string()));
        }
        if !metadata.is_dir() && !metadata.is_file() {
            return Err(SafetyError::InvalidPath(checked.display().to_string()));
        }
    }
    Ok(dunce::canonicalize(&absolute)?)
}

fn is_protected_path(path: &Path) -> bool {
    if path.parent().is_none() {
        return true;
    }
    #[cfg(unix)]
    {
        for system in [
            "/usr", "/bin", "/sbin", "/etc", "/boot", "/dev", "/proc", "/sys", "/lib", "/lib64",
        ] {
            if path.starts_with(system) {
                return true;
            }
        }
        if path == Path::new("/home") {
            return true;
        }
    }
    #[cfg(windows)]
    {
        for variable in [
            "SystemRoot",
            "ProgramFiles",
            "ProgramFiles(x86)",
            "ProgramData",
        ] {
            if let Some(system) = std::env::var_os(variable) {
                if let Ok(system) = dunce::canonicalize(PathBuf::from(system)) {
                    if path.starts_with(system) {
                        return true;
                    }
                }
            }
        }
    }
    for personal in [
        dirs::home_dir(),
        dirs::document_dir(),
        dirs::desktop_dir(),
        dirs::download_dir(),
    ]
    .into_iter()
    .flatten()
    {
        if let Ok(personal) = dunce::canonicalize(personal) {
            if path == personal {
                return true;
            }
        }
    }
    false
}

/// Autoriza apenas descendentes reais de diretórios existentes permitidos.
///
/// Todas as raízes permanecem protegidas, inclusive quando se sobrepõem.
/// Caminhos com `..` são resolvidos estritamente antes da comparação por
/// componentes; nenhum caminho inexistente ou alias por link recebe autorização.
pub fn is_safe_to_delete(target: &Path, allowed_roots: &[PathBuf]) -> Result<PathBuf, SafetyError> {
    let target_canon = real_path(target)?;
    if is_protected_path(&target_canon) {
        return Err(SafetyError::ProtectedPath(
            target_canon.display().to_string(),
        ));
    }

    let mut allowed = false;
    for root in allowed_roots {
        let root_canon = real_path(root)?;
        if !fs::symlink_metadata(&root_canon)?.is_dir() {
            return Err(SafetyError::InvalidPath(root.display().to_string()));
        }
        // Também impede excluir uma pasta que contenha outra raiz autorizada.
        if root_canon.starts_with(&target_canon) {
            return Err(SafetyError::ProtectedPath(
                target_canon.display().to_string(),
            ));
        }
        if target_canon.starts_with(&root_canon) {
            allowed = true;
        }
    }
    if !allowed {
        return Err(SafetyError::OutsideAllowedScope(
            target_canon.display().to_string(),
            allowed_roots
                .iter()
                .map(|r| r.display().to_string())
                .collect::<Vec<_>>()
                .join(", "),
        ));
    }
    Ok(target_canon)
}

pub fn safe_remove_file(target: &Path, allowed_roots: &[PathBuf]) -> Result<(), SafetyError> {
    let safe_target = is_safe_to_delete(target, allowed_roots)?;
    if !fs::symlink_metadata(&safe_target)?.is_file() {
        return Err(SafetyError::InvalidPath(target.display().to_string()));
    }
    fs::remove_file(&safe_target)?;
    Ok(())
}

/// Remove uma pasta real; links contidos nela são desvinculados, não seguidos.
pub fn safe_remove_dir_all(target: &Path, allowed_roots: &[PathBuf]) -> Result<(), SafetyError> {
    let safe_target = is_safe_to_delete(target, allowed_roots)?;
    if !fs::symlink_metadata(&safe_target)?.is_dir() {
        return Err(SafetyError::InvalidPath(target.display().to_string()));
    }
    fs::remove_dir_all(&safe_target)?;
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
