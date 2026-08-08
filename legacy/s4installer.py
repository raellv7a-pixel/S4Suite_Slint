#!/usr/bin/env python3
import os
import zipfile
import shutil
import re
import tempfile
import sys
import hashlib
import subprocess
from s4common import (
    ConfigManager,
    command_exists,
    safe_delete_folder,
    safe_remove,
    unique_dest_path,
    is_path_inside,
)
from s4translator import t

# ==================================================================================
# S4 SUITE: MOD INSTALLER ENGINE V4 (LINUX)
# Features:
# - Anti-Malware: Blocks .exe, .bat, etc.
# - Safe Update: Only replaces a previous S4Suite-managed folder with the same install identity.
# - Safe & Surgical Deletion: Deletes old version files without wiping custom user folders.
# ==================================================================================

def install_identity_name(filename):
    name = os.path.basename(filename)
    name = os.path.splitext(name)[0]
    name = re.sub(r'[^a-zA-Z0-9]', '_', name)
    return name.strip('_').lower()

def is_valid_mod_file(filename):
    if filename.startswith('._') or filename == '.DS_Store':
        return False
    ext = os.path.splitext(filename)[1].lower()
    return ext in ['.package', '.ts4script']

def _validate_extracted_path(path, temp_dir):
    real_path = os.path.realpath(path)
    real_temp = os.path.realpath(temp_dir)
    if real_path == real_temp or is_path_inside(real_path, real_temp):
        return real_path
    raise ValueError(t("Arquivo extraído fora da pasta temporária. Instalação bloqueada: {}").format(path))

def _copy_files_to_staging(files_data, staging_target_dir, temp_dir):
    staged_files = []
    for abs_path, rel_path in files_data:
        _validate_extracted_path(abs_path, temp_dir)
        filename = os.path.basename(rel_path)

        if filename.endswith('.ts4script'):
            dest_path = unique_dest_path(staging_target_dir, filename)
        else:
            clean_rel = os.path.normpath(rel_path)
            if clean_rel.startswith("..") or os.path.isabs(clean_rel):
                raise ValueError(t("Caminho inválido dentro do arquivo compactado: {}").format(rel_path))
            dest_path = os.path.join(staging_target_dir, clean_rel)
            if os.path.exists(dest_path):
                dest_path = unique_dest_path(os.path.dirname(dest_path), os.path.basename(dest_path))
            os.makedirs(os.path.dirname(dest_path), exist_ok=True)

        shutil.copy2(abs_path, dest_path)
        staged_files.append((dest_path, rel_path))
    return staged_files

def _restore_install_backup(existing_folder, backup_path):
    try:
        if os.path.exists(existing_folder):
            safe_delete_folder(existing_folder)
        shutil.copytree(backup_path, existing_folder, symlinks=True)
        return True, None
    except Exception as e:
        return False, str(e)

def get_mod_files_from_archive(archive_path, temp_dir):
    """
    Extrai e retorna uma lista de tuplas (caminho_absoluto, caminho_relativo).
    Gera erro se detectar executáveis.
    """
    valid_files = []
    malicious_exts = ['.exe', '.bat', '.msi', '.cmd', '.vbs', '.dll']
    ext = archive_path.lower().split('.')[-1]

    if ext in ['package', 'ts4script']:
        dest = os.path.join(temp_dir, os.path.basename(archive_path))
        shutil.copy2(archive_path, dest)
        return [(dest, os.path.basename(archive_path))]

    try:
        if not command_exists('7z'):
            raise RuntimeError(t("Dependência ausente: instale o pacote 'p7zip'/'7zip' para extrair arquivos compactados."))

        res = subprocess.run(['7z', 'x', archive_path, f'-o{temp_dir}', '-y'], capture_output=True, text=True)
        if res.returncode != 0:
            raise RuntimeError(res.stderr.strip() or res.stdout.strip() or t("Falha ao extrair o arquivo compactado."))
            
        for root, _, files in os.walk(temp_dir):
            for file in files:
                file_ext = os.path.splitext(file)[1].lower()
                if file_ext in malicious_exts:
                    raise ValueError(t("O arquivo '{}' é um executável suspeito. Instalação bloqueada por segurança.").format(file))
                if is_valid_mod_file(file):
                    abs_p = _validate_extracted_path(os.path.join(root, file), temp_dir)
                    rel_p = os.path.relpath(abs_p, temp_dir)
                    valid_files.append((abs_p, rel_p))
    except ValueError as ve:
        raise ve # Propaga o erro de malware
    except RuntimeError:
        raise
    except Exception as e:
        raise RuntimeError(t("Falha ao validar o arquivo compactado: {}").format(e))
    
    return valid_files

def _validate_install_base_dir(target_base_dir, mods_folder):
    if not target_base_dir:
        return None

    target_real = os.path.realpath(target_base_dir)
    mods_real = os.path.realpath(mods_folder)
    if not os.path.isdir(target_real):
        raise ValueError(t("Pasta de destino não encontrada: {}").format(target_base_dir))
    if not is_path_inside(target_real, mods_real):
        raise ValueError(t("Destino bloqueado por segurança: escolha uma pasta dentro de Mods."))
    return target_real

def find_existing_mod(new_files, mods_folder, archive_path=None, managed_base_dir=None):
    """
    Busca uma instalação anterior somente por identidade direta de pasta.

    Arquivos internos de zips podem ser compartilhados por mods diferentes
    (textures, overlays, hair bases). Usar esses nomes para decidir atualização
    pode apagar conteúdo legítimo de outro mod. Por isso, atualização automática
    só é aceita quando a pasta gerenciada pela S4Suite tem a mesma identidade do
    arquivo/arquivo compactado de origem.

    Retorna: (caminho_da_pasta_existente, [lista_de_arquivos_antigos_para_deletar])
    """
    if not os.path.exists(mods_folder):
        return None, []

    candidate_folder_names = []
    if archive_path:
        archive_identity = install_identity_name(archive_path)
        if archive_identity:
            candidate_folder_names.append(archive_identity)

    if archive_path and is_valid_mod_file(os.path.basename(archive_path)):
        for new_file in new_files:
            file_identity = install_identity_name(new_file)
            if file_identity:
                candidate_folder_names.append(file_identity)

    base_dir = managed_base_dir or os.path.join(mods_folder, "00_Triagem_Novos")
    for folder_name in candidate_folder_names:
        direct_path = os.path.join(base_dir, folder_name)
        if os.path.isdir(direct_path):
            old_files = []
            for root, _, files in os.walk(direct_path):
                for f in files:
                    if is_valid_mod_file(f):
                        old_files.append(os.path.relpath(os.path.join(root, f), direct_path))
            return direct_path, old_files

    return None, []

def scan_dependencies(files_data):
    requirements = []
    for abs_f, rel_f in files_data:
        if abs_f.endswith('.package'):
            try:
                with open(abs_f, 'rb') as pf:
                    content = pf.read(min(os.path.getsize(abs_f), 500000))
                    if b'Lot51' in content: requirements.append("Lot51 Core Library")
                    if b'XML Injector' in content or b'XmlInjector' in content: requirements.append("XML Injector")
                    if b'Frankosas' in content: requirements.append("Frankosas Utilities")
            except Exception as e:
                print(t("Erro ao escanear dependências em {}: {}").format(abs_f, e))
    return list(set(requirements))

def careful_scan_mod(archive_path, mods_dir):
    """
    Descompacta o mod numa temp dir, escaneia contra a pasta de mods e gera um relatório.
    Retorna: lista de conflitos, erro (se houver)
    """
    if not os.path.exists(archive_path) or not os.path.exists(mods_dir):
        return [], t("Arquivo ou pasta de mods não encontrados.")

    with tempfile.TemporaryDirectory() as temp_dir:
        try:
            files_data = get_mod_files_from_archive(archive_path, temp_dir)
        except (ValueError, RuntimeError) as ve:
            return [], str(ve)
            
        if not files_data:
            return [], t("Nenhum arquivo de mod útil encontrado no ZIP.")

        existing_mods_map = {}
        for root, _, files in os.walk(mods_dir):
            for f in files:
                if is_valid_mod_file(f):
                    existing_mods_map.setdefault(f.lower(), []).append({
                        'path': os.path.join(root, f),
                        'size': os.path.getsize(os.path.join(root, f)),
                    })

        report = []
        for vf_abs, vf_rel in files_data:
            filename = os.path.basename(vf_abs)
            clean_vf = filename.lower()
            new_size = os.path.getsize(vf_abs)
            
            match_found = False
            # Match exato por nome. Matches por "nome limpo" são informativos
            # demais para decidir substituição com segurança: muitos mods
            # diferentes compartilham arquivos auxiliares com nomes parecidos.
            if clean_vf in existing_mods_map:
                existing_matches = existing_mods_map[clean_vf]
                exact_size_match = next((m for m in existing_matches if m['size'] == new_size), None)
                exist_data = exact_size_match or existing_matches[0]
                if exist_data['size'] == new_size:
                    report.append({'file': filename, 'status': 'EXACT_MATCH', 'existing_path': exist_data['path']})
                else:
                    report.append({'file': filename, 'status': 'SIZE_DIFF', 'existing_path': exist_data['path']})
                match_found = True
                        
            if not match_found:
                report.append({'file': filename, 'status': 'NEW', 'existing_path': None})

        return report, None

def detect_mutually_exclusive(files_data):
    options = []
    keywords = [
        'option',
        'choose',
        'pick',
        'select_one',
        'select one',
        'exclusive',
        'only_one',
        'only one',
        'pick_one',
        'pick one',
        'choose_one',
        'choose one',
    ]
    for abs_p, rel_p in files_data:
        normalized_rel = rel_p.replace("\\", "/").lower()
        parts = [part for part in normalized_rel.split("/") if part]
        if any(any(key in part for key in keywords) for part in parts):
            options.append((abs_p, rel_p))

    if len(options) > 1:
        return options
    return []

def install_mod(archive_path, mods_dir, force_update=None, selected_file=None, target_base_dir=None):
    if not os.path.exists(archive_path):
        return False, t("Arquivo não encontrado.")

    with tempfile.TemporaryDirectory() as temp_dir:
        try:
            target_base_dir = _validate_install_base_dir(target_base_dir, mods_dir) if target_base_dir else None
            files_data = get_mod_files_from_archive(archive_path, temp_dir)
        except (ValueError, RuntimeError) as ve:
            return False, str(ve) # Retorna erro bloqueando instalação
            
        if not files_data:
            return False, t("Nenhum arquivo de mod útil encontrado.")

        if selected_file is None:
            exclusive_options = detect_mutually_exclusive(files_data)
            if exclusive_options:
                return "NEEDS_SELECTION", [f[1] for f in exclusive_options]

        if selected_file:
            exclusive_options = detect_mutually_exclusive(files_data)
            to_remove_rel = [f[1] for f in exclusive_options if f[1] != selected_file]
            files_data = [f for f in files_data if f[1] not in to_remove_rel]

        deps = scan_dependencies(files_data)
        
        # 1. Proteção contra root
        root_conflict = False
        new_filenames = [os.path.basename(f[1]) for f in files_data]
        for f in new_filenames:
            if os.path.exists(os.path.join(mods_dir, f)):
                root_conflict = True
                break
        
        existing_folder, old_files_to_delete = (mods_dir, new_filenames) if root_conflict else find_existing_mod(
            [f[0] for f in files_data],
            mods_dir,
            archive_path,
            managed_base_dir=target_base_dir,
        )
        
        sims_path = ConfigManager.get("sims4_path")
        real_mods_root = os.path.join(sims_path, "Mods") if sims_path else None
        
        if existing_folder and real_mods_root and os.path.abspath(existing_folder) == os.path.abspath(real_mods_root):
             existing_folder = None 

        rollback_dir = None
        created_target_dir = None
        staging_root = None
        staging_target_dir = None

        if existing_folder:
            if force_update is None: return "NEEDS_DECISION", existing_folder
            if force_update is False: return False, t("Instalação pulada pelo usuário.")

            rollback_dir = tempfile.mkdtemp(prefix="s4suite_install_backup_")
            backup_path = os.path.join(rollback_dir, "previous")
            if os.path.exists(existing_folder):
                if os.path.isdir(existing_folder):
                    shutil.copytree(existing_folder, backup_path, symlinks=True)
                else:
                    os.makedirs(backup_path, exist_ok=True)
            
            # Atualização Cirúrgica
            files_in_dest = os.listdir(existing_folder)
            
            # Se a pasta contém APENAS os arquivos do mod antigo, podemos deletar a pasta toda
            can_delete_folder = True
            for f in files_in_dest:
                if f not in old_files_to_delete and f.lower() not in [".ds_store", "thumbs.db"]:
                    can_delete_folder = False
                    break
            
            target_install_dir = existing_folder
        else:
            mod_name_base = install_identity_name(archive_path) or "Novo_Mod"
            staging_dir = target_base_dir or os.path.join(mods_dir, "00_Triagem_Novos")
            if not os.path.exists(staging_dir): os.makedirs(staging_dir)
            target_install_dir = unique_dest_path(staging_dir, mod_name_base)

        try:
            staging_root = tempfile.mkdtemp(prefix=".s4suite_staging_", dir=mods_dir)
            staging_target_dir = os.path.join(staging_root, "payload")
            os.makedirs(staging_target_dir, exist_ok=True)
            staged_files = _copy_files_to_staging(files_data, staging_target_dir, temp_dir)

            if existing_folder:
                if can_delete_folder and os.path.abspath(existing_folder) != os.path.abspath(mods_dir):
                    if not safe_delete_folder(existing_folder):
                        raise RuntimeError(t("Falha ao remover a versão anterior."))
                    shutil.move(staging_target_dir, existing_folder)
                else:
                    # Deleta apenas os arquivos da versão anterior (ou conflituosos)
                    for f in old_files_to_delete:
                        target_file = os.path.join(existing_folder, f)
                        if os.path.exists(target_file):
                            removed = safe_delete_folder(target_file) if os.path.isdir(target_file) else safe_remove(target_file)
                            if not removed:
                                raise RuntimeError(t("Falha ao remover arquivo antigo: {}").format(target_file))

                    # 2. Mover arquivos preparados para o destino final preservando extras.
                    for staged_path, rel_path in staged_files:
                        filename = os.path.basename(rel_path)
                        if filename.endswith('.ts4script'):
                            dest_path = unique_dest_path(target_install_dir, filename)
                        else:
                            clean_rel = os.path.normpath(rel_path)
                            if clean_rel.startswith("..") or os.path.isabs(clean_rel):
                                raise ValueError(t("Caminho inválido dentro do arquivo compactado: {}").format(rel_path))
                            dest_path = os.path.join(target_install_dir, clean_rel)
                            if os.path.exists(dest_path):
                                dest_path = unique_dest_path(os.path.dirname(dest_path), os.path.basename(dest_path))
                            os.makedirs(os.path.dirname(dest_path), exist_ok=True)
                        shutil.move(staged_path, dest_path)
            else:
                shutil.move(staging_target_dir, target_install_dir)
                created_target_dir = target_install_dir
        except Exception as e:
            if rollback_dir and os.path.exists(backup_path):
                restored, restore_error = _restore_install_backup(existing_folder, backup_path)
                if not restored:
                    return False, t("Erro na instalação e falha ao restaurar backup: {} | Erro original: {}").format(restore_error, e)
            elif created_target_dir and os.path.exists(created_target_dir):
                safe_delete_folder(created_target_dir)
            return False, t("Instalação revertida após erro: {}").format(e)
        finally:
            if staging_root and os.path.exists(staging_root):
                shutil.rmtree(staging_root, ignore_errors=True)
            if rollback_dir and os.path.exists(rollback_dir):
                shutil.rmtree(rollback_dir, ignore_errors=True)

        msg = target_install_dir
        if deps:
            msg += "\n" + t("⚠️ Este mod pode precisar de dependências como: {}").format(', '.join(deps))
            
        return True, msg

if __name__ == "__main__":
    if len(sys.argv) < 2:
        print("Uso: python3 s4installer.py <mod.zip>")
    else:
        sims_path = ConfigManager.get("sims4_path")
        if not sims_path:
            print("Erro: sims4_path não configurado.")
            sys.exit(1)
        status, res = install_mod(sys.argv[1], os.path.join(sims_path, "Mods"))
        print(f"Status: {status}\nResultado: {res}")
