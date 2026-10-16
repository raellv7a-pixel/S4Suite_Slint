//! Auditoria integrada P0 da S4Suite Slint:
//! Validação de ponta a ponta com DBPF, integridade SHA-256 antes/depois,
//! atomicidade, symlinks, proteção da raiz e ausência de resíduos temporários.

use s4suite::core::safety::{is_path_inside, is_safe_to_delete, safe_remove_dir_all, safe_remove_file};
use s4suite::engine::dbpf::{DBPFReader, DBPFWriter, PackageResource, ResourceKey};
use s4suite::engine::installer::{
    calculate_conflicts, execute_installation, prepare_staging, MANAGED_BASE_DIR, STAGING_DIR_NAME,
};
use s4suite::engine::merger::{merge_sims4_packages, MergeTask};
use s4suite::engine::organizer::{transfer_items, TransferMode};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn sha256_digest(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}

fn file_sha256(path: &Path) -> String {
    let mut f = File::open(path).unwrap();
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).unwrap();
    sha256_digest(&buf)
}

fn make_real_dbpf_package(path: &Path, resources: &[(u32, &[u8])]) -> Vec<u8> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    let res_vec: Vec<PackageResource> = resources
        .iter()
        .map(|(instance, data)| PackageResource {
            key: ResourceKey {
                type_id: 0x0333406C,
                group_id: 0x00000000,
                instance_ex: 0x00000000,
                instance_low: *instance,
            },
            data: data.to_vec(),
            mem_size: data.len() as u32,
            compressed: 0,
        })
        .collect();

    let mut cursor = std::io::Cursor::new(Vec::new());
    DBPFWriter::write_package(&mut cursor, &res_vec).unwrap();
    let bytes = cursor.into_inner();
    fs::write(path, &bytes).unwrap();
    bytes
}

fn make_zip_archive(path: &Path, entries: &[(&str, &[u8])]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    let file = File::create(path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default();
    for (name, content) in entries {
        zip.start_file(*name, options).unwrap();
        zip.write_all(content).unwrap();
    }
    zip.finish().unwrap();
}
struct AuditEnv {
    _root: TempDir,
    mods_dir: PathBuf,
    downloads_dir: PathBuf,
}

impl AuditEnv {
    fn new() -> Self {
        let root = TempDir::new().unwrap();
        let mods_dir = root.path().join("Mods");
        let downloads_dir = root.path().join("Downloads");
        fs::create_dir_all(&mods_dir).unwrap();
        fs::create_dir_all(&downloads_dir).unwrap();
        Self {
            _root: root,
            mods_dir,
            downloads_dir,
        }
    }

    fn staging(&self) -> PathBuf {
        self.mods_dir.join(STAGING_DIR_NAME)
    }

    fn managed(&self) -> PathBuf {
        self.mods_dir.join(MANAGED_BASE_DIR)
    }
}

#[test]
fn auditoria_instalacao_e_integridade_sha256() {
    let env = AuditEnv::new();

    // Cria pacote DBPF com payloads reais
    let payload1 = b"CONTEUDO_CAS_CABELO_REAL_001_A_TESTE_INTEGRIDADE";
    let payload2 = b"CONTEUDO_CAS_TEXTURA_REAL_002_B_TESTE_INTEGRIDADE";
    let pkg_path = env.downloads_dir.join("Criador_Cabelo.package");
    let pkg_bytes = make_real_dbpf_package(&pkg_path, &[(1, payload1), (2, payload2)]);
    let sha_orig = sha256_digest(&pkg_bytes);

    let zip_path = env.downloads_dir.join("Criador_Cabelo_v1.zip");
    make_zip_archive(&zip_path, &[("Criador_Cabelo.package", &pkg_bytes)]);

    // 1. Staging e Análise
    let staging_rep = prepare_staging(&[zip_path], &env.staging()).unwrap();
    assert_eq!(staging_rep.prepared, 1);
    assert!(staging_rep.failures.is_empty());

    let report = calculate_conflicts(&env.staging(), &env.mods_dir).unwrap();
    assert_eq!(report.new_files, 1);
    assert_eq!(report.updates, 0);
    assert_eq!(report.exact_matches, 0);

    // 2. Instalação
    let (inst, skip) =
        execute_installation(&report, &env.mods_dir, MANAGED_BASE_DIR, &[env.mods_dir.clone()]).unwrap();
    assert_eq!(inst, 1);
    assert_eq!(skip, 0);

    let instalado = env.managed().join("Criador_Cabelo_v1/Criador_Cabelo.package");
    assert!(instalado.exists());

    // 3. Verificação SHA-256 antes e depois: garantia de zero corrupção
    let sha_instalado = file_sha256(&instalado);
    assert_eq!(sha_orig, sha_instalado, "hash SHA-256 deve ser rigorosamente idêntico");

    // Valida que o DBPF pode ser lido perfeitamente
    let mut f = File::open(&instalado).unwrap();
    let (hdr, idx) = DBPFReader::read_index(&mut f).unwrap();
    assert_eq!(hdr.major, 2);
    assert_eq!(idx.len(), 2);
}

#[test]
fn auditoria_atualizacao_com_backup_preservando_hashes() {
    let env = AuditEnv::new();

    // Versão 1 instalada
    let v1_bytes = make_real_dbpf_package(
        &env.managed().join("ModExemplo/item.package"),
        &[(1, b"VERSAO_1_ORIGINAL")],
    );
    let v1_sha = sha256_digest(&v1_bytes);

    // Versão 2 no zip
    let v2_source = env.downloads_dir.join("ModExemplo_v2.package");
    let v2_bytes = make_real_dbpf_package(&v2_source, &[(1, b"VERSAO_2_ATUALIZADA_COMPLETA")]);
    let v2_sha = sha256_digest(&v2_bytes);
    let zip_v2 = env.downloads_dir.join("ModExemplo.zip");
    make_zip_archive(&zip_v2, &[("item.package", &v2_bytes)]);

    // Instalação da atualização
    prepare_staging(&[zip_v2], &env.staging()).unwrap();
    let report = calculate_conflicts(&env.staging(), &env.mods_dir).unwrap();
    assert_eq!(report.updates, 1);

    execute_installation(&report, &env.mods_dir, MANAGED_BASE_DIR, &[env.mods_dir.clone()]).unwrap();

    // O arquivo instalado deve ter o hash da v2
    let instalado = env.managed().join("ModExemplo/item.package");
    assert_eq!(file_sha256(&instalado), v2_sha);

    // O backup gerado em .s4suite_backups deve ter exatamente o hash da v1
    let backups: Vec<_> = fs::read_dir(env.mods_dir.join(".s4suite_backups"))
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(backups.len(), 1);
    assert_eq!(file_sha256(&backups[0].path()), v1_sha, "backup deve preservar exatamente a v1");
}

#[test]
fn auditoria_merger_multipartes_e_hashes_de_recursos() {
    let env = AuditEnv::new();
    let out_dir = env.mods_dir.join("Unificados");

    let r1 = b"RECURSO_A_PAYLOAD_PARA_TESTE_DE_HASH";
    let r2 = b"RECURSO_B_PAYLOAD_PARA_TESTE_DE_HASH";
    let r3 = b"RECURSO_C_PAYLOAD_PARA_TESTE_DE_HASH";
    let r4 = b"RECURSO_D_PAYLOAD_PARA_TESTE_DE_HASH";

    let p1 = env.mods_dir.join("Packs/pack1.package");
    let p2 = env.mods_dir.join("Packs/pack2.package");
    make_real_dbpf_package(&p1, &[(1, r1), (2, r2)]);
    make_real_dbpf_package(&p2, &[(3, r3), (4, r4)]);

    // Força divisão em múltiplas partes (limite de 100 bytes força particionamento)
    let task = MergeTask {
        input_files: vec![p1, p2],
        output_dir: out_dir.clone(),
        max_size_bytes: 100,
        name: Some("MultiHash".to_string()),
    };

    let report = merge_sims4_packages(&task, |_, _, _| {}).unwrap();
    assert!(report.success);
    assert!(report.output_packages.len() >= 2);
    let residuais: Vec<_> = fs::read_dir(&out_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains(".s4tmp"))
        .collect();
    assert!(residuais.is_empty(), "nenhum arquivo .s4tmp pode restar na publicação");

    // Valida que cada parte é DBPF válido
    let mut total_recursos = 0;
    for part in &report.output_packages {
        let mut f = File::open(part).unwrap();
        let (hdr, idx) = DBPFReader::read_index(&mut f).unwrap();
        assert_eq!(hdr.major, 2);
        total_recursos += idx.len();
    }
    assert_eq!(total_recursos, 4, "todos os 4 recursos devem ser preservados");
}

#[test]
fn auditoria_blindagem_da_raiz_mods_e_caminhos_externos() {
    let env = AuditEnv::new();

    // 1. Tentar excluir a raiz Mods deve falhar categoricamente
    let res = safe_remove_dir_all(&env.mods_dir, &[env.mods_dir.clone()]);
    assert!(res.is_err(), "excluir a raiz de Mods deve ser categoricamente bloqueado");
    assert!(env.mods_dir.exists(), "raiz Mods deve permanecer intacta");

    // 2. Tentar excluir pasta pai com traversal
    let pai_traversal = env.mods_dir.join("../");
    assert!(!is_path_inside(&pai_traversal, &env.mods_dir));
    assert!(is_safe_to_delete(&pai_traversal, &[env.mods_dir.clone()]).is_err());

    // 3. Symlink apontando para fora de Mods
    let outside_file = env._root.path().join("sistema_externo.txt");
    fs::write(&outside_file, b"DADOS_CRITICOS_EXTERNOS").unwrap();
    let link_em_mods = env.mods_dir.join("link_perigoso.txt");

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&outside_file, &link_em_mods).unwrap();
        assert!(link_em_mods.exists());

        // A exclusão segura do link deve remover apenas o link, NUNCA o arquivo de fora!
        let del_res = safe_remove_file(&link_em_mods, &[env.mods_dir.clone()]);
        assert!(del_res.is_ok());
        assert!(fs::symlink_metadata(&link_em_mods).is_err(), "o symlink foi removido");
        assert!(outside_file.exists(), "o arquivo externo continua intacto");
        assert_eq!(fs::read(&outside_file).unwrap(), b"DADOS_CRITICOS_EXTERNOS");
    }
}

#[test]
fn auditoria_transferencia_e_seguranca_no_organizador() {
    let env = AuditEnv::new();
    let pasta_a = env.mods_dir.join("CategoriaA");
    let pasta_b = env.mods_dir.join("CategoriaB");
    fs::create_dir_all(&pasta_a).unwrap();
    fs::create_dir_all(&pasta_b).unwrap();

    let mod_file = pasta_a.join("meu_mod.package");
    fs::write(&mod_file, b"CONTEUDO_PARA_MOVER").unwrap();

    // Mover arquivo de CategoriaA para CategoriaB
    let outcome = transfer_items(&[mod_file.clone()], &pasta_b, TransferMode::Move, &env.mods_dir).unwrap();
    assert_eq!(outcome.done, 1);
    assert!(outcome.failures.is_empty());
    assert!(!mod_file.exists(), "origem foi movida");
    assert!(pasta_b.join("meu_mod.package").exists(), "destino recebeu o arquivo");

    // Tentar mover a raiz Mods para dentro de uma subpasta deve ser recusado em outcome.failures
    let outcome_err = transfer_items(&[env.mods_dir.clone()], &pasta_b, TransferMode::Move, &env.mods_dir).unwrap();
    assert_eq!(outcome_err.done, 0, "nenhum item pode ser movido ao tentar mover a raiz");
    assert_eq!(outcome_err.failures.len(), 1, "mover a raiz deve falhar");
    assert!(outcome_err.failures[0].1.contains("raiz"), "mensagem deve indicar proteção da raiz");
    assert!(env.mods_dir.exists(), "raiz deve permanecer intacta");
}
