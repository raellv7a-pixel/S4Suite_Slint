#!/usr/bin/env python3
import os
import json
import shutil
import tempfile
import subprocess

# ==================================================================================
# S4 SUITE: COMMON UTILS & SHARED LOGIC
# ==================================================================================

CONFIG_DIR = os.path.expanduser("~/.config/s4suite")
CONFIG_FILE = os.path.join(CONFIG_DIR, "config.json")
DB_PATH = os.path.join(CONFIG_DIR, "mods_cache.db")

def command_exists(command):
    return shutil.which(command) is not None

def get_documents_dir():
    try:
        res = subprocess.run(["xdg-user-dir", "DOCUMENTS"], capture_output=True, text=True)
        path = res.stdout.strip()
        if res.returncode == 0 and path and os.path.isdir(path):
            return path
    except Exception:
        pass
    for candidate in (os.path.expanduser("~/Documents"), os.path.expanduser("~/Documentos")):
        if os.path.isdir(candidate):
            return candidate
    return os.path.expanduser("~")

class ConfigManager:
    """Centraliza a leitura e escrita de configurações de forma segura."""
    _cache = None

    @classmethod
    def _default_config(cls):
        return {"sims4_path": None}

    @classmethod
    def load(cls, force_reload=False):
        if cls._cache is not None and not force_reload:
            return dict(cls._cache)

        if not os.path.exists(CONFIG_DIR):
            os.makedirs(CONFIG_DIR)
            
        if os.path.exists(CONFIG_FILE):
            try:
                with open(CONFIG_FILE, 'r') as f:
                    cls._cache = json.load(f)
                    return dict(cls._cache)
            except json.JSONDecodeError:
                corrupt_path = CONFIG_FILE + ".corrupt"
                try:
                    shutil.copy2(CONFIG_FILE, corrupt_path)
                except IOError:
                    pass
            except IOError:
                pass
        cls._cache = cls._default_config()
        return dict(cls._cache)

    @classmethod
    def save(cls, data):
        if not os.path.exists(CONFIG_DIR):
            os.makedirs(CONFIG_DIR)
        tmp_path = CONFIG_FILE + ".tmp"
        try:
            with open(tmp_path, 'w') as f:
                json.dump(data, f, indent=4)
                f.flush()
                os.fsync(f.fileno())
            os.replace(tmp_path, CONFIG_FILE)
            cls._cache = dict(data)
            return True
        except IOError:
            if os.path.exists(tmp_path):
                try:
                    os.remove(tmp_path)
                except IOError:
                    pass
            return False

    @classmethod
    def get(cls, key, default=None):
        config = cls.load()
        return config.get(key, default)

    @classmethod
    def set(cls, key, value):
        config = cls.load()
        config[key] = value
        return cls.save(config)

def get_mods_dir():
    sims_path = ConfigManager.get("sims4_path")
    if sims_path and os.path.exists(sims_path):
        return os.path.join(sims_path, "Mods")
    return None

def get_tray_dir():
    sims_path = ConfigManager.get("sims4_path")
    if sims_path and os.path.exists(sims_path):
        return os.path.join(sims_path, "Tray")
    return None



def is_path_inside(child_path, parent_path):
    if not child_path or not parent_path:
        return False
    try:
        child_abs = os.path.realpath(child_path)
        parent_abs = os.path.realpath(parent_path)
        return os.path.commonpath([child_abs, parent_abs]) == parent_abs and child_abs != parent_abs
    except ValueError:
        return False

def unique_dest_path(dest_dir, filename):
    os.makedirs(dest_dir, exist_ok=True)
    base, ext = os.path.splitext(filename)
    candidate = os.path.join(dest_dir, filename)
    counter = 1
    while os.path.exists(candidate):
        candidate = os.path.join(dest_dir, f"{base}_{counter}{ext}")
        counter += 1
    return candidate

def get_allowed_delete_roots():
    roots = [tempfile.gettempdir()]
    sims_path = ConfigManager.get("sims4_path")
    if sims_path:
        roots.extend([
            os.path.join(sims_path, "Mods"),
            os.path.join(sims_path, "Tray"),
            os.path.join(sims_path, "Game", "Bin"),
            os.path.join(sims_path, "cache"),
            os.path.join(sims_path, "cachestr"),
            os.path.join(sims_path, "onlinethumbnailcache"),
        ])

    return [os.path.realpath(root) for root in roots if root]

def get_protected_delete_paths():
    protected = ["/", "/home", "/usr", "/etc", "/var", "/bin", "/opt",
                 "/boot", "/sbin", "/lib", "/lib64", "/dev", "/proc", "/sys",
                 "/tmp", "/root", os.path.expanduser("~")]
    sims_path = ConfigManager.get("sims4_path")
    if sims_path:
        mods_path = os.path.join(sims_path, "Mods")
        protected.extend([
            sims_path,
            mods_path,
            os.path.join(sims_path, "Tray"),
            os.path.join(mods_path, "00_Triagem_Novos"),
        ])

    return [os.path.realpath(path) for path in protected if path]

def get_allowed_delete_files():
    sims_path = ConfigManager.get("sims4_path")
    if not sims_path:
        return []
    filenames = ["localthumbcache.package", "avatarcache.package", "spotlight_pt-br.package"]
    return [os.path.realpath(os.path.join(sims_path, name)) for name in filenames]

def is_safe_to_delete(target_path):
    if not target_path:
        return False
        
    target_abs = os.path.realpath(target_path)
    if target_abs in get_protected_delete_paths():
        return False
    if target_abs in get_allowed_delete_files():
        return True

    return any(is_path_inside(target_abs, root) for root in get_allowed_delete_roots())

def safe_delete_folder(target_path):
    from s4translator import t
    if not target_path or not os.path.exists(target_path):
        return False
        
    if not os.path.isdir(target_path):
        return False
        
    if is_safe_to_delete(target_path):
        try:
            shutil.rmtree(target_path)
            return True
        except Exception as e:
            print(t("Erro ao excluir pasta {}: {}").format(target_path, e))
            return False
    else:
        print(t("BLOQUEIO DE SEGURANÇA: Tentativa de apagar pasta vital prevenida! ({})").format(target_path))
        return False

def safe_remove(target_path):
    from s4translator import t
    if not target_path or not os.path.exists(target_path):
        return False
        
    if os.path.isdir(target_path):
        return False
        
    if is_safe_to_delete(target_path):
        try:
            os.remove(target_path)
            return True
        except Exception as e:
            print(t("Erro ao excluir arquivo {}: {}").format(target_path, e))
            return False
    else:
        print(t("BLOQUEIO DE SEGURANÇA: Tentativa de apagar caminho protegido prevenida! ({})").format(target_path))
        return False
