use s4suite::core::safety::SafetyError;
use s4suite::engine::cache::clean_game_cache;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

const NAMES: [&str; 4] = [
    "localthumbcache.package",
    "spotlight_thumbnails.package",
    "cache",
    "cachestr",
];

struct Fixture {
    _temp: TempDir,
    game: PathBuf,
    external: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        // Canonicalize the OS temporary parent so the fixture itself has no
        // ancestor aliases (notably /var on macOS).
        let base = dunce::canonicalize(temp.path()).unwrap();
        let game = base.join("The Sims 4");
        let external = base.join("external");
        fs::create_dir_all(game.join("Mods")).unwrap();
        fs::create_dir(&external).unwrap();
        Self {
            _temp: temp,
            game,
            external,
        }
    }

    fn populate(&self) {
        for name in &NAMES[..2] {
            fs::write(self.game.join(name), b"cache").unwrap();
        }
        for name in &NAMES[2..] {
            fs::create_dir_all(self.game.join(name).join("nested")).unwrap();
            fs::write(self.game.join(name).join("nested/data"), b"cache").unwrap();
        }
    }

    fn paths(&self) -> Vec<PathBuf> {
        NAMES.iter().map(|name| self.game.join(name)).collect()
    }
}

#[test]
fn removes_only_four_correct_cache_targets() {
    let fx = Fixture::new();
    fx.populate();
    let keep = [
        fx.game.join("Mods/keep.package"),
        fx.game.join("saves/Slot_00000001.save"),
        fx.game.join("Tray/keep.trayitem"),
        fx.game.join("localthumbcache.package.bak"),
        fx.game.join("OtherCache/keep"),
    ];
    for path in &keep {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"preserve").unwrap();
    }
    let report = clean_game_cache(&fx.game).unwrap();
    assert_eq!(report.removed, fx.paths());
    assert!(report.missing.is_empty());
    assert!(report.failures.is_empty());
    for path in fx.paths() {
        assert!(fs::symlink_metadata(path).is_err());
    }
    for path in keep {
        assert_eq!(fs::read(path).unwrap(), b"preserve");
    }
    assert!(fx.game.join("Mods").is_dir());
}

#[test]
fn absent_targets_are_missing_and_second_cleanup_is_idempotent() {
    let fx = Fixture::new();
    let report = clean_game_cache(&fx.game).unwrap();
    assert_eq!(report.missing, fx.paths());
    assert!(report.removed.is_empty());
    assert!(report.failures.is_empty());
    fx.populate();
    assert_eq!(clean_game_cache(&fx.game).unwrap().removed, fx.paths());
    let report = clean_game_cache(&fx.game).unwrap();
    assert_eq!(report.missing, fx.paths());
    assert!(report.failures.is_empty());
}

#[test]
fn incorrect_types_fail_independently_without_removal() {
    for wrong_index in 0..NAMES.len() {
        let fx = Fixture::new();
        fx.populate();
        let wrong = fx.game.join(NAMES[wrong_index]);
        if wrong_index < 2 {
            fs::remove_file(&wrong).unwrap();
            fs::create_dir(&wrong).unwrap();
            fs::write(wrong.join("keep"), b"preserve").unwrap();
        } else {
            fs::remove_dir_all(&wrong).unwrap();
            fs::write(&wrong, b"preserve").unwrap();
        }
        let report = clean_game_cache(&fx.game).unwrap();
        let expected: Vec<_> = fx
            .paths()
            .into_iter()
            .filter(|path| path != &wrong)
            .collect();
        assert_eq!(report.removed, expected);
        assert!(report.missing.is_empty());
        assert_eq!(report.failures.len(), 1);
        assert_eq!(report.failures[0].path, wrong);
        assert!(!report.failures[0].error.is_empty());
        let keep = if wrong_index < 2 {
            wrong.join("keep")
        } else {
            wrong
        };
        assert_eq!(fs::read(keep).unwrap(), b"preserve");
    }
}

#[test]
fn invalid_game_roots_do_not_touch_any_cache() {
    let fx = Fixture::new();
    fx.populate();
    let file_root = fx.external.join("file");
    fs::write(&file_root, b"preserve").unwrap();
    let not_game = fx.external.join("no-mods");
    fs::create_dir(&not_game).unwrap();
    fs::write(not_game.join(NAMES[0]), b"preserve").unwrap();
    let wrong_mods = fx.external.join("file-mods");
    fs::create_dir(&wrong_mods).unwrap();
    fs::write(wrong_mods.join("Mods"), b"preserve").unwrap();
    fs::write(wrong_mods.join(NAMES[0]), b"preserve").unwrap();
    for root in [
        fx.external.join("missing"),
        file_root.clone(),
        not_game.clone(),
        wrong_mods.clone(),
    ] {
        assert!(clean_game_cache(&root).is_err());
    }
    assert_eq!(fs::read(file_root).unwrap(), b"preserve");
    assert_eq!(fs::read(not_game.join(NAMES[0])).unwrap(), b"preserve");
    assert_eq!(fs::read(wrong_mods.join(NAMES[0])).unwrap(), b"preserve");
    for target in fx.paths() {
        assert!(target.exists());
    }
}

#[test]
fn ambiguous_paths_are_rejected_before_deletion() {
    let fx = Fixture::new();
    fx.populate();
    fs::create_dir(fx.game.join("child")).unwrap();
    let alias = fx.game.join("child").join("..");
    assert!(matches!(
        clean_game_cache(&alias),
        Err(SafetyError::InvalidPath(_))
    ));
    assert!(clean_game_cache(Path::new("")).is_err());
    for target in fx.paths() {
        assert!(target.exists());
    }
}

#[test]
fn filesystem_root_is_protected_without_reading_real_game_data() {
    let fx = Fixture::new();
    // Purely lexical root selection: clean_game_cache refuses a root before
    // looking for Mods or touching filesystem content outside the fixture.
    let root = fx.game.ancestors().last().unwrap();
    assert!(matches!(
        clean_game_cache(root),
        Err(SafetyError::ProtectedPath(_))
    ));
}

#[cfg(unix)]
mod unix_links {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn internal_external_and_dangling_targets_fail_without_following_links() {
        for name in NAMES {
            for kind in ["internal", "external", "dangling"] {
                let fx = Fixture::new();
                fx.populate();
                let path = fx.game.join(name);
                let is_dir = name == "cache" || name == "cachestr";
                if is_dir {
                    fs::remove_dir_all(&path).unwrap();
                } else {
                    fs::remove_file(&path).unwrap();
                }
                let target = match kind {
                    "internal" => fx.game.join("keep-target"),
                    "external" => fx.external.join("keep-target"),
                    _ => fx.external.join("missing-target"),
                };
                if kind != "dangling" {
                    if is_dir {
                        fs::create_dir(&target).unwrap();
                        fs::write(target.join("keep"), b"preserve").unwrap();
                    } else {
                        fs::write(&target, b"preserve").unwrap();
                    }
                }
                symlink(&target, &path).unwrap();
                let report = clean_game_cache(&fx.game).unwrap();
                assert_eq!(report.failures.len(), 1, "{name}: {kind}");
                assert_eq!(report.failures[0].path, path);
                assert_eq!(report.removed.len(), 3);
                assert!(report.missing.is_empty());
                assert!(fs::symlink_metadata(&path)
                    .unwrap()
                    .file_type()
                    .is_symlink());
                if kind != "dangling" {
                    let keep = if is_dir { target.join("keep") } else { target };
                    assert_eq!(fs::read(keep).unwrap(), b"preserve");
                }
            }
        }
    }

    #[test]
    fn linked_game_root_ancestor_and_mods_fail_before_any_removal() {
        let fx = Fixture::new();
        fx.populate();
        let link = fx.external.join("game-link");
        symlink(&fx.game, &link).unwrap();
        assert!(clean_game_cache(&link).is_err());
        let ancestor = fx.external.join("ancestor-link");
        let dangling_root = fx.external.join("dangling-game-link");
        symlink(fx.external.join("missing-game"), &dangling_root).unwrap();
        assert!(clean_game_cache(&dangling_root).is_err());
        symlink(fx.game.parent().unwrap(), &ancestor).unwrap();
        assert!(clean_game_cache(&ancestor.join("The Sims 4")).is_err());
        fs::remove_dir(fx.game.join("Mods")).unwrap();
        symlink(&fx.external, fx.game.join("Mods")).unwrap();
        assert!(clean_game_cache(&fx.game).is_err());
        for path in fx.paths() {
            assert!(path.exists());
        }
        fs::remove_file(fx.game.join("Mods")).unwrap();
        symlink(fx.external.join("absent"), fx.game.join("Mods")).unwrap();
        assert!(clean_game_cache(&fx.game).is_err());
        for path in fx.paths() {
            assert!(path.exists());
        }
    }

    #[test]
    fn real_cache_directories_unlink_nested_links_and_preserve_external_targets() {
        let fx = Fixture::new();
        fx.populate();
        let external_file = fx.external.join("keep.package");
        fs::write(&external_file, b"preserve").unwrap();
        let external_dir = fx.external.join("directory");
        fs::create_dir(&external_dir).unwrap();
        fs::write(external_dir.join("keep"), b"preserve").unwrap();
        for name in &NAMES[2..] {
            let nested = fx.game.join(name).join("nested");
            symlink(&external_file, nested.join("file-link")).unwrap();
            symlink(&external_dir, nested.join("directory-link")).unwrap();
            symlink(fx.external.join("missing"), nested.join("dangling-link")).unwrap();
        }
        let report = clean_game_cache(&fx.game).unwrap();
        assert_eq!(report.removed, fx.paths());
        assert!(report.failures.is_empty());
        assert_eq!(fs::read(external_file).unwrap(), b"preserve");
        assert_eq!(fs::read(external_dir.join("keep")).unwrap(), b"preserve");
    }
}
