use crate::core::safety::{safe_remove_file, unique_dest_path};
use crate::engine::tray::{fast_md5, full_sha256};
use crate::engine::walk_user_mods;
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct ModNode {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub size: u64,
    pub disabled: bool,
}

pub fn scan_mods_tree(mods_dir: &Path) -> Vec<ModNode> {
    let mut nodes = Vec::new();
    if !mods_dir.exists() {
        return nodes;
    }

    for entry in walk_user_mods(mods_dir, 5) {
        let path = entry.path();
        if path == mods_dir {
            continue;
        }

        let name = entry.file_name().to_string_lossy().to_string();
        let is_dir = entry.file_type().is_dir();
        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
        let disabled = name.ends_with(".disabled");

        nodes.push(ModNode {
            name,
            path: path.to_path_buf(),
            is_dir,
            size,
            disabled,
        });
    }

    nodes.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then(a.name.cmp(&b.name)));
    nodes
}

#[derive(Debug, Clone)]
pub struct ScriptDepthIssue {
    pub file_path: PathBuf,
    pub filename: String,
    pub depth: usize,
}

pub fn check_script_depth(mods_dir: &Path) -> Vec<ScriptDepthIssue> {
    let mut issues = Vec::new();
    if !mods_dir.exists() {
        return issues;
    }

    for entry in walk_user_mods(mods_dir, usize::MAX) {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        let fname = entry.file_name().to_string_lossy().to_string();
        if fname.to_lowercase().ends_with(".ts4script") {
            if let Ok(rel) = path.strip_prefix(mods_dir) {
                let depth = rel.components().count();
                if depth > 2 {
                    // Nested deeper than Mods/Subfolder/mod.ts4script (depth > 2 components)
                    issues.push(ScriptDepthIssue {
                        file_path: path.to_path_buf(),
                        filename: fname,
                        depth,
                    });
                }
            }
        }
    }

    issues
}

pub fn auto_fix_script_depth(
    issues: &[ScriptDepthIssue],
    mods_dir: &Path,
) -> Result<usize, io::Error> {
    let target_folder = mods_dir.join("00_Scripts_Corrigidos");
    fs::create_dir_all(&target_folder)?;

    let mut moved_count = 0;
    for issue in issues {
        if issue.file_path.exists() {
            let dest = unique_dest_path(&target_folder, &issue.filename);
            fs::rename(&issue.file_path, &dest)?;
            moved_count += 1;
        }
    }

    Ok(moved_count)
}

#[derive(Debug, Clone)]
pub struct DuplicateGroup {
    pub fast_hash: String,
    pub sha256_hash: String,
    pub file_paths: Vec<PathBuf>,
}

pub fn detect_duplicates(mods_dir: &Path) -> Vec<DuplicateGroup> {
    let mut fast_map: HashMap<String, Vec<PathBuf>> = HashMap::new();

    for entry in walk_user_mods(mods_dir, usize::MAX) {
        let path = entry.path();
        if path.is_file() && path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase() == "package" {
            if let Ok(hash) = fast_md5(path) {
                fast_map.entry(hash).or_default().push(path.to_path_buf());
            }
        }
    }

    let mut duplicate_groups = Vec::new();

    for (fast_hash, paths) in fast_map {
        if paths.len() > 1 {
            let mut sha_map: HashMap<String, Vec<PathBuf>> = HashMap::new();
            for p in paths {
                if let Ok(sha) = full_sha256(&p) {
                    sha_map.entry(sha).or_default().push(p);
                }
            }

            for (sha256_hash, file_paths) in sha_map {
                if file_paths.len() > 1 {
                    duplicate_groups.push(DuplicateGroup {
                        fast_hash: fast_hash.clone(),
                        sha256_hash,
                        file_paths,
                    });
                }
            }
        }
    }

    duplicate_groups
}

impl DuplicateGroup {
    /// O arquivo que fica. Escolhemos o de caminho mais curto: mods soltos na
    /// raiz de `Mods` costumam ser a cópia "oficial", e as duplicatas tendem a
    /// estar enterradas em subpastas de instalações antigas.
    pub fn keeper(&self) -> &PathBuf {
        self.file_paths
            .iter()
            .min_by_key(|p| (p.components().count(), p.as_os_str().len()))
            .unwrap_or(&self.file_paths[0])
    }

    /// As cópias redundantes — tudo menos o `keeper`.
    pub fn redundant(&self) -> Vec<PathBuf> {
        let keeper = self.keeper().clone();
        self.file_paths.iter().filter(|p| **p != keeper).cloned().collect()
    }
}

/// Resultado de uma remoção em lote.
#[derive(Debug, Default)]
pub struct RemovalOutcome {
    pub removed: usize,
    pub freed_bytes: u64,
    pub failures: Vec<(PathBuf, String)>,
}

/// Apaga os arquivos indicados, sempre por [`safe_remove_file`], que recusa
/// qualquer caminho fora das raízes permitidas. Um arquivo protegido não
/// interrompe o lote: vira uma entrada em `failures`.
pub fn remove_files(files: &[PathBuf], allowed_roots: &[PathBuf]) -> RemovalOutcome {
    let mut outcome = RemovalOutcome::default();

    for file in files {
        let size = fs::metadata(file).map(|m| m.len()).unwrap_or(0);
        match safe_remove_file(file, allowed_roots) {
            Ok(()) => {
                outcome.removed += 1;
                outcome.freed_bytes += size;
            }
            Err(e) => outcome.failures.push((file.clone(), e.to_string())),
        }
    }

    outcome
}

pub fn find_junk_files(mods_dir: &Path) -> Vec<PathBuf> {
    let junk_exts = ["txt", "url", "png", "jpg", "jpeg", "db"];
    let mut junk = Vec::new();

    for entry in walk_user_mods(mods_dir, usize::MAX) {
        let path = entry.path();
        if path.is_file() {
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
            if junk_exts.contains(&ext.as_str()) {
                junk.push(path.to_path_buf());
            }
        }
    }

    junk
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_script_depth_fixer() {
        let dir = tempdir().unwrap();
        let mods_dir = dir.path().join("Mods");
        let deep_folder = mods_dir.join("Category").join("SubCategory").join("DeepFolder");
        fs::create_dir_all(&deep_folder).unwrap();

        let script_file = deep_folder.join("nested_mod.ts4script");
        fs::write(&script_file, b"dummy script content").unwrap();

        let issues = check_script_depth(&mods_dir);
        assert_eq!(issues.len(), 1);

        let fixed = auto_fix_script_depth(&issues, &mods_dir).unwrap();
        assert_eq!(fixed, 1);

        let corrected_file = mods_dir.join("00_Scripts_Corrigidos").join("nested_mod.ts4script");
        assert!(corrected_file.exists());
    }
}
