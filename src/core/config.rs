use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub sims4_path: Option<PathBuf>,
    pub merge_limit_gb: f64,
    pub language: String,
    pub theme: String,
    pub custom_categories: Vec<(String, PathBuf)>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            sims4_path: None,
            merge_limit_gb: 1.0,
            language: "pt".to_string(),
            theme: "Sims Green".to_string(),
            custom_categories: Vec::new(),
        }
    }
}

pub struct ConfigManager {
    config_dir: PathBuf,
    config_file: PathBuf,
}

impl ConfigManager {
    pub fn new() -> Result<Self, io::Error> {
        let config_dir = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("~/.config"))
            .join("s4suite");
        Self::with_dir(config_dir)
    }

    /// Constrói um gerenciador apontando para um diretório específico.
    ///
    /// Os testes precisam disso: com [`new`](Self::new) eles compartilhariam o
    /// arquivo de config real da máquina, corrompendo-o e disputando o mesmo
    /// `rename` entre threads.
    pub fn with_dir(config_dir: PathBuf) -> Result<Self, io::Error> {
        fs::create_dir_all(&config_dir)?;
        let config_file = config_dir.join("config.json");
        Ok(Self {
            config_dir,
            config_file,
        })
    }

    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    pub fn config_file(&self) -> &Path {
        &self.config_file
    }

    pub fn load(&self) -> Config {
        if !self.config_file.exists() {
            let mut cfg = Config::default();
            cfg.sims4_path = self.auto_detect_sims4_path();
            let _ = self.save(&cfg);
            return cfg;
        }

        match fs::read_to_string(&self.config_file) {
            Ok(content) => match serde_json::from_str::<Config>(&content) {
                Ok(mut cfg) => {
                    if cfg.sims4_path.is_none() {
                        cfg.sims4_path = self.auto_detect_sims4_path();
                    }
                    cfg
                }
                Err(e) => {
                    eprintln!("Erro ao carregar config.json: {}. Backup corrompido criado.", e);
                    let corrupt_file = self.config_dir.join("config.json.corrupt");
                    let _ = fs::copy(&self.config_file, corrupt_file);
                    let mut cfg = Config::default();
                    cfg.sims4_path = self.auto_detect_sims4_path();
                    let _ = self.save(&cfg);
                    cfg
                }
            },
            Err(_) => Config::default(),
        }
    }

    pub fn save(&self, config: &Config) -> Result<(), io::Error> {
        fs::create_dir_all(&self.config_dir)?;
        let tmp_file_path = self.config_dir.join("config.json.tmp");
        {
            let mut file = File::create(&tmp_file_path)?;
            let json_data = serde_json::to_string_pretty(config)?;
            file.write_all(json_data.as_bytes())?;
            file.flush()?;
            file.sync_all()?;
        }
        fs::rename(tmp_file_path, &self.config_file)?;
        Ok(())
    }

    pub fn auto_detect_sims4_path(&self) -> Option<PathBuf> {
        let home = dirs::home_dir()?;
        let mut possible_paths = vec![
            // Faugus Launcher
            home.join("Faugus/the-sims-4/drive_c/users/steamuser/Documents/Electronic Arts/The Sims 4"),
            // Steam Flatpak
            home.join(".var/app/com.valvesoftware.Steam/.local/share/Steam/steamapps/compatdata/1222670/pfx/drive_c/users/steamuser/Documents/Electronic Arts/The Sims 4"),
            // Steam Nativo
            home.join(".steam/steam/steamapps/compatdata/1222670/pfx/drive_c/users/steamuser/Documents/Electronic Arts/The Sims 4"),
            home.join(".local/share/Steam/steamapps/compatdata/1222670/pfx/drive_c/users/steamuser/Documents/Electronic Arts/The Sims 4"),
            // Documentos Nativo / Wine sem prefixo
            home.join("Documents/Electronic Arts/The Sims 4"),
            home.join("Documentos/Electronic Arts/The Sims 4"),
        ];

        // Heroic Launcher
        let heroic_pattern = home.join("Games/Heroic/Prefixes/*/drive_c/users/*/Documents/Electronic Arts/The Sims 4");
        if let Ok(entries) = glob::glob(&heroic_pattern.to_string_lossy()) {
            possible_paths.extend(entries.filter_map(|e| e.ok()));
        }

        // Lutris/Wine Generico
        let wine_pattern = home.join(".wine/drive_c/users/*/Documents/Electronic Arts/The Sims 4");
        if let Ok(entries) = glob::glob(&wine_pattern.to_string_lossy()) {
            possible_paths.extend(entries.filter_map(|e| e.ok()));
        }

        for path in possible_paths {
            if path.exists() && path.is_dir() {
                return Some(path);
            }
        }

        None
    }
}
pub fn get_mods_dir(sims4_path: &Path) -> PathBuf {
    sims4_path.join("Mods")
}

pub fn get_tray_dir(sims4_path: &Path) -> PathBuf {
    sims4_path.join("Tray")
}

pub fn get_saves_dir(sims4_path: &Path) -> PathBuf {
    sims4_path.join("saves")
}
