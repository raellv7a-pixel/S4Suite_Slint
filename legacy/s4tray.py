#!/usr/bin/env python3
import struct
import os
import sys
import json
import sqlite3
import tempfile
import zipfile
import shutil
import hashlib
import re
import subprocess
from s4common import (
    CONFIG_DIR,
    ConfigManager,
    DB_PATH,
    command_exists,
    get_mods_dir,
    get_tray_dir,
    unique_dest_path,
    safe_remove,
    safe_delete_folder,
    is_path_inside,
)
from s4translator import t

# ==================================================================================
# S4 SUITE: TRAY & INDEXING ENGINE V3 (LINUX)
# Features:
# - WAL Mode: Fast SQLite concurrent access.
# - MD5 Deduplication: Content-based check for exact matches.
# - Tray Parser: Extracts real names from .trayitem bin files.
# - Auto-Cleanup: Prunes orphan entries.
# ==================================================================================

TRAY_EXTENSIONS = {'householdbinary', 'trayitem', 'sgi', 'hhi', 'bpi', 'blueprint', 'room', 'rmi'}
ARCHIVE_EXTENSIONS = {'.zip', '.7z', '.rar'}
PACKAGE_EXTENSIONS = {'.package'}
BLOCKED_EXTENSIONS = {'.exe', '.bat', '.msi', '.cmd', '.vbs', '.dll', '.scr', '.com', '.sh'}

def sanitize_import_name(name):
    clean = re.sub(r'[^a-zA-Z0-9\-_ ]', '', name or '').strip()
    return clean or "Imported_Item"

def format_bytes(num_bytes):
    value = float(num_bytes or 0)
    for unit in ("B", "KB", "MB", "GB"):
        if value < 1024 or unit == "GB":
            return f"{value:.1f} {unit}" if unit != "B" else f"{int(value)} B"
        value /= 1024
    return f"{value:.1f} GB"

def init_db():
    os.makedirs(CONFIG_DIR, exist_ok=True)
    conn = sqlite3.connect(DB_PATH, timeout=30.0)
    # WAL mode permite leitura e escrita simultânea sem travar
    conn.execute("PRAGMA journal_mode=WAL")
    conn.execute("PRAGMA synchronous=NORMAL")
    
    cursor = conn.cursor()
    cursor.execute('''
        CREATE TABLE IF NOT EXISTS mod_files (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            filepath TEXT UNIQUE,
            mtime REAL
        )
    ''')
    
    # MIGRAÇÃO: Verificar se a coluna md5 existe, se não, adicionar.
    cursor.execute("PRAGMA table_info(mod_files)")
    columns = [row[1] for row in cursor.fetchall()]
    if 'md5' not in columns:
        print(t("🔧 Migrando banco de dados: Adicionando coluna MD5..."))
        cursor.execute("ALTER TABLE mod_files ADD COLUMN md5 TEXT")
    
    cursor.execute('''
        CREATE TABLE IF NOT EXISTS mod_resources (
            type INTEGER,
            group_id INTEGER,
            instance_ex INTEGER,
            instance_low INTEGER,
            file_id INTEGER,
            FOREIGN KEY(file_id) REFERENCES mod_files(id)
        )
    ''')
    cursor.execute('CREATE INDEX IF NOT EXISTS idx_resource ON mod_resources (type, group_id, instance_ex, instance_low)')
    cursor.execute('CREATE INDEX IF NOT EXISTS idx_md5 ON mod_files (md5)')
    conn.commit()
    return conn

def get_file_md5(fpath):
    """
    Gera um 'Fast Hash' combinado (Tamanho + MD5 do primeiro 1MB).
    Isso é 1000x mais rápido que hashear arquivos de 2GB e praticamente impossível de colidir.
    """
    hash_md5 = hashlib.md5()
    try:
        size = os.path.getsize(fpath)
        hash_md5.update(str(size).encode())
        with open(fpath, "rb") as f:
            # Lê apenas o primeiro 1MB para o hash
            chunk = f.read(1024 * 1024)
            if chunk: hash_md5.update(chunk)
        return hash_md5.hexdigest()
    except Exception as e:
        print(t("Erro ao calcular hash de {}: {}").format(fpath, e))
        return None

def get_file_sha256(fpath):
    hash_sha = hashlib.sha256()
    try:
        with open(fpath, "rb") as f:
            for chunk in iter(lambda: f.read(1024 * 1024), b""):
                hash_sha.update(chunk)
        return hash_sha.hexdigest()
    except Exception as e:
        print(t("Erro ao calcular hash completo de {}: {}").format(fpath, e))
        return None

def read_dbpf_index(fpath):
    resources = set()
    try:
        with open(fpath, "rb") as in_f:
            header = in_f.read(96)
            if len(header) < 96 or header[0:4] != b'DBPF': return set()
            index_count = struct.unpack('<I', header[36:40])[0]
            index_offset = struct.unpack('<I', header[64:68])[0]
            if index_count == 0: return set()
            in_f.seek(index_offset)
            idx_flags = struct.unpack('<I', in_f.read(4))[0]
            const_type = struct.unpack('<I', in_f.read(4))[0] if (idx_flags & 1) else None
            const_group = struct.unpack('<I', in_f.read(4))[0] if (idx_flags & 2) else None
            const_instance_ex = struct.unpack('<I', in_f.read(4))[0] if (idx_flags & 4) else None
            for _ in range(index_count):
                rtype = const_type if (idx_flags & 1) else struct.unpack('<I', in_f.read(4))[0]
                rgroup = const_group if (idx_flags & 2) else struct.unpack('<I', in_f.read(4))[0]
                if (idx_flags & 4):
                    rinstance_low = struct.unpack('<I', in_f.read(4))[0]
                    rinstance_ex = const_instance_ex
                else:
                    inst_bytes = in_f.read(8)
                    rinstance_low = struct.unpack('<I', inst_bytes[0:4])[0]
                    rinstance_ex = struct.unpack('<I', inst_bytes[4:8])[0]
                in_f.read(16)
                resources.add((rtype, rgroup, rinstance_ex, rinstance_low))
    except Exception as e:
        print(t("Erro ao ler índice DBPF de {}: {}").format(fpath, e))
    return resources

def parse_tray_name(trayitem_path):
    """Tenta extrair o nome do Sim/Lote de um arquivo binário .trayitem"""
    try:
        with open(trayitem_path, 'rb') as f:
            data = f.read()
            # O nome geralmente está em UTF-16 ou UTF-8 após certas tags XML ou padrões binários
            # Procura por padrões XML <N>...</N> ou <Name>...</Name> que as vezes aparecem no binário
            match = re.search(rb'<N>(.*?)</N>', data)
            if match: return match.group(1).decode('utf-16' if b'\x00' in match.group(1) else 'utf-8', errors='ignore')
            
            # Fallback: Procurar por strings de texto longas (heurística simples)
            match = re.search(rb'\x00\x00\x00([A-Z][a-z]{2,20} [A-Z][a-z]{2,20})\x00', data)
            if match: return match.group(1).decode('utf-8')
    except Exception as e:
        print(t("Erro ao ler nome do Tray item {}: {}").format(trayitem_path, e))
    return None

def validate_extracted_path(path, temp_dir):
    real_path = os.path.realpath(path)
    real_temp = os.path.realpath(temp_dir)
    if real_path == real_temp or is_path_inside(real_path, real_temp):
        return real_path
    raise ValueError(t("Arquivo extraído fora da pasta temporária. Importação bloqueada: {}").format(path))

def update_index(mods_dir):
    conn = init_db()
    cursor = conn.cursor()
    cursor.execute("SELECT id, filepath, mtime FROM mod_files")
    db_files = {row[1]: {'id': row[0], 'mtime': row[2]} for row in cursor.fetchall()}
    
    current_files = []
    if os.path.exists(mods_dir):
        for root, _, files in os.walk(mods_dir):
            for file in files:
                if file.lower().endswith('.package'):
                    current_files.append(os.path.join(root, file))

    to_delete = [data['id'] for fpath, data in db_files.items() if not os.path.exists(fpath)]
    if to_delete:
        cursor.executemany("DELETE FROM mod_resources WHERE file_id = ?", [(i,) for i in to_delete])
        cursor.executemany("DELETE FROM mod_files WHERE id = ?", [(i,) for i in to_delete])
        conn.commit()

    for fpath in current_files:
        try:
            mtime = os.path.getmtime(fpath)
            if fpath not in db_files or db_files[fpath]['mtime'] < mtime:
                md5 = get_file_md5(fpath)
                if fpath in db_files:
                    fid = db_files[fpath]['id']
                    # Remoção do uso de mod_resources, focando apenas em MD5 para performance extrema
                    cursor.execute("UPDATE mod_files SET mtime=?, md5=? WHERE id=?", (mtime, md5, fid))
                else:
                    cursor.execute("INSERT INTO mod_files (filepath, mtime, md5) VALUES (?, ?, ?)", (fpath, mtime, md5))
        except Exception as e:
            print(t("Erro ao atualizar índice de {}: {}").format(fpath, e))
            continue
    
    conn.commit()
    conn.close()

def _extract_source_to_temp(source_path, temp_dir):
    if os.path.isdir(source_path):
        return os.path.realpath(source_path)

    ext = os.path.splitext(source_path)[1].lower()
    if ext not in ARCHIVE_EXTENSIONS:
        raise RuntimeError(t("Formato não suportado para Tray Importer: {}").format(os.path.basename(source_path)))
    if not command_exists('7z'):
        raise RuntimeError(t("Dependência ausente: instale o pacote 'p7zip'/'7zip' para importar arquivos compactados."))

    res = subprocess.run(['7z', 'x', source_path, f'-o{temp_dir}', '-y'], capture_output=True, text=True)
    if res.returncode != 0:
        raise RuntimeError(res.stderr.strip() or res.stdout.strip() or t("Falha ao extrair o arquivo compactado."))
    return os.path.realpath(temp_dir)

def _iter_source_files(source_root):
    for root, _, files in os.walk(source_root):
        for file in files:
            abs_path = validate_extracted_path(os.path.join(root, file), source_root)
            rel_path = os.path.relpath(abs_path, source_root).replace("\\", "/")
            yield abs_path, rel_path, file

def find_exact_duplicate_package(package_path, cursor):
    fast_hash = get_file_md5(package_path)
    if not fast_hash:
        return None

    cursor.execute("SELECT filepath FROM mod_files WHERE md5=?", (fast_hash,))
    candidates = cursor.fetchall()
    if not candidates:
        return None

    package_sha = get_file_sha256(package_path)
    if not package_sha:
        return None

    for (existing_path,) in candidates:
        if os.path.exists(existing_path) and get_file_sha256(existing_path) == package_sha:
            return existing_path
    return None

def analyze_tray_source(source_path, skip_index=False):
    mods_dir = get_mods_dir()
    tray_dir = get_tray_dir()
    if not mods_dir or not tray_dir:
        return {"success": False, "error": t("Caminhos não configurados."), "source_path": source_path}
    if not os.path.exists(source_path):
        return {"success": False, "error": t("Arquivo não encontrado: {}").format(source_path), "source_path": source_path}

    if not skip_index:
        update_index(mods_dir)

    with tempfile.TemporaryDirectory(prefix="s4suite_tray_analyze_") as temp_dir:
        conn = init_db()
        cursor = conn.cursor()
        try:
            source_root = _extract_source_to_temp(source_path, temp_dir)
            sim_name = None
            tray_files = []
            package_files = []
            other_files = []
            blocked_files = []

            for abs_path, rel_path, filename in _iter_source_files(source_root):
                ext = os.path.splitext(filename)[1].lower()
                ext_no_dot = ext.lstrip(".")
                info = {
                    "rel_path": rel_path,
                    "name": filename,
                    "size": os.path.getsize(abs_path),
                    "size_label": format_bytes(os.path.getsize(abs_path)),
                    "ext": ext_no_dot,
                }

                if ext in BLOCKED_EXTENSIONS:
                    blocked_files.append(info)
                elif ext_no_dot in TRAY_EXTENSIONS:
                    tray_files.append(info)
                    if ext_no_dot == "trayitem" and not sim_name:
                        sim_name = parse_tray_name(abs_path)
                elif ext in PACKAGE_EXTENSIONS:
                    duplicate_path = find_exact_duplicate_package(abs_path, cursor)
                    info["duplicate_path"] = duplicate_path
                    info["status"] = "duplicate" if duplicate_path else "new"
                    package_files.append(info)
                else:
                    other_files.append(info)

            import_name = sanitize_import_name(sim_name or os.path.splitext(os.path.basename(source_path))[0])
            default_target_dir = os.path.join(mods_dir, "Imported_Sims", import_name)
            warnings = []
            if blocked_files:
                warnings.append(t("{} arquivo(s) potencialmente perigoso(s) foram detectados e não serão instalados.").format(len(blocked_files)))
            if not tray_files:
                warnings.append(t("Nenhum arquivo Tray foi detectado."))
            if not package_files:
                warnings.append(t("Nenhum package de CC foi detectado."))

            return {
                "success": True,
                "source_path": source_path,
                "source_name": os.path.basename(source_path),
                "import_name": import_name,
                "default_target_dir": default_target_dir,
                "tray_files": sorted(tray_files, key=lambda item: item["rel_path"].lower()),
                "package_files": sorted(package_files, key=lambda item: item["rel_path"].lower()),
                "other_files": sorted(other_files, key=lambda item: item["rel_path"].lower()),
                "blocked_files": sorted(blocked_files, key=lambda item: item["rel_path"].lower()),
                "warnings": warnings,
            }
        except Exception as e:
            return {"success": False, "error": str(e), "source_path": source_path, "source_name": os.path.basename(source_path)}
        finally:
            conn.close()

def _validate_mod_target_dir(target_dir, mods_dir):
    target_real = os.path.realpath(target_dir)
    mods_real = os.path.realpath(mods_dir)
    if target_real != mods_real and not is_path_inside(target_real, mods_real):
        raise ValueError(t("Destino bloqueado por segurança: escolha uma pasta dentro de Mods."))
    os.makedirs(target_real, exist_ok=True)
    return target_real

def import_tray_plan(plan, skip_index=False, log_callback=None):
    def log(message):
        if log_callback:
            log_callback(message)

    mods_dir = get_mods_dir()
    tray_dir = get_tray_dir()
    if not mods_dir or not tray_dir:
        return False, t("Caminhos não configurados."), None

    source_path = plan.get("source_path")
    if not source_path or not os.path.exists(source_path):
        return False, t("Arquivo não encontrado: {}").format(source_path), None

    if not skip_index:
        update_index(mods_dir)

    copied_files = []
    stats = {"tray": 0, "installed": 0, "skipped": 0, "duplicates": 0, "blocked": 0}

    with tempfile.TemporaryDirectory(prefix="s4suite_tray_import_") as temp_dir:
        conn = init_db()
        cursor = conn.cursor()
        try:
            source_root = _extract_source_to_temp(source_path, temp_dir)
            rel_map = {rel_path: abs_path for abs_path, rel_path, _ in _iter_source_files(source_root)}

            for rel_path in plan.get("tray_files", []):
                abs_path = rel_map.get(rel_path)
                if not abs_path:
                    raise RuntimeError(t("Arquivo Tray não encontrado após extração: {}").format(rel_path))
                dest = unique_dest_path(tray_dir, os.path.basename(rel_path))
                shutil.copy2(abs_path, dest)
                copied_files.append(dest)
                stats["tray"] += 1
                log(t("🗂️ Tray instalado: {}").format(os.path.basename(dest)))

            for package in plan.get("package_files", []):
                rel_path = package.get("rel_path")
                action = package.get("action", "install")
                if action == "skip":
                    stats["skipped"] += 1
                    continue

                abs_path = rel_map.get(rel_path)
                if not abs_path:
                    raise RuntimeError(t("Package não encontrado após extração: {}").format(rel_path))

                duplicate_path = find_exact_duplicate_package(abs_path, cursor)
                if duplicate_path and action != "install_copy":
                    stats["duplicates"] += 1
                    log(t("⏭️ Duplicata pulada: {}").format(os.path.basename(rel_path)))
                    continue

                target_dir = _validate_mod_target_dir(package.get("target_dir") or plan.get("default_target_dir"), mods_dir)
                dest = unique_dest_path(target_dir, os.path.basename(rel_path))
                shutil.copy2(abs_path, dest)
                copied_files.append(dest)
                stats["installed"] += 1

                fast_hash = get_file_md5(dest)
                cursor.execute(
                    "INSERT OR REPLACE INTO mod_files (filepath, mtime, md5) VALUES (?, ?, ?)",
                    (dest, os.path.getmtime(dest), fast_hash)
                )
                log(t("📦 CC instalado: {}").format(os.path.basename(dest)))

            conn.commit()
            return True, t("Importação concluída."), stats
        except Exception as e:
            conn.rollback()
            for copied_path in reversed(copied_files):
                if os.path.exists(copied_path):
                    safe_remove(copied_path)
            return False, t("Importação revertida após erro: {}").format(e), stats
        finally:
            conn.close()

def smart_import(zip_path, skip_index=False):
    mods_dir = get_mods_dir()
    tray_dir = get_tray_dir()
    if not mods_dir or not tray_dir: return False, t("Caminhos não configurados."), None
    
    if not skip_index:
        update_index(mods_dir)
        
    with tempfile.TemporaryDirectory() as temp_dir:
        import subprocess
        if not command_exists('7z'):
            return False, t("Dependência ausente: instale o pacote 'p7zip'/'7zip' para importar arquivos compactados."), None
        res = subprocess.run(['7z', 'x', zip_path, f'-o{temp_dir}', '-y'], capture_output=True, text=True)
        if res.returncode != 0:
            return False, res.stderr.strip() or res.stdout.strip() or t("Falha ao extrair o arquivo compactado."), None

        conn = init_db()
        cursor = conn.cursor()
        moved_files = []
        target_mod_folder = None
        try:
            sim_name = None
            tray_files = []
            package_files = []

            for root, _, files in os.walk(temp_dir):
                for file in files:
                    fpath = validate_extracted_path(os.path.join(root, file), temp_dir)
                    ext = file.lower().split('.')[-1]
                    if ext in ['householdbinary', 'trayitem', 'sgi', 'hhi', 'bpi', 'blueprint', 'room']:
                        tray_files.append(fpath)
                        if ext == 'trayitem' and not sim_name: sim_name = parse_tray_name(fpath)
                    elif ext == 'package':
                        package_files.append(fpath)

            import_name = sim_name or os.path.splitext(os.path.basename(zip_path))[0]
            import_name = re.sub(r'[^a-zA-Z0-9\ ]', '', import_name).strip()
            if not import_name:
                import_name = "Imported_Item"
            target_mod_folder = os.path.join(mods_dir, "Imported_Sims", import_name)

            stats = {"tray": 0, "installed": 0, "skipped": 0}

            for f in tray_files:
                dest = unique_dest_path(tray_dir, os.path.basename(f))
                shutil.move(f, dest)
                moved_files.append(dest)
                stats["tray"] += 1

            for f in package_files:
                md5 = get_file_md5(f)
                is_duplicate = False
                cursor.execute("SELECT filepath FROM mod_files WHERE md5=?", (md5,))
                for (existing_path,) in cursor.fetchall():
                    if os.path.exists(existing_path) and get_file_sha256(existing_path) == get_file_sha256(f):
                        is_duplicate = True
                        break

                if is_duplicate:
                    stats["skipped"] += 1
                    continue

                if not os.path.exists(target_mod_folder): os.makedirs(target_mod_folder)
                dest = unique_dest_path(target_mod_folder, os.path.basename(f))
                shutil.move(f, dest)
                moved_files.append(dest)
                stats["installed"] += 1

                new_md5 = get_file_md5(dest)
                cursor.execute(
                    "INSERT OR REPLACE INTO mod_files (filepath, mtime, md5) VALUES (?, ?, ?)",
                    (dest, os.path.getmtime(dest), new_md5)
                )

            conn.commit()

            return True, target_mod_folder if stats["installed"] > 0 else None, stats
        except Exception as e:
            conn.rollback()
            for moved_path in reversed(moved_files):
                if os.path.exists(moved_path):
                    safe_remove(moved_path)
            if target_mod_folder and os.path.isdir(target_mod_folder) and not os.listdir(target_mod_folder):
                safe_delete_folder(target_mod_folder)
            return False, t("Importação revertida após erro: {}").format(e), None
        finally:
            conn.close()

if __name__ == "__main__":
    if len(sys.argv) > 1: print(smart_import(sys.argv[1]))
