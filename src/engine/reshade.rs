use crate::core::safety::{safe_remove_file, unique_dest_path};
use crate::engine::installer::extract_archive_to_staging;
use crate::engine::tray::full_sha256;
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum ReshadeError {
    #[error("Diretório Game/Bin não encontrado ou inválido: '{0}'")]
    InvalidGameBinPath(String),
    #[error("TS4_x64.exe não encontrado em '{0}'")]
    ExecutableNotFound(String),
    #[error("Falha ao baixar ReShade Setup: {0}")]
    DownloadFailed(String),
    #[error("Extração do ReShade64.dll falhou: {0}")]
    ExtractionFailed(String),
    #[error("DLL extraída não contém a assinatura válida do ReShade")]
    InvalidReShadeBinary,
    #[error("Erro de I/O: {0}")]
    Io(#[from] io::Error),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LinuxEnvType {
    Steam,
    LutrisBottles,
    Generic,
}

pub fn detect_environment(game_bin_path: &Path) -> (LinuxEnvType, String) {
    let lower_path = game_bin_path.to_string_lossy().to_lowercase();
    if lower_path.contains("steamapps") {
        (
            LinuxEnvType::Steam,
            "WINEDLLOVERRIDES=\"dxgi=n,b\" %command%".to_string(),
        )
    } else if lower_path.contains("lutris") || lower_path.contains("bottles") {
        (
            LinuxEnvType::LutrisBottles,
            "Configure WINEDLLOVERRIDES=\"dxgi=n,b\" nas variáveis do prefixo Wine.".to_string(),
        )
    } else {
        (
            LinuxEnvType::Generic,
            "Configure WINEDLLOVERRIDES=\"dxgi=n,b\" no seu Wine/Proton.".to_string(),
        )
    }
}

pub fn is_reshade_dll(dll_path: &Path) -> bool {
    if !dll_path.exists() {
        return false;
    }
    if let Ok(mut file) = File::open(dll_path) {
        let mut buffer = vec![0u8; 4 * 1024 * 1024];
        if let Ok(n) = file.read(&mut buffer) {
            let data = &buffer[..n];
            return data.windows(7).any(|w| w == b"ReShade")
                || data.windows(14).any(|w| w == b"R\x00e\x00S\x00h\x00a\x00d\x00e\x00");
        }
    }
    false
}

fn next_backup_path(dll_path: &Path) -> PathBuf {
    let mut index = 1;
    loop {
        let candidate = dll_path.with_extension(format!("dll.backup.{}", index));
        if !candidate.exists() {
            return candidate;
        }
        index += 1;
    }
}

fn backup_existing_dll(dll_path: &Path) -> Result<Option<PathBuf>, io::Error> {
    if !dll_path.exists() {
        return Ok(None);
    }
    if is_reshade_dll(dll_path) {
        return Ok(None);
    }

    let backup_path = next_backup_path(dll_path);
    fs::copy(dll_path, &backup_path)?;
    Ok(Some(backup_path))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReshadeManifest {
    pub installed_dll: String,
    pub dll_sha256: String,
    pub installed_at: String,
    pub presets: Vec<String>,
}

pub fn download_reshade_setup(dest_path: &Path) -> Result<(), ReshadeError> {
    let url = "https://reshade.me/downloads/ReShade_Setup_5.9.2.exe";
    let response = reqwest::blocking::get(url).map_err(|e| ReshadeError::DownloadFailed(e.to_string()))?;
    if !response.status().is_success() {
        return Err(ReshadeError::DownloadFailed(format!("Status HTTP: {}", response.status())));
    }
    let bytes = response.bytes().map_err(|e| ReshadeError::DownloadFailed(e.to_string()))?;
    fs::write(dest_path, bytes)?;
    Ok(())
}

pub fn install_reshade(
    game_bin_path: &Path,
    custom_setup_exe: Option<&Path>,
    mode_d3d11: bool,
) -> Result<(), ReshadeError> {
    if !game_bin_path.exists() {
        return Err(ReshadeError::InvalidGameBinPath(game_bin_path.display().to_string()));
    }

    let exe_64 = game_bin_path.join("TS4_x64.exe");
    if !exe_64.exists() {
        return Err(ReshadeError::ExecutableNotFound(game_bin_path.display().to_string()));
    }

    let temp_dir = tempfile::tempdir()?;
    let setup_exe_path = match custom_setup_exe {
        Some(p) if p.exists() => p.to_path_buf(),
        _ => {
            let downloaded = temp_dir.path().join("ReShade_Setup_5.9.2.exe");
            download_reshade_setup(&downloaded)?;
            downloaded
        }
    };

    let staging = temp_dir.path().join("staging");
    extract_archive_to_staging(&setup_exe_path, &staging)
        .map_err(|e| ReshadeError::ExtractionFailed(e.to_string()))?;

    let reshade_dll_candidate = staging.join("ReShade64.dll");
    let dll_source = if reshade_dll_candidate.exists() {
        reshade_dll_candidate
    } else {
        // Search recursively for ReShade64.dll
        let mut found = None;
        for entry in walkdir::WalkDir::new(&staging).into_iter().filter_map(|e| e.ok()) {
            if entry.file_name().to_string_lossy().to_lowercase() == "reshade64.dll" {
                found = Some(entry.path().to_path_buf());
                break;
            }
        }
        found.ok_or_else(|| ReshadeError::ExtractionFailed("ReShade64.dll não encontrado no instalador".to_string()))?
    };

    if !is_reshade_dll(&dll_source) {
        return Err(ReshadeError::InvalidReShadeBinary);
    }

    let target_dll_name = if mode_d3d11 { "d3d11.dll" } else { "dxgi.dll" };
    let target_dll_path = game_bin_path.join(target_dll_name);

    backup_existing_dll(&target_dll_path)?;

    fs::copy(&dll_source, &target_dll_path)?;

    let sha256 = full_sha256(&target_dll_path)?;
    let manifest = ReshadeManifest {
        installed_dll: target_dll_name.to_string(),
        dll_sha256: sha256,
        installed_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            .to_string(),
        presets: Vec::new(),
    };

    let manifest_path = game_bin_path.join(".s4suite_reshade_manifest.json");
    if let Ok(json) = serde_json::to_string_pretty(&manifest) {
        let _ = fs::write(manifest_path, json);
    }

    Ok(())
}

pub fn uninstall_reshade(game_bin_path: &Path, allowed_roots: &[PathBuf]) -> Result<(), ReshadeError> {
    for dll_name in &["dxgi.dll", "d3d11.dll", "d3d9.dll"] {
        let dll_path = game_bin_path.join(dll_name);
        if is_reshade_dll(&dll_path) {
            let _ = safe_remove_file(&dll_path, allowed_roots);

            // Restore latest backup
            let mut backups = Vec::new();
            if let Ok(entries) = fs::read_dir(game_bin_path) {
                for entry in entries.filter_map(|e| e.ok()) {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if name.starts_with(&format!("{}.backup.", dll_name)) || name == format!("{}.backup", dll_name) {
                        backups.push(entry.path());
                    }
                }
            }
            backups.sort_by_key(|p| fs::metadata(p).and_then(|m| m.modified()).ok());
            if let Some(latest) = backups.last() {
                let _ = fs::rename(latest, &dll_path);
            }
        }
    }

    let manifest_path = game_bin_path.join(".s4suite_reshade_manifest.json");
    if manifest_path.exists() {
        let _ = safe_remove_file(&manifest_path, allowed_roots);
    }

    Ok(())
}

pub fn install_shader_preset(
    game_bin_path: &Path,
    preset_archive: &Path,
) -> Result<(), ReshadeError> {
    let staging = tempfile::tempdir()?;
    extract_archive_to_staging(preset_archive, staging.path())
        .map_err(|e| ReshadeError::ExtractionFailed(e.to_string()))?;

    let presets_dir = game_bin_path.join("presets");
    fs::create_dir_all(&presets_dir)?;

    for entry in walkdir::WalkDir::new(staging.path()).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_file() {
            let fname = entry.file_name().to_string_lossy();
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
            if ext == "ini" || ext == "txt" {
                let dest = unique_dest_path(&presets_dir, &fname);
                let _ = fs::copy(path, dest);
            }
        }
    }

    Ok(())
}
