#!/usr/bin/env python3
import struct
import os
import sys
import shutil
import tempfile
import subprocess
import json
import datetime
from contextlib import ExitStack
from s4common import command_exists, is_path_inside
from s4translator import t

# ==================================================================================
# SIMS 4 PACKAGE MERGER (LINUX/PYTHON NATIVE) - CORE ENGINE V2
# Features:
# - Merges multiple .package files (DBPF format).
# - Load Order: Last file in list overwrites resources of previous files.
# - Auto-chunking: Splits into multiple files if total size > limit.
# - TGI Sort: Sorts the final index for better game performance.
# - Integrity: Uses fsync to prevent data corruption.
# - Optimized Buffer: Faster file copying with 4MB chunks.
# ==================================================================================

DEFAULT_MAX_SIZE_GB = 1.0
BUFFER_SIZE = 4 * 1024 * 1024  # 4MB buffer for much faster I/O

def validate_extracted_path(path, temp_dir):
    real_path = os.path.realpath(path)
    real_temp = os.path.realpath(temp_dir)
    if real_path == real_temp or is_path_inside(real_path, real_temp):
        return real_path
    raise ValueError(t("Arquivo extraído fora da pasta temporária. Merge bloqueado: {}").format(path))

def read_exact(file_handle, size, context):
    data = file_handle.read(size)
    if len(data) != size:
        raise ValueError(t("Arquivo DBPF truncado ao ler {}.").format(context))
    return data

def merge_sims4_packages(input_dir, output_dir, max_size_gb=DEFAULT_MAX_SIZE_GB, log_callback=None, progress_callback=None):
    def log(msg):
        if log_callback: log_callback(msg)
        else: print(msg)

    def progress(current, total, status_text=None):
        if progress_callback: progress_callback(current, total, status_text)

    max_size_bytes = max_size_gb * 1024 * 1024 * 1024

    if not os.path.exists(input_dir):
        log(t("❌ Erro: Pasta de entrada não encontrada: {}").format(input_dir))
        return False

    if not os.path.exists(output_dir):
        os.makedirs(output_dir)

    all_files = []
    for root, _, files in os.walk(input_dir):
        for file in files:
            if file.lower().endswith(".package") and not file.startswith("Merged_"):
                all_files.append(os.path.join(root, file))
    
    # Ordem alfabética garante consistência, mas o usuário pode prover lista ordenada no futuro
    all_files.sort()
    total_files = len(all_files)

    if total_files == 0:
        log(t("❌ Nenhum arquivo .package encontrado para unir."))
        return False

    log(t("📦 Encontrados {} arquivos. Iniciando Merge (Limite: {}GB)...").format(total_files, max_size_gb))
    progress(0, total_files, t("Iniciando..."))

    chunk_index = 1
    global_resources = {}
    base_header = None
    run_id = datetime.datetime.now().strftime("%Y%m%d_%H%M%S_%f")
    
    success_count = 0
    skipped_count = 0
    failed_files = []
    temp_outputs = []
    finalized_outputs = []

    current_out_f = None
    try:
        for i, fpath in enumerate(all_files):
            fname = os.path.basename(fpath)
            if i % 10 == 0 or i == total_files - 1:
                progress(i + 1, total_files, t("Lendo Metadados: {}/{} ({}...)").format(i+1, total_files, fname[:20]))

            try:
                with open(fpath, "rb") as in_f:
                    header = in_f.read(96)
                    if len(header) < 96 or header[0:4] != b'DBPF':
                        raise ValueError("Inválido")

                    if base_header is None: base_header = bytearray(header)

                    index_count = struct.unpack('<I', header[36:40])[0]
                    index_offset = struct.unpack('<I', header[64:68])[0]
                    source_size = os.path.getsize(fpath)
                    if index_offset < 96 or index_offset + 4 > source_size:
                        raise ValueError(t("Índice DBPF fora dos limites do arquivo."))
                    
                    if index_count == 0: continue

                    in_f.seek(index_offset)
                    idx_flags = struct.unpack('<I', read_exact(in_f, 4, "flags do índice"))[0]

                    const_type = read_exact(in_f, 4, "tipo constante") if (idx_flags & 1) else None
                    const_group = read_exact(in_f, 4, "grupo constante") if (idx_flags & 2) else None
                    const_instance_ex = read_exact(in_f, 4, "instância constante") if (idx_flags & 4) else None

                    for _ in range(index_count):
                        rtype = const_type if (idx_flags & 1) else read_exact(in_f, 4, "tipo")
                        rgroup = const_group if (idx_flags & 2) else read_exact(in_f, 4, "grupo")
                        
                        if (idx_flags & 4):
                            rinstance_low = read_exact(in_f, 4, "instância low")
                            rinstance_ex = const_instance_ex
                        else:
                            # O DBPF salva o Instance de 8 bytes como Low(4) depois Ex(4) (Little Endian)
                            rinstance_low = read_exact(in_f, 4, "instância low")
                            rinstance_ex = read_exact(in_f, 4, "instância ex")

                        roffset = struct.unpack('<I', read_exact(in_f, 4, "offset"))[0]
                        rsize_raw = struct.unpack('<I', read_exact(in_f, 4, "tamanho"))[0]
                        rmemsize = read_exact(in_f, 4, "memsize")
                        rflags = read_exact(in_f, 2, "flags")
                        runknown = read_exact(in_f, 2, "unknown")
                        
                        # A chave para SORTING (TGI) precisa ser Ex primeiro, depois Low
                        resource_key = rtype + rgroup + rinstance_ex + rinstance_low
                        
                        actual_size = rsize_raw & 0x7FFFFFFF
                        if roffset < 0 or actual_size < 0 or roffset + actual_size > source_size:
                            raise ValueError(t("Payload DBPF fora dos limites do arquivo."))
                        
                        global_resources[resource_key] = {
                            'fpath': fpath,
                            'offset': roffset,
                            'size': actual_size,
                            'compressed': bool(rsize_raw & 0x80000000),
                            'memsize': rmemsize,
                            'flags': rflags,
                            'unknown': runknown
                        }
                
                success_count += 1
            except Exception as e:
                log(t("  ❌ Erro em '{}': {}").format(fname, e))
                skipped_count += 1
                failed_files.append(fpath)

        if global_resources:
            current_resources = {}
            current_payload_size = 0
            for resource_key, resource_data in sorted(global_resources.items(), key=lambda item: item[0]):
                if current_resources and (current_payload_size + resource_data['size']) > max_size_bytes * 0.9:
                    current_output_filename = f"Merged_Content_{run_id}_Part{chunk_index:03d}.package"
                    current_out_path = os.path.join(output_dir, current_output_filename)
                    current_tmp_path = current_out_path + ".tmp"
                    temp_outputs.append(current_tmp_path)
                    current_out_f = open(current_tmp_path, "wb")
                    current_out_f.write(b'\x00' * 96)
                    log(t("🏁 Finalizando Parte {}...").format(chunk_index))
                    write_chunk(current_out_f, current_resources, base_header, log)
                    current_out_f.close()
                    current_out_f = None
                    finalized_outputs.append((current_tmp_path, current_out_path))
                    chunk_index += 1
                    current_resources = {}
                    current_payload_size = 0

                current_resources[resource_key] = resource_data
                current_payload_size += resource_data['size']

            current_output_filename = f"Merged_Content_{run_id}_Part{chunk_index:03d}.package"
            current_out_path = os.path.join(output_dir, current_output_filename)
            current_tmp_path = current_out_path + ".tmp"
            temp_outputs.append(current_tmp_path)
            current_out_f = open(current_tmp_path, "wb")
            current_out_f.write(b'\x00' * 96)
            log(t("🏁 Finalizando Parte {}...").format(chunk_index))
            write_chunk(current_out_f, current_resources, base_header, log)
            current_out_f.close()
            current_out_f = None
            finalized_outputs.append((current_tmp_path, current_out_path))

        for tmp_path, final_path in finalized_outputs:
            os.replace(tmp_path, final_path)
        
    except Exception as e:
        log(t("❌ Erro fatal: {}").format(e))
        for tmp_path in temp_outputs:
            if os.path.exists(tmp_path):
                try:
                    os.remove(tmp_path)
                except Exception:
                    pass
        return False
    finally:
        if current_out_f and not current_out_f.closed:
            current_out_f.close()
        for tmp_path in temp_outputs:
            if os.path.exists(tmp_path):
                try:
                    os.remove(tmp_path)
                except Exception:
                    pass

    report_path = os.path.join(output_dir, f"merge_report_{run_id}.json")
    report = {
        "success_count": success_count,
        "skipped_count": skipped_count,
        "failed_files": failed_files,
        "parts": len(finalized_outputs),
        "report_path": report_path,
    }
    try:
        with open(report_path, "w", encoding="utf-8") as report_file:
            json.dump(report, report_file, indent=2, ensure_ascii=False)
    except Exception as e:
        log(t("  ⚠️ Não foi possível salvar relatório do merge: {}").format(e))

    if failed_files:
        if success_count == 0:
            log(t("\n❌ FALHA: nenhum arquivo pôde ser unido. Veja {}.").format(os.path.basename(report_path)))
            return {"success": False, "partial": False, **report}
        log(t("\n⚠️ CONCLUÍDO COM AVISOS: {} unidos, {} pulados. Veja {}.").format(success_count, skipped_count, os.path.basename(report_path)))
        return {"success": False, "partial": True, **report}

    if len(finalized_outputs) == 0:
        log(t("\n❌ FALHA: nenhum recurso válido foi encontrado para unir."))
        return {"success": False, "partial": False, **report}

    log(t("\n✅ CONCLUÍDO! {} arquivos unidos em {} partes.").format(success_count, len(finalized_outputs)))
    return {"success": True, "partial": False, **report}

def calculate_merge_stats(input_dir, max_size_gb):
    """Retorna (num_arquivos, num_partes_estimadas)"""
    total_size = 0
    count = 0
    if not os.path.exists(input_dir): return (0, 0)
    for root, _, files in os.walk(input_dir):
        for f in files:
            if f.lower().endswith(".package"):
                total_size += os.path.getsize(os.path.join(root, f))
                count += 1
    
    max_bytes = max_size_gb * 1024 * 1024 * 1024
    parts = max(1, int((total_size / (max_bytes * 0.9)) + 0.99))
    return count, parts

def prepare_temp_merge_area(files, temp_dir):
    """
    Extrai todos os packages de uma lista de arquivos (zips ou packages) 
    para uma pasta temporária flat, renomeando conflitos de nome.
    """
    extracted_paths = []
    if not command_exists('7z'):
        raise RuntimeError(t("Dependência ausente: instale o pacote 'p7zip'/'7zip' para extrair arquivos compactados."))
    
    for f in files:
        if f.lower().endswith('.package'):
            dest = os.path.join(temp_dir, os.path.basename(f))
            # Antifalha: Se já existir um arquivo com esse nome de outro mod
            counter = 1
            base, ext = os.path.splitext(os.path.basename(f))
            while os.path.exists(dest):
                dest = os.path.join(temp_dir, f"{base}_{counter}{ext}")
                counter += 1
            shutil.copy2(f, dest)
            extracted_paths.append(dest)
        elif f.lower().endswith(('.zip', '.7z', '.rar')):
            # Extrair packages do zip
            sub_temp = tempfile.mkdtemp(dir=temp_dir)
            res = subprocess.run(['7z', 'x', f, f'-o{sub_temp}', '-y'], capture_output=True, text=True)
            if res.returncode != 0:
                raise RuntimeError(res.stderr.strip() or res.stdout.strip() or t("Falha ao extrair o arquivo compactado."))
            for root, _, sub_files in os.walk(sub_temp):
                for sf in sub_files:
                    if sf.lower().endswith('.package'):
                        src = validate_extracted_path(os.path.join(root, sf), sub_temp)
                        dest = os.path.join(temp_dir, sf)
                        counter = 1
                        base, ext = os.path.splitext(sf)
                        while os.path.exists(dest):
                            dest = os.path.join(temp_dir, f"{base}_{counter}{ext}")
                            counter += 1
                        shutil.move(src, dest)
                        extracted_paths.append(dest)
    return extracted_paths

def write_chunk(f_handle, resources, base_header, log_func):
    """Grava os dados físicos e o índice ordenado no arquivo."""
    final_entries = []
    
    # 1. Gravar Payloads (Dados dos recursos)
    # Ordenamos os recursos por arquivo de origem para minimizar seeks no HD de leitura
    sorted_by_file = sorted(resources.items(), key=lambda x: x[1]['fpath'])
    
    with ExitStack() as stack:
        open_sources = {}
        for i, (key, res) in enumerate(sorted_by_file):
            if i % 100 == 0:
                log_func(t("    Gravando recursos: {}/{}...").format(i, len(resources)))

            src_f = open_sources.get(res['fpath'])
            if src_f is None:
                src_f = stack.enter_context(open(res['fpath'], "rb"))
                open_sources[res['fpath']] = src_f

            source_size = os.path.getsize(res['fpath'])
            if res['offset'] + res['size'] > source_size:
                raise ValueError(t("Payload DBPF fora dos limites durante gravação: {}").format(res['fpath']))

            src_f.seek(res['offset'])
            new_offset = f_handle.tell()
            
            # Copiar em chunks
            bytes_to_read = res['size']
            while bytes_to_read > 0:
                buf = src_f.read(min(bytes_to_read, BUFFER_SIZE))
                if not buf:
                    raise ValueError(t("Payload DBPF truncado durante gravação: {}").format(res['fpath']))
                f_handle.write(buf)
                bytes_to_read -= len(buf)
            
            # Preparar entrada para o índice
            # DBPF Index Entry (32 bytes sem flags): Type(4), Group(4), InstLow(4), InstEx(4), Offset(4), Size(4), MemSize(4), Flags(2), Unknown(2)
            # Re-empacotar tamanho com flag de compressão
            size_packed = struct.pack('<I', res['size'] | (0x80000000 if res['compressed'] else 0))
            offset_packed = struct.pack('<I', new_offset)
            
            # Recuperar da chave de ordenação (onde Ex vem antes de Low para TGI sort correto)
            rtype = key[0:4]
            rgroup = key[4:8]
            rinstance_ex = key[8:12]
            rinstance_low = key[12:16]
            
            # Gravar no arquivo em formato Little Endian correto (Low antes de Ex)
            entry = rtype + rgroup + rinstance_low + rinstance_ex + offset_packed + size_packed + res['memsize'] + res['flags'] + res['unknown']
            # Salvar com a chave de ordenação original para ordenar depois
            final_entries.append((key, entry))

    # 2. Ordenar o índice (TGI Sort) - Melhora performance do Jogo
    # Ordenar pela chave TGI original (Type, Group, InstanceEx, InstanceLow)
    final_entries.sort(key=lambda x: x[0])

    # 3. Gravar o Índice
    index_start = f_handle.tell()
    # Header do índice (4 bytes de flags, aqui usamos 0 pois as entradas são completas)
    f_handle.write(struct.pack('<I', 0))
    for _, entry in final_entries:
        f_handle.write(entry)
    
    # 4. Finalizar o Header DBPF
    f_handle.seek(0)
    header = bytearray(base_header) if base_header else bytearray(b'DBPF\x02\x00\x00\x00' + b'\x00'*88)
    header[36:40] = struct.pack('<I', len(final_entries)) # Index Count
    total_index_size = (len(final_entries) * 32) + 4
    header[44:48] = struct.pack('<I', total_index_size) # Index Size
    header[64:68] = struct.pack('<I', index_start) # Index Offset
    f_handle.write(header)
    
    # 5. INTEGRIDADE: Forçar gravação no disco
    f_handle.flush()
    os.fsync(f_handle.fileno())

if __name__ == "__main__":
    if len(sys.argv) < 3:
        print("Uso: python3 s4merger.py <entrada> <saida>")
    else:
        merge_sims4_packages(sys.argv[1], sys.argv[2])
