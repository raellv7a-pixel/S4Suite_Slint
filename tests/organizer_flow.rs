//! Testes do organizador: ativação/desativação de mods, duplicatas, lixo e
//! as garantias de segurança das remoções em lote.

use s4suite::engine::disabled::DisabledManager;
use s4suite::engine::installer::{BACKUP_DIR_NAME, STAGING_DIR_NAME};
use s4suite::engine::organizer::{
    detect_duplicates, find_junk_files, remove_files, scan_mods_tree,
};
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn write_file(path: &Path, content: &[u8]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, content).unwrap();
}

struct Fixture {
    _root: TempDir,
    mods_dir: PathBuf,
    config_dir: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = TempDir::new().unwrap();
        let mods_dir = root.path().join("Mods");
        let config_dir = root.path().join("config");
        fs::create_dir_all(&mods_dir).unwrap();
        Self { _root: root, mods_dir, config_dir }
    }

    fn manager(&self) -> DisabledManager {
        DisabledManager::new(&self.config_dir)
    }
}

#[test]
fn desativar_e_reativar_devolve_o_mod_ao_lugar_de_origem() {
    let fx = Fixture::new();
    let original = fx.mods_dir.join("Categoria/meu_mod.package");
    write_file(&original, b"conteudo do mod");

    let mgr = fx.manager();
    let disabled = mgr
        .disable_mod(&original, &fx.mods_dir, "manual", "teste", &[fx.mods_dir.clone()])
        .unwrap();

    assert!(!original.exists(), "o arquivo original deveria ter sido renomeado");
    assert!(disabled.to_string_lossy().ends_with(".disabled"));

    let restored = mgr.enable_mod(&disabled, &fx.mods_dir).unwrap();

    assert!(!disabled.exists());
    assert_eq!(restored, original, "o mod deveria voltar ao caminho exato de origem");
    assert_eq!(fs::read(&restored).unwrap(), b"conteudo do mod");
}

#[test]
fn desativar_recusa_arquivo_fora_da_pasta_mods() {
    let fx = Fixture::new();
    let fora = fx._root.path().join("Downloads/intruso.package");
    write_file(&fora, b"fora do escopo");

    let resultado = fx
        .manager()
        .disable_mod(&fora, &fx.mods_dir, "manual", "", &[fx.mods_dir.clone()]);

    assert!(resultado.is_err(), "mods fora de Mods não podem ser desativados");
    assert!(fora.exists(), "o arquivo recusado não pode ser movido");
}

/// Os backups do instalador são cópias byte-a-byte de mods instalados. Se a
/// varredura de duplicatas os enxergasse, todo mod atualizado viraria uma
/// "duplicata" e o usuário seria levado a apagar o próprio backup.
#[test]
fn backups_internos_nao_viram_duplicatas() {
    let fx = Fixture::new();
    let conteudo = b"exatamente o mesmo conteudo";

    write_file(&fx.mods_dir.join("meu_mod.package"), conteudo);
    write_file(&fx.mods_dir.join(BACKUP_DIR_NAME).join("meu_mod.package"), conteudo);
    write_file(&fx.mods_dir.join(STAGING_DIR_NAME).join("x/meu_mod.package"), conteudo);

    let grupos = detect_duplicates(&fx.mods_dir);

    assert!(grupos.is_empty(), "staging e backups não são conteúdo do usuário");
}

#[test]
fn duplicatas_reais_sao_agrupadas_e_uma_copia_e_preservada() {
    let fx = Fixture::new();
    let conteudo = b"mod duplicado em tres lugares";

    write_file(&fx.mods_dir.join("mod.package"), conteudo);
    write_file(&fx.mods_dir.join("Pasta/mod.package"), conteudo);
    write_file(&fx.mods_dir.join("Pasta/Subpasta/mod.package"), conteudo);
    write_file(&fx.mods_dir.join("outro.package"), b"conteudo diferente");

    let grupos = detect_duplicates(&fx.mods_dir);

    assert_eq!(grupos.len(), 1);
    let grupo = &grupos[0];
    assert_eq!(grupo.file_paths.len(), 3);

    // A cópia mais rasa é a preservada; as outras duas somem.
    assert_eq!(*grupo.keeper(), fx.mods_dir.join("mod.package"));
    assert_eq!(grupo.redundant().len(), 2);
    assert!(!grupo.redundant().contains(grupo.keeper()));
}

#[test]
fn varredura_de_lixo_ignora_pastas_internas() {
    let fx = Fixture::new();
    write_file(&fx.mods_dir.join("leiame.txt"), b"instrucoes");
    write_file(&fx.mods_dir.join("preview.png"), b"imagem");
    write_file(&fx.mods_dir.join("mod.package"), b"mod de verdade");
    write_file(&fx.mods_dir.join(BACKUP_DIR_NAME).join("nota.txt"), b"interno");

    let lixo = find_junk_files(&fx.mods_dir);

    assert_eq!(lixo.len(), 2);
    assert!(lixo.iter().all(|p| !p.to_string_lossy().contains(BACKUP_DIR_NAME)));
    assert!(lixo.iter().all(|p| p.extension().unwrap() != "package"));
}

#[test]
fn remocao_em_lote_recusa_alvos_fora_das_raizes_permitidas() {
    let fx = Fixture::new();
    let dentro = fx.mods_dir.join("lixo.txt");
    let fora = fx._root.path().join("documento_importante.txt");
    write_file(&dentro, b"pode apagar");
    write_file(&fora, b"NAO pode apagar");

    let outcome = remove_files(&[dentro.clone(), fora.clone()], &[fx.mods_dir.clone()]);

    assert_eq!(outcome.removed, 1);
    assert_eq!(outcome.failures.len(), 1);
    assert!(!dentro.exists());
    assert!(fora.exists(), "arquivo fora da raiz permitida foi apagado");
}

#[test]
fn remocao_em_lote_contabiliza_espaco_liberado() {
    let fx = Fixture::new();
    let a = fx.mods_dir.join("a.txt");
    let b = fx.mods_dir.join("b.txt");
    write_file(&a, &vec![0u8; 1000]);
    write_file(&b, &vec![0u8; 2000]);

    let outcome = remove_files(&[a, b], &[fx.mods_dir.clone()]);

    assert_eq!(outcome.removed, 2);
    assert_eq!(outcome.freed_bytes, 3000);
}

#[test]
fn arvore_de_mods_nao_expoe_pastas_internas_e_marca_desativados() {
    let fx = Fixture::new();
    write_file(&fx.mods_dir.join("ativo.package"), b"a");
    write_file(&fx.mods_dir.join("inativo.package.disabled"), b"b");
    write_file(&fx.mods_dir.join(STAGING_DIR_NAME).join("temp.package"), b"c");

    let nodes = scan_mods_tree(&fx.mods_dir);
    let nomes: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();

    assert!(nomes.contains(&"ativo.package"));
    assert!(nomes.contains(&"inativo.package.disabled"));
    assert!(!nomes.contains(&STAGING_DIR_NAME), "pasta interna vazou para a árvore");
    assert!(!nomes.contains(&"temp.package"));

    let inativo = nodes.iter().find(|n| n.name.ends_with(".disabled")).unwrap();
    assert!(inativo.disabled);
}
