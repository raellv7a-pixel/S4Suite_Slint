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
    #[error("Erro de I/O: {0}")]
    Io(#[from] io::Error),
}

/// Normaliza lexicamente componentes `.` e `..` sem acessar o disco.
pub fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(Component::RootDir.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if let Some(Component::Normal(_)) = normalized.components().last() {
                    normalized.pop();
                } else if normalized.components().last() == Some(Component::RootDir) {
                    // Já está na raiz do sistema de arquivos, não faz nada
                } else {
                    normalized.push(Component::ParentDir.as_os_str());
                }
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    normalized
}

pub fn canonical_path(path: &Path) -> Option<PathBuf> {
    if let Ok(canon) = dunce::canonicalize(path) {
        return Some(canon);
    }

    // Se o caminho não existe, encontra o ancestral mais profundo que existe,
    // canonicaliza-o e anexa os componentes restantes normalizados lexicamente.
    let mut ancestors = Vec::new();
    let mut curr = path;
    while !curr.exists() {
        if let Some(parent) = curr.parent() {
            if let Some(file_name) = curr.file_name() {
                ancestors.push(file_name);
            }
            curr = parent;
        } else {
            break;
        }
    }

    let base_canon = if curr.exists() {
        dunce::canonicalize(curr).ok()?
    } else {
        dunce::canonicalize(".").ok()?
    };

    let mut result = base_canon;
    ancestors.reverse();
    for part in ancestors {
        result.push(part);
    }

    Some(normalize_path(&result))
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
    if allowed_roots.is_empty() {
        return Err(SafetyError::OutsideAllowedScope(
            target.display().to_string(),
            String::from("(nenhuma raiz permitida definida)"),
        ));
    }

    // Verifica se o próprio alvo é um link simbólico antes de resolver
    let is_symlink = fs::symlink_metadata(target)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false);

    let target_canon = canonical_path(target)
        .ok_or_else(|| SafetyError::InvalidPath(target.display().to_string()))?;

    // 1. Bloqueia diretórios de sistema críticos
    let target_str = target_canon.to_string_lossy();
    if target_str == "/"
        || target_str.starts_with("/usr")
        || target_str.starts_with("/bin")
        || target_str.starts_with("/sbin")
        || target_str.starts_with("/etc")
        || target_str.starts_with("/boot")
        || target_str.starts_with("/dev")
        || target_str.starts_with("/proc")
        || target_str.starts_with("/sys")
    {
        return Err(SafetyError::ProtectedPath(target_str.to_string()));
    }

    // 2. Bloqueia diretórios de usuário padrão se forem alvo direto
    if let Some(home) = dirs::home_dir().and_then(|h| canonical_path(&h)) {
        if target_canon == home {
            return Err(SafetyError::ProtectedPath(target_str.to_string()));
        }
    }
    if let Some(docs) = dirs::document_dir().and_then(|d| canonical_path(&d)) {
        if target_canon == docs {
            return Err(SafetyError::ProtectedPath(target_str.to_string()));
        }
    }
    if let Some(desktop) = dirs::desktop_dir().and_then(|d| canonical_path(&d)) {
        if target_canon == desktop {
            return Err(SafetyError::ProtectedPath(target_str.to_string()));
        }
    }
    if let Some(downloads) = dirs::download_dir().and_then(|d| canonical_path(&d)) {
        if target_canon == downloads {
            return Err(SafetyError::ProtectedPath(target_str.to_string()));
        }
    }

    // 3. A própria raiz autorizada NUNCA é um alvo válido de exclusão
    for root in allowed_roots {
        if let Some(root_canon) = canonical_path(root) {
            if target_canon == root_canon {
                return Err(SafetyError::ProtectedPath(format!(
                    "Operação bloqueada: tentativa de excluir a própria raiz permitida '{}'",
                    target_str
                )));
            }
        }
    }

    // 4. Se for symlink, verifica se a localização do symlink está estritamente dentro de uma raiz permitida
    if is_symlink {
        let symlink_parent = target
            .parent()
            .and_then(canonical_path)
            .ok_or_else(|| SafetyError::InvalidPath(target.display().to_string()))?;

        let mut symlink_inside = false;
        for root in allowed_roots {
            if let Some(root_canon) = canonical_path(root) {
                if symlink_parent == root_canon || is_path_inside(&symlink_parent, &root_canon) {
                    symlink_inside = true;
                    break;
                }
            }
        }

        if !symlink_inside {
            return Err(SafetyError::OutsideAllowedScope(
                target.display().to_string(),
                allowed_roots.iter().map(|r| r.display().to_string()).collect::<Vec<_>>().join(", "),
            ));
        }

        // Retorna o próprio caminho do symlink (normalizado) para remoção segura do link, não do alvo
        return Ok(normalize_path(target));
    }

    // 5. Verifica se o alvo normal está estritamente dentro de pelo menos uma raiz autorizada
    let mut allowed = false;
    for root in allowed_roots {
        if let Some(root_canon) = canonical_path(root) {
            if is_path_inside(&target_canon, &root_canon) {
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
    let is_symlink = fs::symlink_metadata(target)
        .or_else(|_| fs::symlink_metadata(&safe_target))
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false);
    if is_symlink {
        fs::remove_file(target)?;
    } else if safe_target.is_file() {
        fs::remove_file(&safe_target)?;
    }
    Ok(())
}

pub fn safe_remove_dir_all(target: &Path, allowed_roots: &[PathBuf]) -> Result<(), SafetyError> {
    let safe_target = is_safe_to_delete(target, allowed_roots)?;
    let is_symlink = fs::symlink_metadata(target)
        .or_else(|_| fs::symlink_metadata(&safe_target))
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false);
    if is_symlink {
        // Se for um link simbólico para diretório, remove apenas o link, nunca o destino
        fs::remove_file(target)?;
    } else if safe_target.is_dir() {
        fs::remove_dir_all(&safe_target)?;
    }
    Ok(())
}
/// Verifica se o caminho existe no sistema de arquivos, seja como arquivo regular, diretório
/// ou link simbólico (mesmo que quebrado/órfão).
pub fn path_exists_or_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

pub fn unique_dest_path(dest_dir: &Path, filename: &str) -> PathBuf {
    // Garante que o filename não contenha componentes de traversal que escapem do dest_dir
    let clean_filename = Path::new(filename)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(filename);
    let base_path = dest_dir.join(clean_filename);
    if !path_exists_or_symlink(&base_path) {
        return base_path;
    }

    let stem = base_path.file_stem().and_then(|s| s.to_str()).unwrap_or("file");
    let ext = base_path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let dot_ext = if ext.is_empty() { String::new() } else { format!(".{}", ext) };

    let mut counter = 1;
    loop {
        let candidate = dest_dir.join(format!("{}_{}{}", stem, counter, dot_ext));
        if !path_exists_or_symlink(&candidate) {
            return candidate;
        }
        counter += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn exclusao_da_raiz_autorizada_e_bloqueada() {
        let temp = tempdir().unwrap();
        let mods_dir = temp.path().join("Mods");
        fs::create_dir_all(&mods_dir).unwrap();

        // A própria raiz autorizada nunca pode ser autorizada para exclusão
        let res = is_safe_to_delete(&mods_dir, &[mods_dir.clone()]);
        assert!(res.is_err(), "deve bloquear exclusão da raiz");
        match res.unwrap_err() {
            SafetyError::ProtectedPath(msg) => {
                assert!(msg.contains("própria raiz permitida"));
            }
            other => panic!("esperava ProtectedPath, veio {:?}", other),
        }

        // safe_remove_dir_all deve recusar apagar a raiz autorizada
        let remove_res = safe_remove_dir_all(&mods_dir, &[mods_dir.clone()]);
        assert!(remove_res.is_err());
        assert!(mods_dir.exists(), "raiz autorizada deve permanecer intacta");
    }

    #[test]
    fn caminhos_externos_sao_bloqueados() {
        let temp = tempdir().unwrap();
        let mods_dir = temp.path().join("Mods");
        let external_dir = temp.path().join("External");
        fs::create_dir_all(&mods_dir).unwrap();
        fs::create_dir_all(&external_dir).unwrap();

        let ext_file = external_dir.join("segredo.txt");
        fs::write(&ext_file, b"conteudo_externo").unwrap();

        let res = is_safe_to_delete(&ext_file, &[mods_dir.clone()]);
        assert!(res.is_err(), "arquivo fora de allowed_roots deve ser recusado");
        assert!(matches!(res.unwrap_err(), SafetyError::OutsideAllowedScope(_, _)));

        let del_res = safe_remove_file(&ext_file, &[mods_dir.clone()]);
        assert!(del_res.is_err());
        assert!(ext_file.exists(), "arquivo externo não pode ser apagado");
    }

    #[test]
    fn path_traversal_com_parent_dir_e_bloqueado() {
        let temp = tempdir().unwrap();
        let mods_dir = temp.path().join("Mods");
        let sub_dir = mods_dir.join("Sub");
        fs::create_dir_all(&sub_dir).unwrap();

        // Tenta escapar de mods_dir usando ..
        let traversal_target = sub_dir.join("../../../etc/shadow");
        assert!(!is_path_inside(&traversal_target, &mods_dir));

        let res = is_safe_to_delete(&traversal_target, &[mods_dir.clone()]);
        assert!(res.is_err(), "traversal com .. deve ser bloqueado");
    }

    #[test]
    fn symlink_para_arquivo_externo_remove_apenas_o_link_e_preserva_o_alvo() {
        let temp = tempdir().unwrap();
        let mods_dir = temp.path().join("Mods");
        let secret_dir = temp.path().join("Secret");
        fs::create_dir_all(&mods_dir).unwrap();
        fs::create_dir_all(&secret_dir).unwrap();

        let target_file = secret_dir.join("documento_confidencial.txt");
        fs::write(&target_file, b"DADOS_CONFIDENCIAIS_INVIOLAVEIS").unwrap();

        let link_path = mods_dir.join("link_malicioso.txt");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target_file, &link_path).unwrap();

        #[cfg(unix)]
        {
            assert!(link_path.exists());
            // Remoção segura do symlink
            let res = safe_remove_file(&link_path, &[mods_dir.clone()]);
            assert!(res.is_ok(), "deve permitir remover o symlink dentro de Mods: {:?}", res.err());

            // O link em Mods foi apagado
            assert!(fs::symlink_metadata(&link_path).is_err(), "o symlink deveria ter sido removido");

            // O arquivo de destino externo DEVE continuar existindo e intacto!
            assert!(target_file.exists(), "o arquivo de destino externo NUNCA deve ser apagado através de symlink");
            assert_eq!(fs::read(&target_file).unwrap(), b"DADOS_CONFIDENCIAIS_INVIOLAVEIS");
        }
    }

    #[test]
    fn symlink_para_diretorio_externo_remove_apenas_o_link_e_preserva_diretorio() {
        let temp = tempdir().unwrap();
        let mods_dir = temp.path().join("Mods");
        let secret_dir = temp.path().join("SecretDir");
        let secret_file = secret_dir.join("arquivo.txt");
        fs::create_dir_all(&mods_dir).unwrap();
        fs::create_dir_all(&secret_dir).unwrap();
        fs::write(&secret_file, b"nao_pode_ser_deletado").unwrap();

        let link_dir = mods_dir.join("link_para_pasta");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&secret_dir, &link_dir).unwrap();

        #[cfg(unix)]
        {
            // safe_remove_dir_all em link para pasta deve remover apenas o link, nunca o diretório externo
            let res = safe_remove_dir_all(&link_dir, &[mods_dir.clone()]);
            assert!(res.is_ok(), "deve permitir remover o link para pasta");

            assert!(fs::symlink_metadata(&link_dir).is_err(), "link deve ter sido removido");
            assert!(secret_dir.exists(), "diretório externo deve permanecer intacto");
            assert!(secret_file.exists(), "conteúdo do diretório externo deve permanecer intacto");
        }
    }

    #[test]
    fn operacoes_legitimas_dentro_do_escopo_funcionam() {
        let temp = tempdir().unwrap();
        let mods_dir = temp.path().join("Mods");
        let sub_dir = mods_dir.join("MeusCabelos");
        fs::create_dir_all(&sub_dir).unwrap();

        let package_file = sub_dir.join("cabelo.package");
        fs::write(&package_file, b"DBPF_VALID_CONTENT").unwrap();

        assert!(is_path_inside(&package_file, &mods_dir));
        assert!(is_path_inside(&sub_dir, &mods_dir));

        // Exclusão legítima de arquivo
        let res_file = safe_remove_file(&package_file, &[mods_dir.clone()]);
        assert!(res_file.is_ok());
        assert!(!package_file.exists());

        // Exclusão legítima de subpasta
        let res_dir = safe_remove_dir_all(&sub_dir, &[mods_dir.clone()]);
        assert!(res_dir.is_ok());
        assert!(!sub_dir.exists());

        // Raiz de mods continua existindo
        assert!(mods_dir.exists());
    }

    #[test]
    fn unique_dest_path_bloqueia_path_traversal() {
        let temp = tempdir().unwrap();
        let dest_dir = temp.path().join("Dest");
        fs::create_dir_all(&dest_dir).unwrap();

        let result = unique_dest_path(&dest_dir, "../../escapar.package");
        assert_eq!(result, dest_dir.join("escapar.package"));
        assert!(is_path_inside(&result, &dest_dir));
    }
}
