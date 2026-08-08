#!/usr/bin/env python3
import os
import sys
import urllib.request
import re
import shutil
import tempfile
import subprocess
import json
import hashlib
from s4common import ConfigManager, command_exists, safe_delete_folder, safe_remove, unique_dest_path, is_path_inside
from s4translator import t

# ==================================================================================
# S4 SUITE: Reshade Linux Engine V2
# Features:
# - Auto-download with better error handling.
# - Linux Environment Detection (Steam/Lutris).
# - Proactive DXGI injection.
# ==================================================================================

def get_latest_reshade_url():
    # Versão 5.9.2 é a recomendada (Legacy Stable) para evitar tela preta no Linux/DXVK
    return "https://reshade.me/downloads/ReShade_Setup_5.9.2.exe"

def resource_path(relative_path):
    try:
        base_path = sys._MEIPASS
    except Exception:
        base_path = os.path.dirname(__file__)
    return os.path.join(base_path, relative_path)

def detect_environment(game_bin_path):
    """Detecta se o jogo está rodando via Steam ou Lutris."""
    if "steamapps" in game_bin_path.lower():
        return "STEAM", t("Adicione em Opções de Inicialização: WINEDLLOVERRIDES=\"dxgi=n,b\" %command%")
    
    # Checar se há evidências de Lutris ou Bottle
    if "lutris" in game_bin_path.lower() or "bottles" in game_bin_path.lower():
        return "LUTRIS/BOTTLES", t("Configure WINEDLLOVERRIDES=\"dxgi=n,b\" nas variáveis de ambiente do prefixo.")
        
    return "GENERIC", t("Certifique-se de configurar WINEDLLOVERRIDES=\"dxgi=n,b\" no seu Wine/Proton.")

def _is_reshade_dll(path):
    try:
        with open(path, "rb") as dll_file:
            data = dll_file.read(4 * 1024 * 1024)
        return b"ReShade" in data or b"R\x00e\x00S\x00h\x00a\x00d\x00e\x00" in data
    except Exception:
        return False

def _next_backup_path(path):
    index = 1
    while True:
        backup_path = f"{path}.backup.{index}"
        if not os.path.exists(backup_path):
            return backup_path
        index += 1

def _backup_existing_dll(path, log):
    if not os.path.exists(path):
        return None
    if _is_reshade_dll(path):
        log(t("♻️ DLL ReShade existente será substituída sem sobrescrever backups originais."))
        return None

    backup_path = _next_backup_path(path)
    shutil.copy2(path, backup_path)
    log(t("🛡️ Backup criado: {}").format(os.path.basename(backup_path)))
    return backup_path

def _restore_latest_backup(path):
    legacy_backup = path + ".backup"
    candidates = []
    if os.path.exists(legacy_backup):
        candidates.append(legacy_backup)

    parent = os.path.dirname(path)
    prefix = os.path.basename(path) + ".backup."
    for name in os.listdir(parent):
        if name.startswith(prefix):
            candidates.append(os.path.join(parent, name))

    if not candidates:
        return False

    candidates.sort(key=lambda p: os.path.getmtime(p), reverse=True)
    shutil.move(candidates[0], path)
    return True

def _manifest_path(game_bin_path):
    return os.path.join(game_bin_path, ".s4suite_reshade_manifest.json")

def _file_sha256(path):
    digest = hashlib.sha256()
    with open(path, "rb") as src:
        for chunk in iter(lambda: src.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()

def _write_manifest(game_bin_path, files):
    manifest = _read_manifest(game_bin_path)
    manifest["files"] = files
    with open(_manifest_path(game_bin_path), "w", encoding="utf-8") as manifest_file:
        json.dump(manifest, manifest_file, indent=2)

def _read_manifest(game_bin_path):
    try:
        with open(_manifest_path(game_bin_path), "r", encoding="utf-8") as manifest_file:
            return json.load(manifest_file)
    except Exception:
        return {"files": {}, "presets": {}}

def _write_preset_manifest(game_bin_path, preset_files):
    manifest = _read_manifest(game_bin_path)
    presets = manifest.get("presets", {})
    presets.update(preset_files)
    manifest["presets"] = presets
    with open(_manifest_path(game_bin_path), "w", encoding="utf-8") as manifest_file:
        json.dump(manifest, manifest_file, indent=2)

def _relative_to_game_bin(game_bin_path, path):
    return os.path.relpath(path, game_bin_path).replace(os.sep, "/")

def _validate_extracted_path(path, temp_dir):
    real_path = os.path.realpath(path)
    real_temp = os.path.realpath(temp_dir)
    if real_path == real_temp or is_path_inside(real_path, real_temp):
        return real_path
    raise ValueError(t("Arquivo extraído fora da pasta temporária. Preset bloqueado: {}").format(path))

def _copy_file_atomically(src, dest_dir, filename):
    os.makedirs(dest_dir, exist_ok=True)
    dest = os.path.join(dest_dir, filename)
    if os.path.exists(dest):
        dest = unique_dest_path(dest_dir, filename)
    tmp_dest = dest + ".s4suite_tmp"
    try:
        shutil.copy2(src, tmp_dest)
        file_hash = _file_sha256(tmp_dest)
        os.replace(tmp_dest, dest)
        return dest, file_hash
    finally:
        if os.path.exists(tmp_dest):
            try:
                os.remove(tmp_dest)
            except OSError:
                pass

def install_reshade(game_bin_path, inject_name="dxgi.dll", log_callback=None):
    def log(msg):
        if log_callback: log_callback(msg)
        else: print(msg)

    if not os.path.exists(os.path.join(game_bin_path, "TS4_x64.exe")):
        return False, t("TS4_x64.exe não encontrado. Selecione a pasta Game/Bin.")

    env_type, env_msg = detect_environment(game_bin_path)
    
    try:
        if not command_exists('7z'):
            return False, t("Dependência ausente: instale o pacote 'p7zip'/'7zip' para extrair o instalador do ReShade.")

        with tempfile.TemporaryDirectory() as temp_dir:
            installer_path = os.path.join(temp_dir, "reshade.exe")
            bundled_installer = resource_path("reshade.exe")

            if os.path.exists(bundled_installer):
                log(t("📦 Usando instalador ReShade local."))
                shutil.copy2(bundled_installer, installer_path)
            else:
                reshade_url = get_latest_reshade_url()
                log(t("🌐 Baixando Reshade 5.9.2 (Estável para Linux)..."))
                req = urllib.request.Request(reshade_url, headers={'User-Agent': 'Mozilla/5.0'})
                with urllib.request.urlopen(req, timeout=30) as response, open(installer_path, 'wb') as out_file:
                    shutil.copyfileobj(response, out_file)
            
            log(t("📦 Extraindo ReShade64.dll..."))
            res = subprocess.run(['7z', 'e', installer_path, f'-o{temp_dir}', 'ReShade64.dll', '-y'], capture_output=True, text=True)
            if res.returncode != 0:
                return False, res.stderr.strip() or res.stdout.strip() or t("Falha na extração.")
            
            dll_src = os.path.join(temp_dir, "ReShade64.dll")
            if not os.path.exists(dll_src): return False, t("Falha na extração.")

            log(t("💉 Injetando arquivo como {}...").format(inject_name))
            dest_dll = os.path.join(game_bin_path, inject_name)
            tmp_dest_dll = dest_dll + ".s4suite_tmp"
            _backup_existing_dll(dest_dll, log)
            try:
                shutil.copy2(dll_src, tmp_dest_dll)
                dll_hash = _file_sha256(tmp_dest_dll)
                os.replace(tmp_dest_dll, dest_dll)
            finally:
                if os.path.exists(tmp_dest_dll):
                    try:
                        os.remove(tmp_dest_dll)
                    except OSError:
                        pass
            manifest_files = {
                inject_name: dll_hash
            }

            # Cleanup conflitos
            other_dlls = ["dxgi.dll", "d3d11.dll", "d3d9.dll"]
            for d in other_dlls:
                if d != inject_name:
                    path = os.path.join(game_bin_path, d)
                    if os.path.exists(path) and _is_reshade_dll(path):
                        safe_remove(path)
                    elif os.path.exists(path):
                        log(t("🛡️ DLL preservada por não parecer ReShade: {}").format(d))

            _write_manifest(game_bin_path, manifest_files)
            return True, t("✅ Reshade instalado ({} | {}).\n\n⚠️ IMPORTANTE:\n{}").format(env_type, inject_name, env_msg)

    except Exception as e:
        return False, t("Erro: {}").format(e)

def uninstall_reshade(game_bin_path, log_callback=None):
    def log(msg):
        if log_callback: log_callback(msg)
        else: print(msg)

    try:
        manifest = _read_manifest(game_bin_path)
        manifest_files = manifest.get("files", {})
        for f in ["dxgi.dll", "d3d11.dll", "d3d9.dll"]:
            path = os.path.join(game_bin_path, f)
            expected_hash = manifest_files.get(f)
            if os.path.exists(path) and expected_hash:
                try:
                    if _file_sha256(path) == expected_hash:
                        safe_remove(path)
                    else:
                        log(t("🛡️ DLL preservada porque difere do manifesto: {}").format(f))
                        continue
                except Exception:
                    log(t("🛡️ DLL preservada por falha ao validar hash: {}").format(f))
                    continue
            elif os.path.exists(path) and _is_reshade_dll(path):
                safe_remove(path)
            elif os.path.exists(path):
                log(t("🛡️ DLL preservada por não parecer ReShade: {}").format(f))
                continue
            _restore_latest_backup(path)

        for rel_path, expected_hash in manifest.get("presets", {}).items():
            path = os.path.join(game_bin_path, rel_path)
            if not os.path.exists(path):
                continue
            try:
                if _file_sha256(path) == expected_hash:
                    safe_remove(path)
                else:
                    log(t("🛡️ Preset preservado porque difere do manifesto: {}").format(rel_path))
            except Exception:
                log(t("🛡️ Preset preservado por falha ao validar hash: {}").format(rel_path))

        for f in ["ReShade.log"]:
            path = os.path.join(game_bin_path, f)
            if os.path.exists(path): safe_remove(path)
            
        shaders = os.path.join(game_bin_path, "reshade-shaders")
        if os.path.exists(shaders):
            try:
                for root, _, _ in os.walk(shaders, topdown=False):
                    if root != shaders and not os.listdir(root):
                        os.rmdir(root)
                if not os.listdir(shaders):
                    safe_delete_folder(shaders)
                else:
                    log(t("🛡️ Pasta reshade-shaders preservada porque ainda contém arquivos não registrados."))
            except Exception as e:
                log(t("🛡️ Não foi possível limpar pastas vazias do ReShade: {}").format(e))
        manifest_file = _manifest_path(game_bin_path)
        if os.path.exists(manifest_file): safe_remove(manifest_file)
        return True, t("Reshade removido.")
    except Exception as e: return False, str(e)

def install_preset(zip_path, game_bin_path, log_callback=None):
    try:
        if not command_exists('7z'):
            return False, t("Dependência ausente: instale o pacote 'p7zip'/'7zip' para extrair presets.")

        with tempfile.TemporaryDirectory() as temp_dir:
            res = subprocess.run(['7z', 'x', zip_path, f'-o{temp_dir}', '-y'], capture_output=True, text=True)
            if res.returncode != 0:
                return False, res.stderr.strip() or res.stdout.strip() or t("Falha ao extrair preset.")
            installed_files = {}
            for root, _, files in os.walk(temp_dir):
                for f in files:
                    src = _validate_extracted_path(os.path.join(root, f), temp_dir)
                    if f.lower().endswith('.ini'):
                        dest, file_hash = _copy_file_atomically(src, game_bin_path, f)
                        installed_files[_relative_to_game_bin(game_bin_path, dest)] = file_hash
                    elif f.lower().endswith(('.fx', '.png')):
                        folder = "Textures" if f.lower().endswith('.png') else "Shaders"
                        dest = os.path.join(game_bin_path, "reshade-shaders", folder)
                        dest_file, file_hash = _copy_file_atomically(src, dest, f)
                        installed_files[_relative_to_game_bin(game_bin_path, dest_file)] = file_hash
            if installed_files:
                _write_preset_manifest(game_bin_path, installed_files)
            return True, t("Preset instalado.")
    except Exception as e: return False, str(e)
