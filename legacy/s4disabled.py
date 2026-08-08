#!/usr/bin/env python3
import datetime
import json
import os
import shutil

from s4common import (
    CONFIG_DIR,
    get_mods_dir,
    is_path_inside,
    safe_remove,
    unique_dest_path,
)

MANIFEST_PATH = os.path.join(CONFIG_DIR, "disabled_mods.json")

REASONS = {
    "merge_original": "Original desativado após merge",
    "suspected_bug": "Suspeita de bug",
    "testing": "Desativado para teste",
    "outdated": "Mod desatualizado",
    "duplicate": "Duplicata",
    "manual": "Manual",
}


def _now_iso():
    return datetime.datetime.now().isoformat(timespec="seconds")


def _load_manifest():
    if not os.path.exists(MANIFEST_PATH):
        return {}
    try:
        with open(MANIFEST_PATH, "r", encoding="utf-8") as fh:
            data = json.load(fh)
        return data if isinstance(data, dict) else {}
    except (OSError, json.JSONDecodeError):
        return {}


def _save_manifest(data):
    os.makedirs(CONFIG_DIR, exist_ok=True)
    tmp_path = MANIFEST_PATH + ".tmp"
    with open(tmp_path, "w", encoding="utf-8") as fh:
        json.dump(data, fh, indent=2, ensure_ascii=False)
        fh.flush()
        os.fsync(fh.fileno())
    os.replace(tmp_path, MANIFEST_PATH)


def _validate_mod_file(path):
    mods_dir = get_mods_dir()
    if not mods_dir:
        raise ValueError("Pasta Mods não configurada.")
    if not path or not os.path.isfile(path):
        raise ValueError("Arquivo não encontrado.")

    real_path = os.path.realpath(path)
    real_mods = os.path.realpath(mods_dir)
    if not is_path_inside(real_path, real_mods):
        raise ValueError("Operação bloqueada: arquivo fora de Mods.")
    return real_path, real_mods


def _disabled_name(path):
    name = os.path.basename(path)
    return name if name.endswith(".disabled") else name + ".disabled"


def list_disabled_mods(include_missing=False):
    manifest = _load_manifest()
    items = []
    for disabled_path, data in manifest.items():
        exists = os.path.exists(disabled_path)
        if exists or include_missing:
            entry = dict(data)
            entry["disabled_path"] = disabled_path
            entry["exists"] = exists
            items.append(entry)
    items.sort(key=lambda item: (item.get("reason", ""), item.get("disabled_at", ""), item.get("disabled_path", "")))
    return items


def disable_mod(
    path,
    reason="manual",
    note="",
    source_process="organizer",
    related_output=None,
    archive_dir=None,
):
    real_path, real_mods = _validate_mod_file(path)
    reason = reason if reason in REASONS else "manual"

    if archive_dir:
        archive_real = os.path.realpath(archive_dir)
        if not (archive_real == real_mods or is_path_inside(archive_real, real_mods)):
            raise ValueError("Destino de desativados precisa ficar dentro de Mods.")
        os.makedirs(archive_real, exist_ok=True)
        disabled_path = unique_dest_path(archive_real, _disabled_name(real_path))
        shutil.move(real_path, disabled_path)
    else:
        disabled_path = unique_dest_path(os.path.dirname(real_path), _disabled_name(real_path))
        os.rename(real_path, disabled_path)

    manifest = _load_manifest()
    try:
        size = os.path.getsize(disabled_path)
    except OSError:
        size = None
    manifest[os.path.realpath(disabled_path)] = {
        "original_path": real_path[:-9] if real_path.endswith(".disabled") else real_path,
        "disabled_at": _now_iso(),
        "reason": reason,
        "label": REASONS[reason],
        "note": note,
        "source_process": source_process,
        "related_output": related_output,
        "size": size,
        "can_restore": True,
    }
    _save_manifest(manifest)
    return os.path.realpath(disabled_path)


def restore_disabled(disabled_path):
    manifest = _load_manifest()
    real_disabled = os.path.realpath(disabled_path)
    data = manifest.get(real_disabled, {})

    if not os.path.isfile(real_disabled):
        manifest.pop(real_disabled, None)
        _save_manifest(manifest)
        return False, "Arquivo desativado não encontrado."

    target = data.get("original_path")
    if not target:
        target = real_disabled[:-9] if real_disabled.endswith(".disabled") else real_disabled.replace(".disabled", "")

    mods_dir = get_mods_dir()
    if not mods_dir or not is_path_inside(os.path.realpath(target), os.path.realpath(mods_dir)):
        return False, "Destino original inválido ou fora de Mods."

    os.makedirs(os.path.dirname(target), exist_ok=True)
    if os.path.exists(target):
        target = unique_dest_path(os.path.dirname(target), os.path.basename(target))
    shutil.move(real_disabled, target)
    manifest.pop(real_disabled, None)
    _save_manifest(manifest)
    return True, target


def delete_disabled(disabled_path):
    manifest = _load_manifest()
    real_disabled = os.path.realpath(disabled_path)
    if not os.path.exists(real_disabled):
        manifest.pop(real_disabled, None)
        _save_manifest(manifest)
        return True
    if safe_remove(real_disabled):
        manifest.pop(real_disabled, None)
        _save_manifest(manifest)
        return True
    return False


def register_existing_disabled(path, reason="manual", note="", source_process="organizer"):
    real_path, _ = _validate_mod_file(path)
    if not real_path.endswith(".disabled"):
        raise ValueError("Arquivo não está desativado.")
    manifest = _load_manifest()
    manifest.setdefault(real_path, {
        "original_path": real_path[:-9],
        "disabled_at": _now_iso(),
        "reason": reason if reason in REASONS else "manual",
        "label": REASONS.get(reason, REASONS["manual"]),
        "note": note,
        "source_process": source_process,
        "related_output": None,
        "size": os.path.getsize(real_path),
        "can_restore": True,
    })
    _save_manifest(manifest)
    return real_path
