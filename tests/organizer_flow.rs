//! Testes do organizador: ativação/desativação de mods, duplicatas, lixo e
//! as garantias de segurança das remoções em lote.

use s4suite::engine::disabled::DisabledManager;
use s4suite::engine::installer::{BACKUP_DIR_NAME, STAGING_DIR_NAME};
use s4suite::engine::organizer::{
    create_folder, detect_duplicates, filter_tree, find_junk_files, remove_entries, remove_files,
    rename_item, sanitize_entry_name, scan_mods_tree, transfer_items, TransferMode,
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

// --- Explorador de arquivos: criar, mover, copiar, renomear, excluir ---

#[test]
fn nome_de_pasta_perde_os_caracteres_proibidos() {
    assert_eq!(sanitize_entry_name("  Meus Mods  ").unwrap(), "Meus Mods");
    assert_eq!(sanitize_entry_name("CC/Cabelos").unwrap(), "CCCabelos");
    assert_eq!(sanitize_entry_name(r#"a:b*c?d"e<f>g|h"#).unwrap(), "abcdefgh");
    // Só separador e ponto não formam um nome: melhor recusar do que criar uma
    // pasta invisível ou escapar do diretório.
    assert!(sanitize_entry_name("///").is_none());
    assert!(sanitize_entry_name("..").is_none());
    assert!(sanitize_entry_name("   ").is_none());
}

#[test]
fn nova_pasta_e_criada_dentro_de_mods() {
    let fx = Fixture::new();
    let criada = create_folder(&fx.mods_dir, "Cabelos", &fx.mods_dir).unwrap();

    assert!(criada.is_dir());
    assert_eq!(criada, fx.mods_dir.join("Cabelos"));

    let repetida = create_folder(&fx.mods_dir, "Cabelos", &fx.mods_dir);
    assert!(repetida.is_err(), "não deveria sobrescrever pasta existente");
}

#[test]
fn nova_pasta_recusa_destino_fora_de_mods() {
    let fx = Fixture::new();
    let fora = fx.mods_dir.parent().unwrap().join("Outra");
    fs::create_dir_all(&fora).unwrap();

    assert!(create_folder(&fora, "Teste", &fx.mods_dir).is_err());
    assert!(!fora.join("Teste").exists());
}

#[test]
fn renomear_mantem_o_item_na_mesma_pasta() {
    let fx = Fixture::new();
    let original = fx.mods_dir.join("Categoria/mod_antigo.package");
    write_file(&original, b"conteudo");

    let novo = rename_item(&original, "mod novo.package", &fx.mods_dir).unwrap();

    assert_eq!(novo, fx.mods_dir.join("Categoria/mod novo.package"));
    assert!(!original.exists());
    assert_eq!(fs::read(&novo).unwrap(), b"conteudo");
}

#[test]
fn renomear_recusa_nome_ja_usado_e_a_raiz_de_mods() {
    let fx = Fixture::new();
    let a = fx.mods_dir.join("a.package");
    let b = fx.mods_dir.join("b.package");
    write_file(&a, b"a");
    write_file(&b, b"b");

    assert!(rename_item(&a, "b.package", &fx.mods_dir).is_err());
    assert!(a.exists() && b.exists());

    // Renomear a raiz levaria a biblioteca inteira junto.
    assert!(rename_item(&fx.mods_dir, "Outra Coisa", &fx.mods_dir).is_err());
    assert!(fx.mods_dir.exists());
}

#[test]
fn mover_leva_o_arquivo_para_a_pasta_escolhida() {
    let fx = Fixture::new();
    let origem = fx.mods_dir.join("mod.package");
    write_file(&origem, b"conteudo");
    let destino = fx.mods_dir.join("Cabelos");
    fs::create_dir_all(&destino).unwrap();

    let outcome =
        transfer_items(&[origem.clone()], &destino, TransferMode::Move, &fx.mods_dir).unwrap();

    assert_eq!(outcome.done, 1);
    assert!(outcome.failures.is_empty());
    assert!(!origem.exists());
    assert_eq!(fs::read(destino.join("mod.package")).unwrap(), b"conteudo");
}

#[test]
fn copiar_preserva_a_origem_e_copia_pasta_inteira() {
    let fx = Fixture::new();
    let pasta = fx.mods_dir.join("PackCompleto");
    write_file(&pasta.join("a.package"), b"a");
    write_file(&pasta.join("sub/b.package"), b"b");
    let destino = fx.mods_dir.join("Backup");
    fs::create_dir_all(&destino).unwrap();

    let outcome =
        transfer_items(&[pasta.clone()], &destino, TransferMode::Copy, &fx.mods_dir).unwrap();

    assert_eq!(outcome.done, 1);
    assert!(pasta.join("a.package").exists(), "a origem não deveria sumir numa cópia");
    assert_eq!(fs::read(destino.join("PackCompleto/a.package")).unwrap(), b"a");
    assert_eq!(fs::read(destino.join("PackCompleto/sub/b.package")).unwrap(), b"b");
}

#[test]
fn transferencia_nao_sobrescreve_arquivo_de_mesmo_nome_no_destino() {
    let fx = Fixture::new();
    let origem = fx.mods_dir.join("Origem/mod.package");
    write_file(&origem, b"nova versao");
    let destino = fx.mods_dir.join("Destino");
    write_file(&destino.join("mod.package"), b"versao existente");

    transfer_items(&[origem], &destino, TransferMode::Move, &fx.mods_dir).unwrap();

    assert_eq!(fs::read(destino.join("mod.package")).unwrap(), b"versao existente");
    assert_eq!(fs::read(destino.join("mod_1.package")).unwrap(), b"nova versao");
}

/// Mover uma pasta para dentro dela mesma entraria em laço criando cópias
/// dentro de cópias até encher o disco.
#[test]
fn mover_pasta_para_dentro_de_si_mesma_e_recusado() {
    let fx = Fixture::new();
    let pasta = fx.mods_dir.join("Pack");
    let dentro = pasta.join("sub");
    write_file(&pasta.join("a.package"), b"a");
    fs::create_dir_all(&dentro).unwrap();

    let outcome =
        transfer_items(&[pasta.clone()], &dentro, TransferMode::Move, &fx.mods_dir).unwrap();

    assert_eq!(outcome.done, 0);
    assert_eq!(outcome.failures.len(), 1);
    assert!(pasta.join("a.package").exists());
}

#[test]
fn transferencia_recusa_origem_fora_de_mods() {
    let fx = Fixture::new();
    let fora = fx.mods_dir.parent().unwrap().join("documento.package");
    write_file(&fora, b"nao sou um mod");
    let destino = fx.mods_dir.join("Destino");
    fs::create_dir_all(&destino).unwrap();

    let outcome =
        transfer_items(&[fora.clone()], &destino, TransferMode::Move, &fx.mods_dir).unwrap();

    assert_eq!(outcome.done, 0);
    assert_eq!(outcome.failures.len(), 1);
    assert!(fora.exists(), "arquivo fora de Mods foi movido");
}

/// O jogo só carrega `.ts4script` até um nível de subpasta. Mover um script
/// para mais fundo o desliga sem que o jogo reclame de nada.
#[test]
fn mover_script_fundo_demais_gera_aviso_sem_falhar() {
    let fx = Fixture::new();
    let script = fx.mods_dir.join("mod.ts4script");
    write_file(&script, b"script");
    let destino = fx.mods_dir.join("Categoria/Autor");
    fs::create_dir_all(&destino).unwrap();

    let outcome =
        transfer_items(&[script], &destino, TransferMode::Move, &fx.mods_dir).unwrap();

    assert_eq!(outcome.done, 1, "o arquivo é movido mesmo assim");
    assert_eq!(outcome.script_warnings.len(), 1);
    assert!(outcome.script_warnings[0].ends_with("mod.ts4script"));
}

#[test]
fn exclusao_em_lote_apaga_arquivos_e_pastas() {
    let fx = Fixture::new();
    let arquivo = fx.mods_dir.join("solto.package");
    let pasta = fx.mods_dir.join("Pack");
    write_file(&arquivo, &vec![0u8; 500]);
    write_file(&pasta.join("a.package"), &vec![0u8; 300]);
    write_file(&pasta.join("b.package"), &vec![0u8; 200]);

    let outcome = remove_entries(
        &[arquivo.clone(), pasta.clone()],
        &[fx.mods_dir.clone()],
    );

    assert_eq!(outcome.removed, 2);
    assert_eq!(outcome.freed_bytes, 1000, "o tamanho da pasta é a soma do conteúdo");
    assert!(!arquivo.exists() && !pasta.exists());
}

#[test]
fn exclusao_em_lote_recusa_pasta_fora_de_mods() {
    let fx = Fixture::new();
    let fora = fx.mods_dir.parent().unwrap().join("Documentos");
    write_file(&fora.join("importante.txt"), b"nao me apague");

    let outcome = remove_entries(&[fora.clone()], &[fx.mods_dir.clone()]);

    assert_eq!(outcome.removed, 0);
    assert_eq!(outcome.failures.len(), 1);
    assert!(fora.exists());
}

// --- Busca na árvore ---

#[test]
fn busca_traz_o_acerto_com_as_pastas_que_levam_ate_ele() {
    let fx = Fixture::new();
    write_file(&fx.mods_dir.join("Cabelos/Autor/cabelo_longo.package"), b"x");
    write_file(&fx.mods_dir.join("Roupas/vestido.package"), b"y");

    let nodes = scan_mods_tree(&fx.mods_dir);
    let achados = filter_tree(&nodes, "cabelo_longo");

    let nomes: Vec<&str> = achados.iter().map(|n| n.name.as_str()).collect();
    assert!(nomes.contains(&"cabelo_longo.package"));
    // Sem os ancestrais, o resultado ficaria pendurado sob pastas que sumiram.
    assert!(nomes.contains(&"Cabelos"));
    assert!(nomes.contains(&"Autor"));
    assert!(!nomes.contains(&"vestido.package"));
    assert!(!nomes.contains(&"Roupas"));
}

#[test]
fn busca_vazia_devolve_a_arvore_inteira() {
    let fx = Fixture::new();
    write_file(&fx.mods_dir.join("a.package"), b"a");
    write_file(&fx.mods_dir.join("Pasta/b.package"), b"b");

    let nodes = scan_mods_tree(&fx.mods_dir);
    assert_eq!(filter_tree(&nodes, "   ").len(), nodes.len());
}

#[test]
fn busca_ignora_maiusculas() {
    let fx = Fixture::new();
    write_file(&fx.mods_dir.join("MeuMod.package"), b"x");

    let nodes = scan_mods_tree(&fx.mods_dir);
    assert_eq!(filter_tree(&nodes, "meumod").len(), 1);
}

// --- Painel de desativados ---

#[test]
fn painel_lista_desativados_com_motivo_e_observacao() {
    let fx = Fixture::new();
    let mod_a = fx.mods_dir.join("mod_a.package");
    write_file(&mod_a, b"a");

    let mgr = fx.manager();
    mgr.disable_mod(&mod_a, &fx.mods_dir, "suspected_bug", "trava no CAS", &[fx.mods_dir.clone()])
        .unwrap();

    let lista = mgr.list_disabled(&fx.mods_dir);
    assert_eq!(lista.len(), 1);
    assert_eq!(lista[0].name, "mod_a.package");
    assert_eq!(lista[0].reason, "suspected_bug");
    assert_eq!(lista[0].note, "trava no CAS");
    assert!(!lista[0].missing);
}

/// Um `.disabled` renomeado à mão fora do app precisa aparecer, senão fica
/// invisível e sem como ser reativado pela interface.
#[test]
fn desativado_por_fora_do_app_tambem_e_listado() {
    let fx = Fixture::new();
    write_file(&fx.mods_dir.join("Categoria/na_mao.package.disabled"), b"x");

    let lista = fx.manager().list_disabled(&fx.mods_dir);

    assert_eq!(lista.len(), 1);
    assert_eq!(lista[0].name, "na_mao.package");
    assert_eq!(lista[0].reason, "manual");
}

#[test]
fn entrada_orfa_do_manifesto_e_marcada_e_pode_ser_limpa() {
    let fx = Fixture::new();
    let mod_a = fx.mods_dir.join("mod_a.package");
    write_file(&mod_a, b"a");

    let mgr = fx.manager();
    let desativado = mgr
        .disable_mod(&mod_a, &fx.mods_dir, "manual", "", &[fx.mods_dir.clone()])
        .unwrap();
    fs::remove_file(&desativado).unwrap();

    let lista = mgr.list_disabled(&fx.mods_dir);
    assert_eq!(lista.len(), 1);
    assert!(lista[0].missing, "arquivo sumiu do disco, deveria vir marcado");

    assert_eq!(mgr.prune_missing().unwrap(), 1);
    assert!(mgr.list_disabled(&fx.mods_dir).is_empty());
}

#[test]
fn painel_de_desativados_ignora_as_pastas_internas() {
    let fx = Fixture::new();
    write_file(&fx.mods_dir.join(BACKUP_DIR_NAME).join("antigo.package.disabled"), b"x");
    write_file(&fx.mods_dir.join("real.package.disabled"), b"y");

    let lista = fx.manager().list_disabled(&fx.mods_dir);

    assert_eq!(lista.len(), 1);
    assert_eq!(lista[0].name, "real.package");
}
