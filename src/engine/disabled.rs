use crate::core::safety::{is_path_inside, unique_dest_path};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisabledModEntry {
    pub disabled_path: String,
    pub original_path: String,
    pub disabled_at: String,
    pub reason: String,
    pub note: String,
}

pub struct DisabledManager {
    manifest_path: PathBuf,
}

impl DisabledManager {
    pub fn new(config_dir: &Path) -> Self {
        Self {
            manifest_path: config_dir.join("disabled_mods.json"),
        }
    }

    pub fn load_manifest(&self) -> HashMap<String, DisabledModEntry> {
        if !self.manifest_path.exists() {
            return HashMap::new();
        }
        match fs::read_to_string(&self.manifest_path) {
            Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
            Err(_) => HashMap::new(),
        }
    }

    pub fn save_manifest(&self, data: &HashMap<String, DisabledModEntry>) -> Result<(), io::Error> {
        if let Some(parent) = self.manifest_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp_path = self.manifest_path.with_extension("json.tmp");
        {
            let mut file = File::create(&tmp_path)?;
            let json = serde_json::to_string_pretty(data)?;
            file.write_all(json.as_bytes())?;
            file.flush()?;
            file.sync_all()?;
        }
        fs::rename(tmp_path, &self.manifest_path)?;
        Ok(())
    }

    pub fn disable_mod(
        &self,
        path: &Path,
        mods_dir: &Path,
        reason: &str,
        note: &str,
        _allowed_roots: &[PathBuf],
    ) -> Result<PathBuf, Box<dyn std::error::Error + Send + Sync>> {
        if !path.exists() || !path.is_file() {
            return Err("Arquivo de mod não existe".into());
        }

        if !is_path_inside(path, mods_dir) && dunce::canonicalize(path)? != dunce::canonicalize(mods_dir)? {
            return Err("Operação bloqueada: mod fora da pasta Mods".into());
        }

        let fname = path.file_name().unwrap_or_default().to_string_lossy();
        let disabled_name = if fname.ends_with(".disabled") {
            fname.to_string()
        } else {
            format!("{}.disabled", fname)
        };

        let parent = path.parent().unwrap_or(mods_dir);
        let dest_disabled_path = unique_dest_path(parent, &disabled_name);

        fs::rename(path, &dest_disabled_path)?;

        let mut manifest = self.load_manifest();
        let entry = DisabledModEntry {
            disabled_path: dest_disabled_path.display().to_string(),
            original_path: path.display().to_string(),
            disabled_at: chrono_like_now(),
            reason: reason.to_string(),
            note: note.to_string(),
        };

        manifest.insert(dest_disabled_path.display().to_string(), entry);
        self.save_manifest(&manifest)?;

        Ok(dest_disabled_path)
    }

    pub fn enable_mod(
        &self,
        disabled_path: &Path,
        mods_dir: &Path,
    ) -> Result<PathBuf, Box<dyn std::error::Error + Send + Sync>> {
        if !disabled_path.exists() {
            return Err("Arquivo .disabled não foi encontrado".into());
        }

        let mut manifest = self.load_manifest();
        let path_str = disabled_path.display().to_string();

        let original_target = if let Some(entry) = manifest.get(&path_str) {
            PathBuf::from(&entry.original_path)
        } else {
            let fname = disabled_path.file_name().unwrap_or_default().to_string_lossy();
            let clean_name = fname.trim_end_matches(".disabled");
            disabled_path.parent().unwrap_or(mods_dir).join(clean_name)
        };

        let target_dest = unique_dest_path(original_target.parent().unwrap_or(mods_dir), original_target.file_name().unwrap_or_default().to_str().unwrap_or("mod.package"));

        fs::rename(disabled_path, &target_dest)?;

        manifest.remove(&path_str);
        self.save_manifest(&manifest)?;

        Ok(target_dest)
    }
}

fn chrono_like_now() -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    format!("{}", now.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_disabled_mod_manifest() {
        let dir = tempdir().unwrap();
        let config_dir = dir.path().join("config");
        let mods_dir = dir.path().join("Mods");
        fs::create_dir_all(&mods_dir).unwrap();

        let mod_file = mods_dir.join("test_mod.package");
        fs::write(&mod_file, b"sample package content").unwrap();

        let mgr = DisabledManager::new(&config_dir);
        let disabled_path = mgr
            .disable_mod(&mod_file, &mods_dir, "manual", "testing", &[mods_dir.clone()])
            .unwrap();

        assert!(disabled_path.exists());
        assert!(disabled_path.to_string_lossy().ends_with(".disabled"));

        let manifest = mgr.load_manifest();
        assert_eq!(manifest.len(), 1);

        let restored_path = mgr.enable_mod(&disabled_path, &mods_dir).unwrap();
        assert!(restored_path.exists());
        assert!(!restored_path.to_string_lossy().ends_with(".disabled"));

        let manifest_after = mgr.load_manifest();
        assert_eq!(manifest_after.len(), 0);
    }
}
