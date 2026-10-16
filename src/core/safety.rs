use std::ffi::OsStr;
use std::fs::{self, File};
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
                if let Some(Component::Normal(_)) = normalized.components().next_back() {
                    normalized.pop();
                } else if normalized.components().next_back() == Some(Component::RootDir) {
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
                allowed_roots
                    .iter()
                    .map(|r| r.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
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
    is_safe_to_delete(target, allowed_roots)?;
    let (parent, name) = anchored_scoped_parent(target, allowed_roots)?;
    // unlinkat never follows the final symlink and reports wrong types/errors.
    parent.remove_file(&name)?;
    Ok(())
}

pub fn safe_remove_dir_all(target: &Path, allowed_roots: &[PathBuf]) -> Result<(), SafetyError> {
    is_safe_to_delete(target, allowed_roots)?;
    let (parent, name) = anchored_scoped_parent(target, allowed_roots)?;
    parent.remove_tree(&name)?;
    Ok(())
}

fn anchored_scoped_parent(
    target: &Path,
    allowed_roots: &[PathBuf],
) -> Result<(AnchoredDir, std::ffi::OsString), SafetyError> {
    let absolute = absolute_without_parent(target)?;
    let inside = allowed_roots.iter().any(|root| {
        absolute_without_parent(root)
            .map(|root| absolute != root && absolute.starts_with(root))
            .unwrap_or(false)
    });
    if !inside {
        return Err(SafetyError::OutsideAllowedScope(
            target.display().to_string(),
            "raiz lexical autorizada".to_string(),
        ));
    }
    let name = absolute
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Caminho sem nome final"))?
        .to_os_string();
    Ok((AnchoredDir::open(absolute.parent().unwrap())?, name))
}

fn absolute_without_parent(path: &Path) -> io::Result<PathBuf> {
    if path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Componente '..' bloqueado",
        ));
    }
    Ok(if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    })
}

/// Diretório mantido aberto; no Linux cada ancestral é aberto com NOFOLLOW.
/// Os métodos aceitam apenas um nome, nunca caminhos relativos compostos.
pub(crate) struct AnchoredDir {
    file: File,
}

impl AnchoredDir {
    pub(crate) fn open(path: &Path) -> io::Result<Self> {
        Self::open_path(path, false)
    }

    pub(crate) fn create(path: &Path) -> io::Result<Self> {
        Self::open_path(path, true)
    }

    pub(crate) fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            file: self.file.try_clone()?,
        })
    }

    pub(crate) fn sync_all(&self) -> io::Result<()> {
        self.file.sync_all()
    }

    fn open_path(path: &Path, create: bool) -> io::Result<Self> {
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::{AsRawFd, FromRawFd};
            let absolute = absolute_without_parent(path)?;
            let mut current = Self {
                file: File::open("/")?,
            };
            for component in absolute.components() {
                if let Component::Normal(name) = component {
                    let name = single_name(name)?;
                    if create {
                        let result = unsafe {
                            libc::mkdirat(current.file.as_raw_fd(), name.as_ptr(), 0o755)
                        };
                        if result != 0 {
                            let error = io::Error::last_os_error();
                            if error.kind() != io::ErrorKind::AlreadyExists {
                                return Err(error);
                            }
                        }
                    }
                    let fd = unsafe {
                        libc::openat(
                            current.file.as_raw_fd(),
                            name.as_ptr(),
                            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                        )
                    };
                    if fd < 0 {
                        return Err(anchor_error());
                    }
                    current = Self {
                        file: unsafe { File::from_raw_fd(fd) },
                    };
                }
            }
            Ok(current)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (path, create);
            Err(unsupported_anchoring())
        }
    }

    pub(crate) fn create_child(&self, name: impl AsRef<OsStr>) -> io::Result<Self> {
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd;
            let encoded = single_name(name.as_ref())?;
            if unsafe { libc::mkdirat(self.file.as_raw_fd(), encoded.as_ptr(), 0o755) } != 0 {
                let error = io::Error::last_os_error();
                if error.kind() != io::ErrorKind::AlreadyExists {
                    return Err(error);
                }
            }
            self.child(name)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = name;
            Err(unsupported_anchoring())
        }
    }

    pub(crate) fn child(&self, name: impl AsRef<OsStr>) -> io::Result<Self> {
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::{AsRawFd, FromRawFd};
            let name = single_name(name.as_ref())?;
            let fd = unsafe {
                libc::openat(
                    self.file.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if fd < 0 {
                return Err(anchor_error());
            }
            Ok(Self {
                file: unsafe { File::from_raw_fd(fd) },
            })
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = name;
            Err(unsupported_anchoring())
        }
    }

    /// Apenas para APIs que precisam de um caminho; mantenha este handle vivo.
    pub(crate) fn path(&self) -> PathBuf {
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd;
            PathBuf::from(format!("/proc/self/fd/{}", self.file.as_raw_fd()))
        }
        #[cfg(not(target_os = "linux"))]
        {
            unreachable!("AnchoredDir não é construído sem suporte a anchoring")
        }
    }

    /// Caminho para ferramentas filhas: `self` no /proc pertenceria ao filho,
    /// cujos descritores CLOEXEC já estariam fechados. O handle pai fica vivo.
    pub(crate) fn child_process_path(&self) -> PathBuf {
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd;
            PathBuf::from(format!(
                "/proc/{}/fd/{}",
                std::process::id(),
                self.file.as_raw_fd()
            ))
        }
        #[cfg(not(target_os = "linux"))]
        {
            unreachable!("AnchoredDir não é construído sem suporte a anchoring")
        }
    }

    pub(crate) fn open_regular(&self, name: impl AsRef<OsStr>) -> io::Result<File> {
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::{AsRawFd, FromRawFd};
            let name = single_name(name.as_ref())?;
            let fd = unsafe {
                libc::openat(
                    self.file.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
                )
            };
            if fd < 0 {
                return Err(anchor_error());
            }
            let file = unsafe { File::from_raw_fd(fd) };
            if !file.metadata()?.is_file() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Fonte não é arquivo regular",
                ));
            }
            Ok(file)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = name;
            Err(unsupported_anchoring())
        }
    }

    pub(crate) fn create_new(&self, name: impl AsRef<OsStr>) -> io::Result<File> {
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::{AsRawFd, FromRawFd};
            let name = single_name(name.as_ref())?;
            let fd = unsafe {
                libc::openat(
                    self.file.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_WRONLY
                        | libc::O_CREAT
                        | libc::O_EXCL
                        | libc::O_NOFOLLOW
                        | libc::O_CLOEXEC,
                    0o600,
                )
            };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(unsafe { File::from_raw_fd(fd) })
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = name;
            Err(unsupported_anchoring())
        }
    }

    /// Publica arquivo ou diretório sem substituir nenhuma entrada existente.
    /// ENOSYS/EINVAL/filesystems sem RENAME_NOREPLACE falham fechados.
    pub(crate) fn publish_noreplace(
        &self,
        src_name: impl AsRef<OsStr>,
        dest: &Self,
        dest_name: impl AsRef<OsStr>,
    ) -> io::Result<()> {
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd;
            let src = single_name(src_name.as_ref())?;
            let dest_name = single_name(dest_name.as_ref())?;
            let result = unsafe {
                libc::renameat2(
                    self.file.as_raw_fd(),
                    src.as_ptr(),
                    dest.file.as_raw_fd(),
                    dest_name.as_ptr(),
                    libc::RENAME_NOREPLACE,
                )
            };
            if result != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (src_name, dest, dest_name);
            Err(unsupported_anchoring())
        }
    }

    pub(crate) fn remove_file(&self, name: impl AsRef<OsStr>) -> io::Result<()> {
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd;
            let name = single_name(name.as_ref())?;
            if unsafe { libc::unlinkat(self.file.as_raw_fd(), name.as_ptr(), 0) } != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = name;
            Err(unsupported_anchoring())
        }
    }

    pub(crate) fn remove_tree(&self, name: impl AsRef<OsStr>) -> io::Result<()> {
        let name = name.as_ref();
        validate_single_name(name)?;
        let path = self.path().join(name);
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            self.remove_file(name)
        } else if metadata.is_dir() {
            // std's Unix implementation walks the tree with descriptor-relative
            // operations and does not follow symlinks encountered during removal.
            fs::remove_dir_all(path)
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Alvo não é diretório",
            ))
        }
    }

    /// Serializa operações da aplicação no mesmo diretório (não é lock de
    /// segurança contra processos externos que ignoram flock).
    pub(crate) fn lock_exclusive(&self) -> io::Result<DirectoryLock> {
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd;
            // Open a separate description so clones cannot accidentally share
            // ownership of the same flock and unlock another operation.
            let file = File::open(self.path())?;
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(DirectoryLock(file))
        }
        #[cfg(not(target_os = "linux"))]
        {
            Err(unsupported_anchoring())
        }
    }
}

pub(crate) struct DirectoryLock(File);

impl Drop for DirectoryLock {
    fn drop(&mut self) {
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd;
            unsafe {
                libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
            }
        }
    }
}

fn validate_single_name(name: &OsStr) -> io::Result<()> {
    let mut components = Path::new(name).components();
    if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Esperado um único nome de arquivo",
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn single_name(name: &OsStr) -> io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;
    validate_single_name(name)?;
    std::ffi::CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Nome contém NUL"))
}

#[cfg(target_os = "linux")]
fn anchor_error() -> io::Error {
    let error = io::Error::last_os_error();
    if matches!(
        error.raw_os_error(),
        Some(libc::ELOOP) | Some(libc::ENOTDIR)
    ) {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("Ancestral/link ou tipo inseguro: {error}"),
        )
    } else {
        error
    }
}

#[cfg(not(target_os = "linux"))]
fn unsupported_anchoring() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "Operação requer anchoring e publicação no-replace seguros",
    )
}

#[allow(dead_code)]
/// Move sem substituir; não usa check-then-rename nem fallback com overwrite.
pub(crate) fn publish_noreplace(src: &Path, dest: &Path) -> io::Result<()> {
    let src_parent = src
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Fonte sem pai"))?;
    let dest_parent = dest
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Destino sem pai"))?;
    let src_name = src
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Fonte sem nome"))?;
    let dest_name = dest
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Destino sem nome"))?;
    AnchoredDir::open(src_parent)?.publish_noreplace(
        src_name,
        &AnchoredDir::open(dest_parent)?,
        dest_name,
    )
}

/// Leases Linux bloqueiam escritores já abertos e detectam solicitações de
/// escrita durante a captura. Sem suporte, operações destrutivas falham fechadas.
pub(crate) struct ReadLease {
    pub(crate) file: File,
}

impl ReadLease {
    #[cfg(target_os = "linux")]
    pub(crate) fn acquire(file: File) -> io::Result<Self> {
        use std::os::fd::AsRawFd;
        // F_SETSIG é por open-file-description. SIGURG é ignorado por padrão,
        // diferentemente de SIGIO, que poderia terminar a aplicação.
        const F_SETSIG: libc::c_int = 10;
        let fd = file.as_raw_fd();
        unsafe {
            if libc::fcntl(fd, libc::F_SETOWN, libc::getpid()) == -1
                || libc::fcntl(fd, F_SETSIG, libc::SIGURG) == -1
                || libc::fcntl(fd, libc::F_SETLEASE, libc::F_RDLCK) == -1
            {
                let error = io::Error::last_os_error();
                return Err(io::Error::new(error.kind(), format!(
                    "Não foi possível garantir estabilidade por lease Linux; arquivo preservado (escritor aberto ou filesystem sem suporte): {error}")));
            }
        }
        Ok(Self { file })
    }

    #[cfg(not(target_os = "linux"))]
    pub(crate) fn acquire(_file: File) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Exclusão segura exige file leases Linux",
        ))
    }

    pub(crate) fn check(&self) -> io::Result<()> {
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd;
            if unsafe { libc::fcntl(self.file.as_raw_fd(), libc::F_GETLEASE) } == libc::F_RDLCK {
                return Ok(());
            }
        }
        Err(io::Error::other(
            "Escritor concorrente ou lease revogada; originais preservados",
        ))
    }
}

impl Drop for ReadLease {
    fn drop(&mut self) {
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd;
            unsafe {
                libc::fcntl(self.file.as_raw_fd(), libc::F_SETLEASE, libc::F_UNLCK);
            }
        }
    }
}

pub(crate) fn same_file_identity(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        a.dev() == b.dev() && a.ino() == b.ino()
    }
    #[cfg(not(unix))]
    {
        let _ = (a, b);
        false
    }
}

/// Verifica se o caminho existe no sistema de arquivos, seja como arquivo regular, diretório
/// ou link simbólico (mesmo que quebrado/órfão).
pub fn path_exists_or_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

/// Verifica se qualquer diretório pai/ancestral de `path` é um link simbólico.
pub fn has_symlink_ancestor(path: &Path) -> bool {
    let mut current = path.parent();
    while let Some(parent) = current {
        if let Ok(meta) = fs::symlink_metadata(parent) {
            if meta.file_type().is_symlink() {
                return true;
            }
        }
        current = parent.parent();
    }
    false
}

pub fn unique_dest_path(dest_dir: &Path, filename: &str) -> PathBuf {
    // Garante que o filename não contenha componentes de traversal que escapem do dest_dir
    let clean_filename = Path::new(filename)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("file");
    let base_path = dest_dir.join(clean_filename);
    if !path_exists_or_symlink(&base_path) {
        return base_path;
    }

    let stem = base_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("file");
    let ext = base_path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let dot_ext = if ext.is_empty() {
        String::new()
    } else {
        format!(".{}", ext)
    };

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
        assert!(
            res.is_err(),
            "arquivo fora de allowed_roots deve ser recusado"
        );
        assert!(matches!(
            res.unwrap_err(),
            SafetyError::OutsideAllowedScope(_, _)
        ));

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
            assert!(
                res.is_ok(),
                "deve permitir remover o symlink dentro de Mods: {:?}",
                res.err()
            );

            // O link em Mods foi apagado
            assert!(
                fs::symlink_metadata(&link_path).is_err(),
                "o symlink deveria ter sido removido"
            );

            // O arquivo de destino externo DEVE continuar existindo e intacto!
            assert!(
                target_file.exists(),
                "o arquivo de destino externo NUNCA deve ser apagado através de symlink"
            );
            assert_eq!(
                fs::read(&target_file).unwrap(),
                b"DADOS_CONFIDENCIAIS_INVIOLAVEIS"
            );
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

            assert!(
                fs::symlink_metadata(&link_dir).is_err(),
                "link deve ter sido removido"
            );
            assert!(
                secret_dir.exists(),
                "diretório externo deve permanecer intacto"
            );
            assert!(
                secret_file.exists(),
                "conteúdo do diretório externo deve permanecer intacto"
            );
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

    #[test]
    fn detector_de_symlink_ancestral_identifica_link_em_qualquer_nivel() {
        let temp = tempdir().unwrap();
        let base = temp.path().join("Base");
        let alvo = temp.path().join("Alvo");
        fs::create_dir_all(&base).unwrap();
        fs::create_dir_all(&alvo).unwrap();

        let link = base.join("LinkFolder");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&alvo, &link).unwrap();

            let arquivo_no_link = link.join("sub/arquivo.txt");
            assert!(
                has_symlink_ancestor(&arquivo_no_link),
                "deve acusar symlink no ancestral LinkFolder"
            );

            let arquivo_normal = base.join("normal/arquivo.txt");
            assert!(
                !has_symlink_ancestor(&arquivo_normal),
                "pasta normal não deve acusar symlink"
            );
        }
    }
    #[test]
    fn p0_remove_file_informa_tipo_incorreto() {
        let root = tempdir().unwrap();
        let folder = root.path().join("folder");
        fs::create_dir(&folder).unwrap();
        fs::write(folder.join("sentinel"), b"preserved bytes").unwrap();
        let result = safe_remove_file(&folder, &[root.path().to_path_buf()]);
        assert!(
            result.is_err(),
            "diretório tratado como arquivo removido: {result:?}"
        );
        assert_eq!(
            fs::read(folder.join("sentinel")).unwrap(),
            b"preserved bytes"
        );
    }

    #[cfg(unix)]
    #[test]
    fn p0_remove_file_recusa_ancestral_symlink_mesmo_dentro_do_escopo() {
        let root = tempdir().unwrap();
        let folder = root.path().join("folder");
        fs::create_dir(&folder).unwrap();
        let sentinel = folder.join("sentinel");
        fs::write(&sentinel, b"preserved bytes").unwrap();
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&folder, &link).unwrap();
        let result = safe_remove_file(&link.join("sentinel"), &[root.path().to_path_buf()]);
        let actual = fs::read(&sentinel);
        assert!(
            result.is_err(),
            "remoção por symlink aceita: {result:?}; bytes={actual:?}"
        );
        assert_eq!(actual.unwrap(), b"preserved bytes");
    }
    #[test]
    fn p0_unique_dest_path_mantem_nomes_especiais_dentro_da_pasta() {
        let root = tempdir().unwrap();
        let dest = root.path().join("dest");
        // O diretório ainda não existe: nenhum exists() pode ocultar traversal
        // fazendo o helper cair no ramo de sufixo para colisões.
        for name in ["/", "..", ".", ""] {
            let candidate = unique_dest_path(&dest, name);
            let normalized = normalize_path(&candidate);
            assert!(
                normalized != dest && normalized.starts_with(&dest),
                "nome {name:?} escapou: {} -> {}",
                candidate.display(),
                normalized.display()
            );
            assert!(
                matches!(candidate.components().last(), Some(Component::Normal(_))),
                "nome final inválido: {}",
                candidate.display()
            );
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn p0_anchor_mantem_publicacao_no_diretorio_capturado() {
        let root = tempdir().unwrap();
        let live = root.path().join("live");
        let moved = root.path().join("moved");
        let external = root.path().join("external");
        fs::create_dir(&live).unwrap();
        fs::create_dir(&external).unwrap();
        let anchor = AnchoredDir::open(&live).unwrap();
        let workspace = tempfile::tempdir_in(anchor.path()).unwrap();
        let private = anchor.child(workspace.path().file_name().unwrap()).unwrap();
        fs::write(private.path().join("payload"), b"complete bytes").unwrap();
        fs::rename(&live, &moved).unwrap();
        std::os::unix::fs::symlink(&external, &live).unwrap();
        private
            .publish_noreplace("payload", &anchor, "result")
            .unwrap();
        workspace.close().unwrap();
        assert_eq!(fs::read(moved.join("result")).unwrap(), b"complete bytes");
        assert!(
            !external.join("result").exists(),
            "publicação escapou da âncora"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn p0_publish_noreplace_preserva_arquivo_e_diretorio_ocupados() {
        let root = tempdir().unwrap();
        let source = root.path().join("source");
        let destination = root.path().join("destination");
        fs::write(&source, b"new bytes").unwrap();
        fs::write(&destination, b"original bytes").unwrap();
        let result = publish_noreplace(&source, &destination);
        assert!(result.is_err(), "sobrescrita aceita: {result:?}");
        assert_eq!(fs::read(&source).unwrap(), b"new bytes");
        assert_eq!(fs::read(&destination).unwrap(), b"original bytes");
        let source_dir = root.path().join("source-dir");
        let dest_dir = root.path().join("dest-dir");
        fs::create_dir(&source_dir).unwrap();
        fs::create_dir(&dest_dir).unwrap();
        fs::write(dest_dir.join("sentinel"), b"directory bytes").unwrap();
        assert!(publish_noreplace(&source_dir, &dest_dir).is_err());
        assert_eq!(
            fs::read(dest_dir.join("sentinel")).unwrap(),
            b"directory bytes"
        );
        assert!(source_dir.is_dir());
    }
}
