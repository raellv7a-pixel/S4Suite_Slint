//! Todas as fontes e destinos são sintéticos e vivem em TempDir.
//! A releitura até EOF exercita a descompressão e a verificação CRC do ZIP.

use s4suite::engine::backup::{create_zip_archive, BackupError, BackupReport};
use std::fs::{self, File};
use std::io::Read;
use std::path::PathBuf;
use tempfile::TempDir;
use zip::ZipArchive;

struct Fixture {
    _temp: TempDir,
    base: PathBuf,
    source: PathBuf,
    destination: PathBuf,
    output: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = TempDir::new().unwrap();
        let base = dunce::canonicalize(temp.path()).unwrap();
        let source = base.join("Saves");
        let destination = base.join("Backups");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("keep.tmp"), b"unrelated temporary").unwrap();
        let output = destination.join("saves.zip");
        Self {
            _temp: temp,
            base,
            source,
            destination,
            output,
        }
    }

    fn write(&self, relative: &str, content: &[u8]) {
        let file = self.source.join(relative);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, content).unwrap();
    }

    fn assert_no_output_or_leaked_temp(&self) {
        let names: Vec<_> = fs::read_dir(&self.destination)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("keep.tmp")]);
        assert_eq!(
            fs::read(self.destination.join("keep.tmp")).unwrap(),
            b"unrelated temporary"
        );
        assert!(!self.output.exists());
    }
}

fn assert_content(archive: &mut ZipArchive<File>, name: &str, expected: &[u8]) {
    let mut entry = archive.by_name(name).unwrap();
    assert!(!entry.is_dir());
    assert_eq!(entry.size(), expected.len() as u64);
    let mut buffer = [0_u8; 16 * 1024];
    for chunk in expected.chunks(buffer.len()) {
        entry.read_exact(&mut buffer[..chunk.len()]).unwrap();
        assert_eq!(&buffer[..chunk.len()], chunk);
    }
    // CRC errors are surfaced at EOF, not merely when opening the entry.
    assert_eq!(entry.read(&mut buffer).unwrap(), 0);
}

#[test]
fn nested_tree_large_stream_empty_directories_and_zip_contents_are_preserved() {
    let fx = Fixture::new();
    let mut state = 0x1234_5678_u32;
    let large: Vec<u8> = (0..2 * 1024 * 1024 + 137)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state as u8
        })
        .collect();
    fx.write("Slot_00000001.save", b"main save");
    fx.write("nested/another/Slot_00000001.save.ver0", &large);
    fx.write("nested/another/zero.save", b"");
    fx.write("unicode/ação.save", b"unicode");
    fs::create_dir_all(fx.source.join("empty/deep-empty")).unwrap();

    let report = create_zip_archive(&fx.source, &fx.output).unwrap();
    assert_eq!(
        report,
        BackupReport {
            files: 4,
            bytes: large.len() as u64 + 9 + 7
        }
    );
    let mut archive = ZipArchive::new(File::open(&fx.output).unwrap()).unwrap();
    let mut names: Vec<_> = (0..archive.len())
        .map(|index| archive.by_index(index).unwrap().name().to_owned())
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec![
            "Slot_00000001.save",
            "empty/",
            "empty/deep-empty/",
            "nested/",
            "nested/another/",
            "nested/another/Slot_00000001.save.ver0",
            "nested/another/zero.save",
            "unicode/",
            "unicode/ação.save",
        ]
    );
    for name in [
        "empty/",
        "empty/deep-empty/",
        "nested/",
        "nested/another/",
        "unicode/",
    ] {
        let entry = archive.by_name(name).unwrap();
        assert!(entry.is_dir());
        assert_eq!(entry.size(), 0);
    }
    assert_content(&mut archive, "Slot_00000001.save", b"main save");
    assert_content(
        &mut archive,
        "nested/another/Slot_00000001.save.ver0",
        &large,
    );
    assert_content(&mut archive, "nested/another/zero.save", b"");
    assert_content(&mut archive, "unicode/ação.save", b"unicode");
    assert_eq!(
        fs::read(fx.source.join("nested/another/Slot_00000001.save.ver0")).unwrap(),
        large
    );
    assert_eq!(
        fs::read(fx.destination.join("keep.tmp")).unwrap(),
        b"unrelated temporary"
    );
    assert_eq!(fs::read_dir(&fx.destination).unwrap().count(), 2);
}

#[test]
fn empty_source_publishes_a_valid_empty_zip() {
    let fx = Fixture::new();
    assert_eq!(
        create_zip_archive(&fx.source, &fx.output).unwrap(),
        BackupReport { files: 0, bytes: 0 }
    );
    let archive = ZipArchive::new(File::open(&fx.output).unwrap()).unwrap();
    assert_eq!(archive.len(), 0);
}

#[test]
fn existing_backup_is_never_truncated_or_replaced() {
    let fx = Fixture::new();
    fx.write("save.dat", b"new data");
    let original = b"existing backup must stay byte-identical";
    fs::write(&fx.output, original).unwrap();
    assert!(matches!(
        create_zip_archive(&fx.source, &fx.output),
        Err(BackupError::DestinationExists(_))
    ));
    assert_eq!(fs::read(&fx.output).unwrap(), original);
    assert_eq!(fs::read_dir(&fx.destination).unwrap().count(), 2);
}

#[test]
fn existing_destination_directory_is_preserved() {
    let fx = Fixture::new();
    fs::create_dir(&fx.output).unwrap();
    fs::write(fx.output.join("keep"), b"keep directory content").unwrap();
    assert!(matches!(
        create_zip_archive(&fx.source, &fx.output),
        Err(BackupError::DestinationExists(_))
    ));
    assert_eq!(
        fs::read(fx.output.join("keep")).unwrap(),
        b"keep directory content"
    );
    assert_eq!(fs::read_dir(&fx.destination).unwrap().count(), 2);
}

#[test]
fn invalid_sources_and_output_parents_abort_without_side_effects() {
    let fx = Fixture::new();
    let file = fx.base.join("not-a-directory");
    fs::write(&file, b"keep").unwrap();
    for source in [&file, &fx.base.join("missing-source")] {
        assert!(create_zip_archive(source, &fx.output).is_err());
    }
    for output in [
        fx.base.join("missing-parent/backup.zip"),
        file.join("backup.zip"),
        PathBuf::new(),
    ] {
        assert!(create_zip_archive(&fx.source, &output).is_err());
    }
    assert_eq!(fs::read(file).unwrap(), b"keep");
    fx.assert_no_output_or_leaked_temp();
}

#[test]
fn parentdir_aliases_are_rejected_for_source_and_output() {
    let fx = Fixture::new();
    fs::create_dir(fx.source.join("inner")).unwrap();
    let source = fx.source.join("inner/..");
    assert!(matches!(
        create_zip_archive(&source, &fx.output),
        Err(BackupError::InvalidPath(_))
    ));
    let output = fx.destination.join("../Backups/saves.zip");
    assert!(matches!(
        create_zip_archive(&fx.source, &output),
        Err(BackupError::InvalidPath(_))
    ));
    fx.assert_no_output_or_leaked_temp();
}

#[test]
fn destination_inside_source_is_rejected_at_every_depth() {
    let fx = Fixture::new();
    fx.write("nested/save.dat", b"preserved");
    for output in [
        fx.source.join("backup.zip"),
        fx.source.join("nested/backup.zip"),
    ] {
        assert!(matches!(
            create_zip_archive(&fx.source, &output),
            Err(BackupError::DestinationInsideSource(_))
        ));
        assert!(!output.exists());
    }
    assert_eq!(
        fs::read(fx.source.join("nested/save.dat")).unwrap(),
        b"preserved"
    );
    fx.assert_no_output_or_leaked_temp();
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::os::unix::fs::symlink;
    use std::os::unix::net::UnixListener;

    #[test]
    fn source_root_link_and_source_ancestor_link_are_rejected() {
        let fx = Fixture::new();
        fx.write("nested/save.dat", b"preserved");
        let alias = fx.base.join("source-alias");
        symlink(&fx.source, &alias).unwrap();
        for source in [&alias, &alias.join("nested")] {
            assert!(matches!(
                create_zip_archive(source, &fx.output),
                Err(BackupError::UnsafeEntry(_))
            ));
        }
        fx.assert_no_output_or_leaked_temp();
    }

    #[test]
    fn file_directory_and_dangling_source_links_abort_instead_of_being_omitted() {
        for target_kind in ["file", "directory", "missing"] {
            let fx = Fixture::new();
            fx.write("normal.save", b"normal");
            let external = fx.base.join("external");
            match target_kind {
                "file" => fs::write(&external, b"outside content").unwrap(),
                "directory" => {
                    fs::create_dir(&external).unwrap();
                    fs::write(external.join("keep.dat"), b"outside content").unwrap();
                }
                _ => {}
            }
            symlink(&external, fx.source.join("link")).unwrap();
            assert!(matches!(
                create_zip_archive(&fx.source, &fx.output),
                Err(BackupError::UnsafeEntry(_))
            ));
            assert!(fs::symlink_metadata(fx.source.join("link"))
                .unwrap()
                .file_type()
                .is_symlink());
            match target_kind {
                "file" => assert_eq!(fs::read(&external).unwrap(), b"outside content"),
                "directory" => assert_eq!(
                    fs::read(external.join("keep.dat")).unwrap(),
                    b"outside content"
                ),
                _ => assert!(!external.exists()),
            }
            fx.assert_no_output_or_leaked_temp();
        }
    }

    #[test]
    fn output_parent_and_output_ancestor_links_are_rejected() {
        let fx = Fixture::new();
        let alias = fx.base.join("output-alias");
        fs::create_dir(fx.destination.join("inner")).unwrap();
        symlink(&fx.destination, &alias).unwrap();
        for output in [alias.join("backup.zip"), alias.join("inner/backup.zip")] {
            assert!(matches!(
                create_zip_archive(&fx.source, &output),
                Err(BackupError::UnsafeEntry(_))
            ));
            assert!(!output.exists());
        }
        assert_eq!(
            fs::read_dir(fx.destination.join("inner")).unwrap().count(),
            0
        );
        assert_eq!(fs::read_dir(&fx.destination).unwrap().count(), 2);
    }

    #[test]
    fn output_links_including_dangling_links_are_existing_destinations() {
        for dangling in [false, true] {
            let fx = Fixture::new();
            let external = fx.base.join("outside.zip");
            if !dangling {
                fs::write(&external, b"existing external archive").unwrap();
            }
            symlink(&external, &fx.output).unwrap();
            assert!(matches!(
                create_zip_archive(&fx.source, &fx.output),
                Err(BackupError::DestinationExists(_))
            ));
            assert!(fs::symlink_metadata(&fx.output)
                .unwrap()
                .file_type()
                .is_symlink());
            if dangling {
                assert!(!external.exists());
            } else {
                assert_eq!(fs::read(external).unwrap(), b"existing external archive");
            }
            assert_eq!(fs::read_dir(&fx.destination).unwrap().count(), 2);
        }
    }

    #[test]
    fn special_source_socket_is_rejected_without_opening_or_hanging() {
        let fx = Fixture::new();
        let _listener = UnixListener::bind(fx.source.join("special.socket")).unwrap();
        assert!(matches!(
            create_zip_archive(&fx.source, &fx.output),
            Err(BackupError::UnsafeEntry(_))
        ));
        fx.assert_no_output_or_leaked_temp();
    }

    #[test]
    fn non_utf8_names_are_rejected_instead_of_lossy_zip_collisions() {
        use std::os::unix::ffi::OsStringExt;
        let fx = Fixture::new();
        fs::write(
            fx.source
                .join(std::ffi::OsString::from_vec(vec![b's', 0xff])),
            b"keep",
        )
        .unwrap();
        assert!(matches!(
            create_zip_archive(&fx.source, &fx.output),
            Err(BackupError::InvalidPath(_))
        ));
        fx.assert_no_output_or_leaked_temp();
    }
}

#[test]
fn ambiguous_zip_separator_names_are_rejected() {
    let fx = Fixture::new();
    // A backslash is a normal filename character on Unix but a path separator
    // on Windows. A portable ZIP must not silently change its interpretation.
    #[cfg(unix)]
    {
        fx.write("folder\\save.dat", b"keep");
        assert!(matches!(
            create_zip_archive(&fx.source, &fx.output),
            Err(BackupError::InvalidPath(_))
        ));
        fx.assert_no_output_or_leaked_temp();
    }
    #[cfg(not(unix))]
    let _ = fx;
}
