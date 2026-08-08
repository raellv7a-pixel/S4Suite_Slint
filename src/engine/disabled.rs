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

/// Um mod desativado, como aparece no painel de desativados.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisabledMod {
    pub path: PathBuf,
    /// Nome sem o sufixo `.disabled`, que é o que o usuário reconhece.
    pub name: String,
    pub reason: String,
    pub note: String,
    /// Está no manifesto mas sumiu do disco — o usuário apagou por fora.
    pub missing: bool,
}

impl DisabledManager {
    /// Lista os mods desativados dentro de `mods_dir`.
    ///
    /// A fonte da verdade é o disco, não o manifesto: um `.disabled` renomeado
    /// à mão por fora do app precisa aparecer aqui, senão fica invisível e sem
    /// como ser reativado. O manifesto só enriquece o que foi encontrado, e as
    /// entradas órfãs entram marcadas como `missing` para poderem ser limpas.
    pub fn list_disabled(&self, mods_dir: &Path) -> Vec<DisabledMod> {
        let manifest = self.load_manifest();
        let mut items = Vec::new();
        let mut seen: Vec<String> = Vec::new();

        for entry in crate::engine::walk_user_mods(mods_dir, usize::MAX) {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let fname = entry.file_name().to_string_lossy().to_string();
            if !fname.ends_with(".disabled") {
                continue;
            }

            let key = path.display().to_string();
            let record = manifest.get(&key);
            seen.push(key);

            items.push(DisabledMod {
                path: path.to_path_buf(),
                name: fname.trim_end_matches(".disabled").to_string(),
                reason: record.map(|e| e.reason.clone()).unwrap_or_else(|| "manual".to_string()),
                note: record.map(|e| e.note.clone()).unwrap_or_default(),
                missing: false,
            });
        }

        for (key, record) in &manifest {
            if seen.iter().any(|s| s == key) {
                continue;
            }
            let path = PathBuf::from(key);
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().trim_end_matches(".disabled").to_string())
                .unwrap_or_else(|| key.clone());
            items.push(DisabledMod {
                path,
                name,
                reason: record.reason.clone(),
                note: record.note.clone(),
                missing: true,
            });
        }

        items.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        items
    }

    /// Remove do manifesto as entradas cujo arquivo não existe mais.
    pub fn prune_missing(&self) -> Result<usize, io::Error> {
        let mut manifest = self.load_manifest();
        let before = manifest.len();
        manifest.retain(|key, _| Path::new(key).exists());
        let removed = before - manifest.len();
        if removed > 0 {
            self.save_manifest(&manifest)?;
        }
        Ok(removed)
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
