//! Regressões destrutivas usam somente fixtures temporárias, sem dados reais.
//! Raízes e aliases por link são recusados; links dentro de uma pasta removida
//! são apenas desvinculados, preservando o conteúdo fora daquela pasta.

use s4suite::core::safety::{
    is_safe_to_delete, safe_remove_dir_all, safe_remove_file, SafetyError,
};
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

struct Fixture {
    _temp: TempDir,
    base: PathBuf,
    mods: PathBuf,
    external: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = TempDir::new().unwrap();
        // Evita aliases da pasta temporária do sistema na própria fixture.
        let base = dunce::canonicalize(temp.path()).unwrap();
        let mods = base.join("Mods");
        let external = base.join("External");
        fs::create_dir_all(&mods).unwrap();
        fs::create_dir_all(&external).unwrap();
        Self {
            _temp: temp,
            base,
            mods,
            external,
        }
    }

    fn roots(&self) -> Vec<PathBuf> {
        vec![self.mods.clone()]
    }

    fn file(&self, path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"preserved fixture content").unwrap();
    }
}

#[test]
fn mods_root_and_dotdot_aliases_are_protected() {
    let fx = Fixture::new();
    let inner = fx.mods.join("Inner");
    fs::create_dir_all(&inner).unwrap();
    let sentinel = fx.mods.join("keep.package");
    fx.file(&sentinel);

    for target in [
        &fx.mods,
        &fx.mods.join("."),
        &inner.join(".."),
        &inner.join("../."),
    ] {
        assert!(matches!(
            is_safe_to_delete(target, &fx.roots()),
            Err(SafetyError::ProtectedPath(_))
        ));
        assert!(safe_remove_dir_all(target, &fx.roots()).is_err());
    }
    assert!(inner.is_dir());
    assert!(sentinel.is_file());
}

#[test]
fn every_overlapping_root_is_protected_in_either_order() {
    let fx = Fixture::new();
    let category = fx.mods.join("Category");
    fs::create_dir_all(&category).unwrap();
    let sentinel = category.join("keep.package");
    fx.file(&sentinel);

    for roots in [
        vec![fx.base.clone(), fx.mods.clone(), category.clone()],
        vec![category.clone(), fx.mods.clone(), fx.base.clone()],
    ] {
        for target in [&fx.base, &fx.mods, &category] {
            assert!(matches!(
                is_safe_to_delete(target, &roots),
                Err(SafetyError::ProtectedPath(_))
            ));
            assert!(safe_remove_dir_all(target, &roots).is_err());
        }
    }
    assert!(sentinel.is_file());
}

#[test]
fn a_folder_containing_an_allowed_root_is_protected() {
    let fx = Fixture::new();
    let category = fx.mods.join("Category");
    let nested_root = category.join("ProtectedRoot");
    fs::create_dir_all(&nested_root).unwrap();
    let roots = vec![fx.mods.clone(), nested_root.clone()];

    assert!(safe_remove_dir_all(&category, &roots).is_err());
    assert!(nested_root.is_dir());
}

#[test]
fn external_paths_and_dotdot_escape_preserve_files_and_folders() {
    let fx = Fixture::new();
    let file = fx.external.join("keep.package");
    fx.file(&file);
    let prefix_neighbor = fx.base.join("Mods-other");
    let neighbor_file = prefix_neighbor.join("keep.package");
    fx.file(&neighbor_file);

    for target in [
        &file,
        &fx.mods.join("../External/keep.package"),
        &neighbor_file,
    ] {
        assert!(matches!(
            is_safe_to_delete(target, &fx.roots()),
            Err(SafetyError::OutsideAllowedScope(_, _))
        ));
        assert!(safe_remove_file(target, &fx.roots()).is_err());
    }
    assert!(safe_remove_dir_all(&fx.external, &fx.roots()).is_err());
    assert!(safe_remove_dir_all(&prefix_neighbor, &fx.roots()).is_err());
    assert_eq!(fs::read(&file).unwrap(), b"preserved fixture content");
    assert!(neighbor_file.is_file());
}

#[test]
fn real_internal_files_and_nested_folders_are_removed() {
    let fx = Fixture::new();
    let file = fx.mods.join("Category/file.package");
    fx.file(&file);
    let category = file.parent().unwrap();
    assert_eq!(is_safe_to_delete(&file, &fx.roots()).unwrap(), file);
    safe_remove_file(&category.join("./file.package"), &fx.roots()).unwrap();
    assert!(!file.exists());
    assert!(category.is_dir());

    let nested_file = category.join("Nested/child.package");
    fx.file(&nested_file);
    safe_remove_dir_all(category, &fx.roots()).unwrap();
    assert!(!category.exists());
    assert!(fx.mods.is_dir());
}

#[test]
fn internal_dotdot_aliases_are_allowed_only_when_they_stay_in_scope() {
    let fx = Fixture::new();
    let category = fx.mods.join("Category");
    fs::create_dir_all(&category).unwrap();
    let file = fx.mods.join("remove.package");
    fx.file(&file);

    safe_remove_file(&category.join("../remove.package"), &fx.roots()).unwrap();
    assert!(!file.exists());
    assert!(category.is_dir());
}

#[test]
fn configured_dot_and_dotdot_aliases_keep_the_same_root_boundary() {
    let fx = Fixture::new();
    let category = fx.mods.join("Category");
    fs::create_dir_all(&category).unwrap();
    let file = fx.mods.join("remove.package");

    for root in [fx.mods.join("."), category.join("..")] {
        let roots = vec![root];
        assert!(matches!(
            is_safe_to_delete(&fx.mods, &roots),
            Err(SafetyError::ProtectedPath(_))
        ));
        fx.file(&file);
        safe_remove_file(&file, &roots).unwrap();
        assert!(!file.exists());
    }
    assert!(fx.mods.is_dir());
    assert!(category.is_dir());
}

#[test]
fn wrong_types_and_missing_targets_never_report_success() {
    let fx = Fixture::new();
    let folder = fx.mods.join("Folder");
    fs::create_dir_all(&folder).unwrap();
    let file = fx.mods.join("keep.package");
    fx.file(&file);
    let missing = fx.mods.join("missing.package");
    let missing_parent = fx.mods.join("missing-parent/missing.package");

    assert!(safe_remove_file(&folder, &fx.roots()).is_err());
    assert!(safe_remove_dir_all(&file, &fx.roots()).is_err());
    for target in [&missing, &missing_parent] {
        assert!(is_safe_to_delete(target, &fx.roots()).is_err());
        assert!(safe_remove_file(target, &fx.roots()).is_err());
        assert!(safe_remove_dir_all(target, &fx.roots()).is_err());
    }
    assert!(folder.is_dir());
    assert!(file.is_file());
}

#[test]
fn invalid_configured_roots_fail_closed_even_beside_a_valid_root() {
    let fx = Fixture::new();
    let file = fx.mods.join("keep.package");
    fx.file(&file);
    let non_directory = fx.external.join("not-a-directory");
    fx.file(&non_directory);

    for invalid in [non_directory, fx.base.join("missing-root")] {
        let roots = vec![fx.mods.clone(), invalid];
        assert!(safe_remove_file(&file, &roots).is_err());
        assert!(file.is_file());
    }
    assert!(is_safe_to_delete(&file, &[]).is_err());
}

#[cfg(unix)]
#[test]
fn system_and_home_roots_are_rejected_without_mutating_them() {
    // Somente consulta de autorização: nenhuma remoção em caminhos reais.
    for system in ["/", "/usr", "/etc", "/home", "/proc", "/dev"] {
        let path = Path::new(system);
        if path.exists() {
            assert!(is_safe_to_delete(path, &[PathBuf::from("/")]).is_err());
        }
    }
    for home in [
        dirs::home_dir(),
        dirs::document_dir(),
        dirs::desktop_dir(),
        dirs::download_dir(),
    ]
    .into_iter()
    .flatten()
    {
        if home.exists() {
            assert!(is_safe_to_delete(&home.join("."), &[PathBuf::from("/")]).is_err());
        }
    }
}

#[cfg(unix)]
mod links {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn internal_and_external_file_links_are_not_followed_or_unlinked() {
        let fx = Fixture::new();
        for (name, target) in [
            ("internal-link", fx.mods.join("keep.package")),
            ("external-link", fx.external.join("keep.package")),
        ] {
            fx.file(&target);
            let link = fx.mods.join(name);
            symlink(&target, &link).unwrap();
            assert!(is_safe_to_delete(&link, &fx.roots()).is_err());
            assert!(safe_remove_file(&link, &fx.roots()).is_err());
            assert!(safe_remove_dir_all(&link, &fx.roots()).is_err());
            assert!(fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink());
            assert_eq!(fs::read(&target).unwrap(), b"preserved fixture content");
        }
    }

    #[test]
    fn internal_and_external_directory_links_block_descendant_removal() {
        let fx = Fixture::new();
        for (name, target) in [
            ("internal-link", fx.mods.join("Real")),
            ("external-link", fx.external.clone()),
        ] {
            let file = target.join("Nested/keep.package");
            fx.file(&file);
            let link = fx.mods.join(name);
            symlink(&target, &link).unwrap();
            assert!(safe_remove_dir_all(&link, &fx.roots()).is_err());
            assert!(safe_remove_file(&link.join("Nested/keep.package"), &fx.roots()).is_err());
            assert!(safe_remove_dir_all(&link.join("Nested"), &fx.roots()).is_err());
            assert!(file.is_file());
            assert!(fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink());
        }
    }

    #[test]
    fn trailing_separator_does_not_hide_a_directory_link() {
        let fx = Fixture::new();
        let file = fx.external.join("keep.package");
        fx.file(&file);
        let link = fx.mods.join("external-link");
        symlink(&fx.external, &link).unwrap();
        let alias = PathBuf::from(format!("{}/", link.display()));

        assert!(safe_remove_dir_all(&alias, &fx.roots()).is_err());
        assert!(file.is_file());
        assert!(fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[test]
    fn symlink_before_dotdot_is_rejected_even_when_final_path_is_internal() {
        let fx = Fixture::new();
        let real = fx.mods.join("Real");
        fs::create_dir_all(&real).unwrap();
        let link = fx.mods.join("alias");
        symlink(&real, &link).unwrap();
        let file = fx.mods.join("keep.package");
        fx.file(&file);

        assert!(safe_remove_file(&link.join("../keep.package"), &fx.roots()).is_err());
        assert!(file.is_file());
    }

    #[test]
    fn configured_root_link_and_ancestor_link_do_not_grant_authorization() {
        let fx = Fixture::new();
        let file = fx.mods.join("keep.package");
        fx.file(&file);
        let root_link = fx.base.join("ModsAlias");
        symlink(&fx.mods, &root_link).unwrap();
        let base_link = fx.base.join("BaseAlias");
        symlink(&fx.base, &base_link).unwrap();

        for roots in [
            vec![root_link.clone()],
            vec![base_link.join("Mods")],
            vec![fx.mods.clone(), root_link.clone()],
        ] {
            assert!(safe_remove_file(&file, &roots).is_err());
            assert!(file.is_file());
        }
        assert!(safe_remove_dir_all(&root_link, &fx.roots()).is_err());
        assert!(safe_remove_file(&root_link.join("keep.package"), &fx.roots()).is_err());
        assert!(safe_remove_file(&base_link.join("Mods/keep.package"), &fx.roots()).is_err());
        assert!(file.is_file());
    }

    #[test]
    fn dangling_links_fail_closed() {
        let fx = Fixture::new();
        let link = fx.mods.join("dangling-link");
        symlink(fx.external.join("missing.package"), &link).unwrap();

        assert!(safe_remove_file(&link, &fx.roots()).is_err());
        assert!(safe_remove_dir_all(&link, &fx.roots()).is_err());
        assert!(fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[test]
    fn deleting_a_real_directory_unlinks_children_without_deleting_their_targets() {
        let fx = Fixture::new();
        let removable = fx.mods.join("Removable");
        let own_file = removable.join("Nested/remove.package");
        fx.file(&own_file);
        let external_file = fx.external.join("Folder/keep.package");
        fx.file(&external_file);
        let internal_file = fx.mods.join("Preserved/keep.package");
        fx.file(&internal_file);
        symlink(&external_file, removable.join("external-file-link")).unwrap();
        symlink(
            &fx.external,
            removable.join("Nested/external-directory-link"),
        )
        .unwrap();
        symlink(&internal_file, removable.join("internal-file-link")).unwrap();
        symlink(fx.external.join("missing"), removable.join("dangling-link")).unwrap();

        safe_remove_dir_all(&removable, &fx.roots()).unwrap();
        assert!(!removable.exists());
        assert_eq!(
            fs::read(&external_file).unwrap(),
            b"preserved fixture content"
        );
        assert_eq!(
            fs::read(&internal_file).unwrap(),
            b"preserved fixture content"
        );
        assert!(fx.external.is_dir());
        assert!(fx.mods.is_dir());
    }

    #[test]
    fn special_files_are_rejected_without_unlinking_them() {
        let fx = Fixture::new();
        let socket = fx.mods.join("socket");
        let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();

        assert!(is_safe_to_delete(&socket, &fx.roots()).is_err());
        assert!(safe_remove_file(&socket, &fx.roots()).is_err());
        assert!(safe_remove_dir_all(&socket, &fx.roots()).is_err());
        assert!(socket.exists());
    }
}

#[test]
fn empty_target_never_authorizes_an_implicit_working_directory() {
    let fx = Fixture::new();
    let sentinel = fx.mods.join("keep.package");
    fx.file(&sentinel);
    assert!(matches!(
        is_safe_to_delete(Path::new(""), &fx.roots()),
        Err(SafetyError::InvalidPath(_))
    ));
    assert!(safe_remove_file(Path::new(""), &fx.roots()).is_err());
    assert!(safe_remove_dir_all(Path::new(""), &fx.roots()).is_err());
    assert_eq!(fs::read(sentinel).unwrap(), b"preserved fixture content");
}
