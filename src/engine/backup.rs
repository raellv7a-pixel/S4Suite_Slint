//! Backup ZIP transacional: só publica um arquivo concluído e relido com CRC.
//!
//! A API recusa links, reparse points, tipos especiais e caminhos com `..`.
//! Metadados/identidade são comparados antes e depois de cada cópia e a árvore
//! inteira é conferida novamente antes da publicação. Isso detecta alterações
//! observáveis, mas **não** é um snapshot atômico: as APIs portáveis de `std`
//! não eliminam TOCTOU contra um processo hostil trocando caminhos durante I/O.
//! Não execute sobre uma árvore sendo editada; para isolamento forte é preciso
//! um snapshot do filesystem ou acesso relativo a descritores com no-follow.

use std::fs::{self, File, Metadata};
use std::io::{self, Seek, Write};
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;
use tempfile::NamedTempFile;
use walkdir::WalkDir;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackupReport {
    pub files: usize,
    /// Bytes de conteúdo, antes da compressão; diretórios não contam.
    pub bytes: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    #[error("Caminho de backup inválido: {0}")]
    InvalidPath(PathBuf),
    #[error("Links, reparse points e tipos especiais não são permitidos: {0}")]
    UnsafeEntry(PathBuf),
    #[error("O destino do backup já existe e foi preservado: {0}")]
    DestinationExists(PathBuf),
    #[error("O destino do backup não pode ficar dentro da origem: {0}")]
    DestinationInsideSource(PathBuf),
    #[error("A origem mudou durante o backup; nenhum ZIP foi publicado: {0}")]
    SourceChanged(PathBuf),
    #[error("Falha de I/O no backup em {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("Falha ao percorrer a origem do backup: {0}")]
    Walk(#[from] walkdir::Error),
    #[error("Falha ao escrever ou validar o ZIP: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("A verificação do ZIP concluído falhou: {0}")]
    Verification(String),
}

fn io_error(path: &Path, source: io::Error) -> BackupError {
    BackupError::Io {
        path: path.to_path_buf(),
        source,
    }
}

fn is_link(metadata: &Metadata) -> bool {
    let link = metadata.file_type().is_symlink();
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        link || metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        link
    }
}

// Unlike delete authorization, this validator permits a real filesystem root
// as output parent; it never grants deletion rights. Component-wise checks use
// the same no-link/reparse policy as core::safety, with stricter `..` rejection.
fn real_path(path: &Path) -> Result<PathBuf, BackupError> {
    if path.as_os_str().is_empty() || path.components().any(|c| c == Component::ParentDir) {
        return Err(BackupError::InvalidPath(path.to_path_buf()));
    }
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| io_error(path, e))?
            .join(path)
    };
    let mut checked = PathBuf::new();
    let mut components = absolute.components().peekable();
    while let Some(component) = components.next() {
        if component == Component::CurDir {
            continue;
        }
        checked.push(component.as_os_str());
        let metadata = fs::symlink_metadata(&checked).map_err(|e| io_error(&checked, e))?;
        if is_link(&metadata) || (!metadata.is_dir() && !metadata.is_file()) {
            return Err(BackupError::UnsafeEntry(checked));
        }
        if components.peek().is_some() && !metadata.is_dir() {
            return Err(BackupError::InvalidPath(checked));
        }
    }
    dunce::canonicalize(&absolute).map_err(|e| io_error(path, e))
}

fn real_directory(path: &Path) -> Result<PathBuf, BackupError> {
    let real = real_path(path)?;
    if !fs::symlink_metadata(&real)
        .map_err(|e| io_error(&real, e))?
        .is_dir()
    {
        return Err(BackupError::InvalidPath(path.to_path_buf()));
    }
    Ok(real)
}

#[derive(Debug, PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: SystemTime,
    directory: bool,
    readonly: bool,
    #[cfg(unix)]
    identity: (u64, u64),
    #[cfg(unix)]
    change_time: (i64, i64),
    #[cfg(windows)]
    identity: (u64, u32),
}

impl Stamp {
    fn new(path: &Path, metadata: &Metadata) -> Result<Self, BackupError> {
        if is_link(metadata) || (!metadata.is_file() && !metadata.is_dir()) {
            return Err(BackupError::UnsafeEntry(path.to_path_buf()));
        }
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        #[cfg(windows)]
        use std::os::windows::fs::MetadataExt;
        Ok(Self {
            len: metadata.len(),
            modified: metadata.modified().map_err(|e| io_error(path, e))?,
            directory: metadata.is_dir(),
            readonly: metadata.permissions().readonly(),
            #[cfg(unix)]
            identity: (metadata.dev(), metadata.ino()),
            #[cfg(unix)]
            change_time: (metadata.ctime(), metadata.ctime_nsec()),
            #[cfg(windows)]
            identity: (metadata.creation_time(), metadata.file_attributes()),
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Entry {
    path: PathBuf,
    name: String,
    stamp: Stamp,
}

fn snapshot(source: &Path) -> Result<Vec<Entry>, BackupError> {
    let mut entries = Vec::new();
    for entry in WalkDir::new(source)
        .follow_links(false)
        .follow_root_links(false)
    {
        let entry = entry?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(path).map_err(|e| io_error(path, e))?;
        let stamp = Stamp::new(path, &metadata)?;
        let relative = path
            .strip_prefix(source)
            .map_err(|_| BackupError::InvalidPath(path.to_path_buf()))?;
        let mut name = String::new();
        for component in relative.components() {
            let Component::Normal(part) = component else {
                return Err(BackupError::InvalidPath(path.to_path_buf()));
            };
            let part = part
                .to_str()
                .ok_or_else(|| BackupError::InvalidPath(path.to_path_buf()))?;
            // ZIP uses slash separators on every platform. Reject aliases that
            // another extractor could interpret as separators or drive names.
            if part.contains(['\\', ':']) {
                return Err(BackupError::InvalidPath(path.to_path_buf()));
            }
            if !name.is_empty() {
                name.push('/');
            }
            name.push_str(part);
        }
        if stamp.directory && !name.is_empty() {
            name.push('/');
        }
        entries.push(Entry {
            path: path.to_path_buf(),
            name,
            stamp,
        });
    }
    entries.sort_unstable_by(|a, b| a.name.cmp(&b.name));
    Ok(entries)
}

fn check_stamp(entry: &Entry, metadata: Metadata) -> Result<(), BackupError> {
    if Stamp::new(&entry.path, &metadata)? != entry.stamp {
        return Err(BackupError::SourceChanged(entry.path.clone()));
    }
    Ok(())
}

fn write_archive<W, C>(
    writer: W,
    entries: &[Entry],
    mut copy: C,
) -> Result<BackupReport, BackupError>
where
    W: Write + Seek,
    C: FnMut(&mut File, &mut ZipWriter<W>) -> io::Result<u64>,
{
    let mut archive = ZipWriter::new(writer);
    let mut report = BackupReport { files: 0, bytes: 0 };
    for entry in entries.iter().filter(|e| !e.name.is_empty()) {
        if real_path(&entry.path)? != entry.path {
            return Err(BackupError::SourceChanged(entry.path.clone()));
        }
        check_stamp(
            entry,
            fs::symlink_metadata(&entry.path).map_err(|e| io_error(&entry.path, e))?,
        )?;
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        if entry.stamp.directory {
            archive.add_directory(&entry.name, options)?;
            continue;
        }
        let mut file = File::open(&entry.path).map_err(|e| io_error(&entry.path, e))?;
        check_stamp(
            entry,
            file.metadata().map_err(|e| io_error(&entry.path, e))?,
        )?;
        archive.start_file(
            &entry.name,
            options.large_file(entry.stamp.len > u32::MAX as u64),
        )?;
        let bytes = copy(&mut file, &mut archive).map_err(|e| io_error(&entry.path, e))?;
        check_stamp(
            entry,
            file.metadata().map_err(|e| io_error(&entry.path, e))?,
        )?;
        real_path(&entry.path)?;
        check_stamp(
            entry,
            fs::symlink_metadata(&entry.path).map_err(|e| io_error(&entry.path, e))?,
        )?;
        if bytes != entry.stamp.len {
            return Err(BackupError::SourceChanged(entry.path.clone()));
        }
        report.files += 1;
        report.bytes = report
            .bytes
            .checked_add(bytes)
            .ok_or_else(|| BackupError::Verification("Contagem de bytes excede u64".into()))?;
    }
    let mut writer = archive.finish()?;
    writer
        .flush()
        .map_err(|e| io_error(Path::new("ZIP temporário"), e))?;
    Ok(report)
}

fn verify_archive(temp: &NamedTempFile, entries: &[Entry]) -> Result<(), BackupError> {
    let file = temp.reopen().map_err(|e| io_error(temp.path(), e))?;
    let mut archive = ZipArchive::new(file)?;
    let expected_count = entries.iter().filter(|e| !e.name.is_empty()).count();
    if archive.len() != expected_count {
        return Err(BackupError::Verification(
            "Número de entradas divergente".into(),
        ));
    }
    for (index, expected) in entries.iter().filter(|e| !e.name.is_empty()).enumerate() {
        let mut entry = archive.by_index(index)?;
        let size = if expected.stamp.directory {
            0
        } else {
            expected.stamp.len
        };
        if entry.name() != expected.name
            || entry.is_dir() != expected.stamp.directory
            || entry.size() != size
        {
            return Err(BackupError::Verification(format!(
                "Entrada divergente: {}",
                expected.name
            )));
        }
        // Full streaming read validates compressed data and the ZIP CRC at EOF.
        let bytes = io::copy(&mut entry, &mut io::sink()).map_err(|e| io_error(temp.path(), e))?;
        if bytes != size {
            return Err(BackupError::Verification(format!(
                "Tamanho divergente: {}",
                expected.name
            )));
        }
    }
    Ok(())
}

fn require_absent(output: &Path) -> Result<(), BackupError> {
    match fs::symlink_metadata(output) {
        Ok(_) => Err(BackupError::DestinationExists(output.to_path_buf())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_error(output, error)),
    }
}

fn transaction<F>(source_dir: &Path, output: &Path, write: F) -> Result<BackupReport, BackupError>
where
    F: FnOnce(&mut File, &[Entry]) -> Result<BackupReport, BackupError>,
{
    let source = real_directory(source_dir)?;
    if output.components().any(|c| c == Component::ParentDir) {
        return Err(BackupError::InvalidPath(output.to_path_buf()));
    }
    let filename = output
        .file_name()
        .ok_or_else(|| BackupError::InvalidPath(output.to_path_buf()))?;
    if !matches!(output.components().next_back(), Some(Component::Normal(_))) {
        return Err(BackupError::InvalidPath(output.to_path_buf()));
    }
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let parent = real_directory(parent)?;
    let output = parent.join(filename);
    if output.starts_with(&source) {
        return Err(BackupError::DestinationInsideSource(output));
    }
    require_absent(&output)?;
    let parent_stamp = Stamp::new(
        &parent,
        &fs::symlink_metadata(&parent).map_err(|e| io_error(&parent, e))?,
    )?;
    let entries = snapshot(&source)?;
    // The temporary is on the destination filesystem; Drop removes only this
    // owned file on every failure, including a persist_noclobber collision.
    let mut temp = NamedTempFile::new_in(&parent).map_err(|e| io_error(&parent, e))?;
    let report = write(temp.as_file_mut(), &entries)?;
    temp.as_file_mut()
        .flush()
        .map_err(|e| io_error(temp.path(), e))?;
    temp.as_file()
        .sync_all()
        .map_err(|e| io_error(temp.path(), e))?;
    verify_archive(&temp, &entries)?;
    if real_directory(&source)? != source || snapshot(&source)? != entries {
        return Err(BackupError::SourceChanged(source));
    }
    if real_directory(&parent)? != parent {
        return Err(BackupError::InvalidPath(parent));
    }
    let current_parent = Stamp::new(
        &parent,
        &fs::symlink_metadata(&parent).map_err(|e| io_error(&parent, e))?,
    )?;
    // Creating our own temporary legitimately changes the parent's timestamps.
    #[cfg(any(unix, windows))]
    if current_parent.identity != parent_stamp.identity {
        return Err(BackupError::InvalidPath(parent));
    }
    #[cfg(not(any(unix, windows)))]
    let _ = (current_parent, parent_stamp);
    match temp.persist_noclobber(&output) {
        Ok(_) => Ok(report),
        Err(error) => {
            let source = error.error;
            // Explicitly keep ownership until Drop; never remove the final path.
            drop(error.file);
            if source.kind() == io::ErrorKind::AlreadyExists {
                Err(BackupError::DestinationExists(output))
            } else {
                Err(io_error(&output, source))
            }
        }
    }
}

/// Compacta o conteúdo de uma pasta real, preservando diretórios vazios.
///
/// Não sobrescreve nenhum destino, nem em uma colisão concorrente. Só publica
/// depois de `finish`, flush, sync_all, releitura/CRC e comparação da origem.
/// Falhas deixam o destino ausente (ou preservam o arquivo de outro processo).
/// `files` e `bytes` contabilizam apenas os arquivos regulares da origem.
pub fn create_zip_archive(source_dir: &Path, output: &Path) -> Result<BackupReport, BackupError> {
    transaction(source_dir, output, |file, entries| {
        write_archive(file, entries, |reader, writer| io::copy(reader, writer))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use tempfile::TempDir;

    struct Fixture {
        _temp: TempDir,
        source: PathBuf,
        destination: PathBuf,
        output: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let temp = TempDir::new().unwrap();
            let base = dunce::canonicalize(temp.path()).unwrap();
            let source = base.join("source");
            let destination = base.join("destination");
            fs::create_dir(&source).unwrap();
            fs::create_dir(&destination).unwrap();
            fs::write(source.join("save.dat"), vec![42; 128 * 1024]).unwrap();
            fs::write(destination.join("other.tmp"), b"unrelated").unwrap();
            let output = destination.join("backup.zip");
            Self {
                _temp: temp,
                source,
                destination,
                output,
            }
        }

        fn assert_cleanup(&self) {
            let paths: Vec<_> = fs::read_dir(&self.destination)
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .collect();
            assert_eq!(paths, vec![std::ffi::OsString::from("other.tmp")]);
            assert_eq!(
                fs::read(self.destination.join("other.tmp")).unwrap(),
                b"unrelated"
            );
            assert!(!self.output.exists());
        }
    }

    struct ReadFailure;
    impl Read for ReadFailure {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::new(
                io::ErrorKind::Other,
                "injected read failure",
            ))
        }
    }

    struct WriteFailure<'a> {
        file: &'a mut File,
        remaining: usize,
    }
    impl Write for WriteFailure<'_> {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.remaining == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    "injected write failure",
                ));
            }
            let count = self.file.write(&bytes[..bytes.len().min(self.remaining)])?;
            self.remaining -= count;
            Ok(count)
        }
        fn flush(&mut self) -> io::Result<()> {
            self.file.flush()
        }
    }
    impl Seek for WriteFailure<'_> {
        fn seek(&mut self, pos: io::SeekFrom) -> io::Result<u64> {
            self.file.seek(pos)
        }
    }

    #[test]
    fn reader_failure_after_partial_real_copy_aborts_and_cleans_own_temp() {
        let fx = Fixture::new();
        let result = transaction(&fx.source, &fx.output, |file, entries| {
            write_archive(file, entries, |reader, writer| {
                io::copy(&mut reader.take(4096).chain(ReadFailure), writer)
            })
        });
        assert!(matches!(result, Err(BackupError::Io { .. })));
        fx.assert_cleanup();
    }

    #[test]
    fn writer_failure_after_partial_zip_write_aborts_and_cleans_own_temp() {
        let fx = Fixture::new();
        let result = transaction(&fx.source, &fx.output, |file, entries| {
            write_archive(
                WriteFailure {
                    file,
                    remaining: 64,
                },
                entries,
                |reader, writer| io::copy(reader, writer),
            )
        });
        assert!(result.is_err());
        fx.assert_cleanup();
    }

    #[test]
    fn source_mutation_during_stream_is_rejected() {
        let fx = Fixture::new();
        let path = fx.source.join("save.dat");
        let result = transaction(&fx.source, &fx.output, |file, entries| {
            write_archive(file, entries, |reader, writer| {
                let bytes = io::copy(reader, writer)?;
                fs::write(&path, b"changed")?;
                Ok(bytes)
            })
        });
        assert!(matches!(result, Err(BackupError::SourceChanged(_))));
        fx.assert_cleanup();
    }

    #[test]
    fn added_entry_after_stream_is_rejected_by_final_tree_check() {
        let fx = Fixture::new();
        let result = transaction(&fx.source, &fx.output, |file, entries| {
            let report = write_archive(file, entries, |reader, writer| io::copy(reader, writer))?;
            fs::write(fx.source.join("new.dat"), b"new").unwrap();
            Ok(report)
        });
        assert!(matches!(result, Err(BackupError::SourceChanged(_))));
        fx.assert_cleanup();
    }

    #[test]
    fn concurrent_publish_collision_keeps_other_process_file_and_cleans_temp() {
        let fx = Fixture::new();
        let result = transaction(&fx.source, &fx.output, |file, entries| {
            let report = write_archive(file, entries, |reader, writer| io::copy(reader, writer))?;
            fs::write(&fx.output, b"concurrent backup").unwrap();
            Ok(report)
        });
        assert!(matches!(result, Err(BackupError::DestinationExists(_))));
        assert_eq!(fs::read(&fx.output).unwrap(), b"concurrent backup");
        let mut names: Vec<_> = fs::read_dir(&fx.destination)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                std::ffi::OsString::from("backup.zip"),
                std::ffi::OsString::from("other.tmp")
            ]
        );
    }

    #[test]
    fn corrupted_completed_archive_is_never_published() {
        let fx = Fixture::new();
        let result = transaction(&fx.source, &fx.output, |file, entries| {
            let report = write_archive(&mut *file, entries, |reader, writer| {
                io::copy(reader, writer)
            })?;
            file.set_len(8).unwrap();
            Ok(report)
        });
        assert!(result.is_err());
        fx.assert_cleanup();
    }
}
