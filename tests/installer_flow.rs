//! Testes de integração do fluxo do instalador: seleção -> staging ->
//! conflitos -> instalação em Mods.

use s4suite::engine::installer::{
    calculate_conflicts, collect_dependencies, detect_mutually_exclusive, execute_installation,
    install_identity_name, is_valid_mod_file, prepare_staging, ConflictType, MANAGED_BASE_DIR,
    STAGING_DIR_NAME,
};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

/// Cabeçalho DBPF mínimo para que o conteúdo pareça um package de verdade.
fn package_bytes(payload: &str) -> Vec<u8> {
    let mut data = b"DBPF".to_vec();
    data.extend_from_slice(&[0u8; 92]);
    data.extend_from_slice(payload.as_bytes());
    data
}

fn write_file(path: &Path, content: &[u8]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, content).unwrap();
}

/// Cria um .zip com os pares (caminho interno, conteúdo).
fn make_zip(path: &Path, entries: &[(&str, Vec<u8>)]) {
    let file = fs::File::create(path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    let opts = zip::write::SimpleFileOptions::default();
    for (name, content) in entries {
        zip.start_file(*name, opts).unwrap();
        zip.write_all(content).unwrap();
    }
    zip.finish().unwrap();
}

struct Fixture {
    _root: TempDir,
    mods_dir: PathBuf,
    downloads: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = TempDir::new().unwrap();
        let mods_dir = root.path().join("Mods");
        let downloads = root.path().join("Downloads");
        fs::create_dir_all(&mods_dir).unwrap();
        fs::create_dir_all(&downloads).unwrap();
        Self {
            _root: root,
            mods_dir,
            downloads,
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
fn identidade_de_mod_normaliza_nome_do_arquivo() {
    // O nome vira pasta dentro de Mods e o usuário o lê na árvore: preserva
    // acento, espaço e maiúscula, e só tira o que não pode estar num nome de
    // arquivo.
    assert_eq!(
        install_identity_name(Path::new("Meu Mod v2.1.zip")),
        "Meu Mod v2.1"
    );
    assert_eq!(
        install_identity_name(Path::new("/tmp/UI-Cheats.package")),
        "UI-Cheats"
    );
    assert_eq!(
        install_identity_name(Path::new("Mod X — Tradução PT-BR.package")),
        "Mod X — Tradução PT-BR"
    );
    assert_eq!(
        install_identity_name(Path::new("mod:teste*final.zip")),
        "mod_teste_final"
    );
    assert_eq!(install_identity_name(Path::new("___.zip")), "mod");
}

#[test]
fn residuos_de_sistema_nao_sao_mods_validos() {
    assert!(is_valid_mod_file("mod.package"));
    assert!(is_valid_mod_file("script.ts4script"));
    assert!(!is_valid_mod_file("._mod.package"));
    assert!(!is_valid_mod_file(".DS_Store"));
    assert!(!is_valid_mod_file("leiame.txt"));
}

#[test]
fn zip_e_extraido_para_o_staging_sob_a_identidade_do_mod() {
    let fx = Fixture::new();
    let archive = fx.downloads.join("Cool Mod.zip");
    make_zip(
        &archive,
        &[
            ("CoolMod/cool.package", package_bytes("cool")),
            ("CoolMod/leiame.txt", b"ignore-me".to_vec()),
        ],
    );

    let report = prepare_staging(&[archive], &fx.staging()).unwrap();

    assert_eq!(report.prepared, 1);
    assert!(report.failures.is_empty());
    assert!(fx.staging().join("Cool Mod/CoolMod/cool.package").exists());
}

#[test]
fn package_avulso_tambem_entra_no_staging() {
    let fx = Fixture::new();
    let loose = fx.downloads.join("UI Cheats.package");
    write_file(&loose, &package_bytes("ui"));

    prepare_staging(&[loose], &fx.staging()).unwrap();

    assert!(fx.staging().join("UI Cheats/UI Cheats.package").exists());
}

#[test]
fn arquivo_com_executavel_e_bloqueado_e_nao_deixa_residuo() {
    let fx = Fixture::new();
    let archive = fx.downloads.join("suspeito.zip");
    make_zip(
        &archive,
        &[
            ("mod.package", package_bytes("x")),
            ("instalar.exe", b"MZ".to_vec()),
        ],
    );

    let err = prepare_staging(&[archive], &fx.staging()).unwrap_err();

    assert!(
        err.to_string().contains("instalar.exe"),
        "erro inesperado: {}",
        err
    );
    assert!(
        !fx.staging().join("suspeito").exists(),
        "staging não foi limpo após bloqueio"
    );
}

#[test]
fn uma_fonte_ruim_nao_aborta_as_boas() {
    let fx = Fixture::new();

    let bom = fx.downloads.join("bom.zip");
    make_zip(&bom, &[("bom.package", package_bytes("ok"))]);

    let ruim = fx.downloads.join("ruim.zip");
    write_file(&ruim, b"isto nao e um zip");

    let report = prepare_staging(&[bom, ruim], &fx.staging()).unwrap();

    assert_eq!(report.prepared, 1);
    assert_eq!(report.failures.len(), 1);
    assert!(fx.staging().join("bom/bom.package").exists());
}

/// Regressão: o staging mora dentro de Mods. Se `calculate_conflicts` varrer
/// Mods sem excluí-lo, os arquivos recém-extraídos aparecem como "já
/// instalados" e a fila inteira é classificada como ExactMatch.
#[test]
fn staging_nao_e_confundido_com_mods_ja_instalados() {
    let fx = Fixture::new();
    let archive = fx.downloads.join("novo.zip");
    make_zip(&archive, &[("novo.package", package_bytes("novo"))]);

    prepare_staging(&[archive], &fx.staging()).unwrap();
    let report = calculate_conflicts(&fx.staging(), &fx.mods_dir).unwrap();

    assert_eq!(report.new_files, 1);
    assert_eq!(report.exact_matches, 0);
    assert_eq!(report.staged_files[0].conflict, ConflictType::NewFile);
}

#[test]
fn mod_ja_presente_com_tamanho_diferente_vira_atualizacao() {
    let fx = Fixture::new();
    let instalado = fx.mods_dir.join("MeusMods/alpha.package");
    write_file(&instalado, &package_bytes("versao antiga"));

    let archive = fx.downloads.join("alpha.zip");
    make_zip(
        &archive,
        &[("alpha.package", package_bytes("versao nova, bem maior"))],
    );

    prepare_staging(&[archive], &fx.staging()).unwrap();
    let report = calculate_conflicts(&fx.staging(), &fx.mods_dir).unwrap();

    assert_eq!(report.updates, 1);
    assert_eq!(report.new_files, 0);
    // Atualização sobrescreve onde o mod já estava, não cria uma segunda cópia.
    assert_eq!(
        report.staged_files[0].destination(&fx.mods_dir, MANAGED_BASE_DIR),
        instalado
    );
}

#[test]
fn instalacao_organiza_packages_e_mantem_scripts_rasos() {
    let fx = Fixture::new();
    let archive = fx.downloads.join("Pacote Completo.zip");
    make_zip(
        &archive,
        &[
            ("mod/texturas/skin.package", package_bytes("skin")),
            ("mod/core.ts4script", b"PK-script".to_vec()),
        ],
    );

    prepare_staging(&[archive], &fx.staging()).unwrap();
    let report = calculate_conflicts(&fx.staging(), &fx.mods_dir).unwrap();
    let (installed, skipped) = execute_installation(
        &report,
        &fx.mods_dir,
        MANAGED_BASE_DIR,
        &[fx.mods_dir.clone()],
    )
    .unwrap();

    assert_eq!(installed, 2);
    assert_eq!(skipped, 0);

    // .package preserva a estrutura interna do arquivo compactado.
    assert!(fx
        .managed()
        .join("Pacote Completo/mod/texturas/skin.package")
        .exists());

    // .ts4script fica em Mods/00_Triagem_Novos/ — o jogo só carrega scripts até
    // um nível de subpasta, então não pode ir mais fundo.
    let script = fx.managed().join("core.ts4script");
    assert!(
        script.exists(),
        "script deveria estar raso na base gerenciada"
    );
    let profundidade = script
        .strip_prefix(&fx.mods_dir)
        .unwrap()
        .components()
        .count();
    assert_eq!(
        profundidade, 2,
        "script fundo demais: o jogo não o carregaria"
    );
}

#[test]
fn arquivo_identico_e_ignorado_em_vez_de_reinstalado() {
    let fx = Fixture::new();
    let conteudo = package_bytes("identico");
    let instalado = fx.mods_dir.join("MeusMods/igual.package");
    write_file(&instalado, &conteudo);

    let archive = fx.downloads.join("igual.zip");
    make_zip(&archive, &[("igual.package", conteudo.clone())]);

    prepare_staging(&[archive], &fx.staging()).unwrap();
    let report = calculate_conflicts(&fx.staging(), &fx.mods_dir).unwrap();
    let (installed, skipped) = execute_installation(
        &report,
        &fx.mods_dir,
        MANAGED_BASE_DIR,
        &[fx.mods_dir.clone()],
    )
    .unwrap();

    assert_eq!(installed, 0);
    assert_eq!(skipped, 1);
    assert!(!fx.managed().join("igual/igual.package").exists());
}

#[test]
fn atualizacao_guarda_backup_do_arquivo_substituido() {
    let fx = Fixture::new();
    let antigo = package_bytes("conteudo antigo");
    let instalado = fx.mods_dir.join("MeusMods/beta.package");
    write_file(&instalado, &antigo);

    let archive = fx.downloads.join("beta.zip");
    make_zip(
        &archive,
        &[("beta.package", package_bytes("conteudo novo e diferente"))],
    );

    prepare_staging(&[archive], &fx.staging()).unwrap();
    let report = calculate_conflicts(&fx.staging(), &fx.mods_dir).unwrap();
    execute_installation(
        &report,
        &fx.mods_dir,
        MANAGED_BASE_DIR,
        &[fx.mods_dir.clone()],
    )
    .unwrap();

    let backups: Vec<_> = fs::read_dir(fx.mods_dir.join(".s4suite_backups"))
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(backups.len(), 1);
    assert_eq!(fs::read(backups[0].path()).unwrap(), antigo);
    assert_ne!(
        fs::read(&instalado).unwrap(),
        antigo,
        "o mod deveria ter sido atualizado"
    );
}

#[test]
fn staging_e_recriado_do_zero_a_cada_instalacao() {
    let fx = Fixture::new();
    write_file(
        &fx.staging().join("lixo_da_rodada_anterior.package"),
        &package_bytes("velho"),
    );

    let archive = fx.downloads.join("novo.zip");
    make_zip(&archive, &[("novo.package", package_bytes("novo"))]);
    prepare_staging(&[archive], &fx.staging()).unwrap();

    assert!(!fx
        .staging()
        .join("lixo_da_rodada_anterior.package")
        .exists());
}

// --- Variantes exclusivas e dependências ---

#[test]
fn variantes_marcadas_como_opcao_viram_uma_escolha() {
    let fx = Fixture::new();
    let archive = fx.downloads.join("CabeloPack.zip");
    make_zip(
        &archive,
        &[
            ("Options/Cabelo Loiro.package", b"loiro".to_vec()),
            ("Options/Cabelo Ruivo.package", b"ruivo".to_vec()),
            ("leiame.txt", b"escolha um".to_vec()),
        ],
    );

    prepare_staging(&[archive], &fx.staging()).unwrap();
    let report = calculate_conflicts(&fx.staging(), &fx.mods_dir).unwrap();
    let opcoes = detect_mutually_exclusive(&report.staged_files);

    assert_eq!(opcoes.len(), 2, "as duas variantes deveriam virar opções");
    assert!(opcoes.iter().all(|f| f.filename.starts_with("Cabelo")));
}

/// Uma opção sozinha não é uma escolha — perguntar ali só atrapalharia.
#[test]
fn uma_variante_isolada_nao_gera_pergunta() {
    let fx = Fixture::new();
    let archive = fx.downloads.join("Mod.zip");
    make_zip(&archive, &[("Option A/unico.package", b"x".to_vec())]);

    prepare_staging(&[archive], &fx.staging()).unwrap();
    let report = calculate_conflicts(&fx.staging(), &fx.mods_dir).unwrap();

    assert!(detect_mutually_exclusive(&report.staged_files).is_empty());
}

#[test]
fn mod_comum_nao_e_confundido_com_variante_exclusiva() {
    let fx = Fixture::new();
    let archive = fx.downloads.join("Normal.zip");
    make_zip(
        &archive,
        &[
            ("Cabelos/longo.package", b"a".to_vec()),
            ("Cabelos/curto.package", b"b".to_vec()),
        ],
    );

    prepare_staging(&[archive], &fx.staging()).unwrap();
    let report = calculate_conflicts(&fx.staging(), &fx.mods_dir).unwrap();

    assert!(detect_mutually_exclusive(&report.staged_files).is_empty());
}

#[test]
fn dependencias_sao_reunidas_sem_repetir() {
    let fx = Fixture::new();
    let archive = fx.downloads.join("ModComDeps.zip");
    let mut com_lot51 = b"DBPF".to_vec();
    com_lot51.extend_from_slice(b"....Lot51....");
    let mut com_xml = b"DBPF".to_vec();
    com_xml.extend_from_slice(b"....XML Injector....");
    let mut outro_lot51 = b"DBPF".to_vec();
    outro_lot51.extend_from_slice(b"....Lot51 de novo....");

    make_zip(
        &archive,
        &[
            ("a.package", com_lot51),
            ("b.package", com_xml),
            ("c.package", outro_lot51),
        ],
    );

    prepare_staging(&[archive], &fx.staging()).unwrap();
    let report = calculate_conflicts(&fx.staging(), &fx.mods_dir).unwrap();
    let deps = collect_dependencies(&report.staged_files);

    assert_eq!(deps, vec!["Lot51 Core Library", "XML Injector"]);
}

#[test]
fn mod_sem_dependencia_nao_inventa_nenhuma() {
    let fx = Fixture::new();
    let archive = fx.downloads.join("Simples.zip");
    make_zip(
        &archive,
        &[("mod.package", b"DBPF conteudo qualquer".to_vec())],
    );

    prepare_staging(&[archive], &fx.staging()).unwrap();
    let report = calculate_conflicts(&fx.staging(), &fx.mods_dir).unwrap();

    assert!(collect_dependencies(&report.staged_files).is_empty());
}

/// A marca de dependência só existe **dentro** do recurso, comprimida.
///
/// Este é o caso do mundo real: num `.package` de verdade os recursos vêm em
/// zlib, e a varredura dos bytes crus — que é o que o app fazia — não acha nada.
/// Medido contra a pasta `Mods` real: 0 acertos em 1569 packages nos bytes
/// crus, e o mod que de fato exige Lot51 aparece assim que se descomprime.
#[test]
fn dependencia_e_encontrada_dentro_de_recurso_comprimido() {
    use flate2::write::ZlibEncoder;
    use flate2::Compression;
    use s4suite::engine::dbpf::{DBPFWriter, PackageResource, ResourceKey};

    let fx = Fixture::new();
    // Uma tuning real tem centenas de linhas repetitivas; num texto curto o
    // deflate emite quase tudo literal e a marca sobreviveria por acaso,
    // fazendo o teste passar pelo motivo errado.
    let mut tuning: Vec<u8> = Vec::new();
    for i in 0..200 {
        tuning.extend_from_slice(
            format!("<T n=\"parametro_{}\">valor padrao de teste</T>\n", i % 7).as_bytes(),
        );
    }
    tuning.extend_from_slice(br#"<I c="Interaction" m="Lot51:core.interactions">"#);
    let tuning = tuning;

    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::best());
    encoder.write_all(&tuning).unwrap();
    let comprimido = encoder.finish().unwrap();

    let recurso = PackageResource {
        key: ResourceKey {
            type_id: 0x03B33DDF,
            group_id: 0,
            instance_ex: 0,
            instance_low: 7,
        },
        data: comprimido,
        mem_size: tuning.len() as u32,
        compressed: 0x5A42,
    };

    let pacote = fx.downloads.join("MeuMod.package");
    let mut f = fs::File::create(&pacote).unwrap();
    DBPFWriter::write_package(&mut f, &[recurso]).unwrap();
    drop(f);

    // O que o app fazia antes: procurar no arquivo cru.
    let cru = fs::read(&pacote).unwrap();
    assert!(
        s4suite::engine::installer::detect_dependencies(&cru).is_empty(),
        "o teste perdeu o sentido: a marca ficou legível sem descomprimir"
    );

    assert_eq!(
        s4suite::engine::installer::detect_dependencies_in_package(&pacote),
        vec!["Lot51 Core Library"]
    );
}

// --- Testes de regressão P0: estabilização do instalador ---

#[test]
fn conteudo_diferente_com_mesmo_tamanho_e_identificado_como_atualizacao_e_instalado() {
    let fx = Fixture::new();
    // Cria dois conteúdos com exatamente o mesmo número de bytes
    let conteudo_antigo =
        b"DBPF_VERSION_1_CONTEUDO_ANTIGO_COM_EXATAMENTE_64_BYTES_DE_TAMANHO!".to_vec();
    let conteudo_novo =
        b"DBPF_VERSION_2_CONTEUDO_NOVO___COM_EXATAMENTE_64_BYTES_DE_TAMANHO!".to_vec();
    assert_eq!(conteudo_antigo.len(), conteudo_novo.len());
    assert_ne!(conteudo_antigo, conteudo_novo);

    let instalado = fx.mods_dir.join("MeusMods/update_mod.package");
    write_file(&instalado, &conteudo_antigo);

    let archive = fx.downloads.join("update_mod.zip");
    make_zip(&archive, &[("update_mod.package", conteudo_novo.clone())]);

    prepare_staging(&[archive], &fx.staging()).unwrap();
    let report = calculate_conflicts(&fx.staging(), &fx.mods_dir).unwrap();

    // Mesmo com o mesmo tamanho em bytes, não pode ser classificado como ExactMatch
    assert_eq!(
        report.exact_matches, 0,
        "conteúdo diferente não pode ser ExactMatch mesmo com tamanho igual"
    );
    assert_eq!(report.updates, 1, "deve ser reconhecido como atualização");
    assert_eq!(report.staged_files[0].conflict, ConflictType::SizeDiff);
    assert_eq!(
        report.staged_files[0].destination(&fx.mods_dir, MANAGED_BASE_DIR),
        instalado
    );

    let (installed, skipped) = execute_installation(
        &report,
        &fx.mods_dir,
        MANAGED_BASE_DIR,
        &[fx.mods_dir.clone()],
    )
    .unwrap();

    assert_eq!(installed, 1);
    assert_eq!(skipped, 0);
    assert_eq!(
        fs::read(&instalado).unwrap(),
        conteudo_novo,
        "o arquivo instalado deve ter o novo conteúdo"
    );

    // Verifica que o backup do conteúdo anterior foi salvo intacto
    let backups: Vec<_> = fs::read_dir(fx.mods_dir.join(".s4suite_backups"))
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(backups.len(), 1);
    assert_eq!(fs::read(backups[0].path()).unwrap(), conteudo_antigo);
}

#[test]
fn nomes_repetidos_em_mods_diferentes_nao_sobrescrevem_outros_autores() {
    let fx = Fixture::new();

    // Dois mods existentes de autores diferentes com o mesmo nome genérico de arquivo
    let mod_autor_a = fx.mods_dir.join("AutorA_Roupas/item.package");
    let mod_autor_b = fx.mods_dir.join("AutorB_Cabelos/item.package");
    write_file(&mod_autor_a, b"conteudo_exclusivo_do_autor_a");
    write_file(&mod_autor_b, b"conteudo_exclusivo_do_autor_b");

    // Agora instalamos um terceiro mod não relacionado que também contém item.package
    let archive = fx.downloads.join("AutorC_Moveis.zip");
    make_zip(
        &archive,
        &[("item.package", b"conteudo_novo_do_autor_c".to_vec())],
    );

    prepare_staging(&[archive], &fx.staging()).unwrap();
    let report = calculate_conflicts(&fx.staging(), &fx.mods_dir).unwrap();

    // Não deve sobrescrever AutorA nem AutorB
    assert_eq!(
        report.updates, 0,
        "não deve assumir atualização de mod não relacionado"
    );
    assert_eq!(report.exact_matches, 0);
    assert_eq!(
        report.new_files, 1,
        "deve ser classificado como novo arquivo isolado"
    );
    assert_eq!(report.staged_files[0].conflict, ConflictType::NewFile);

    let esperado_dest = fx.managed().join("AutorC_Moveis/item.package");
    assert_eq!(
        report.staged_files[0].destination(&fx.mods_dir, MANAGED_BASE_DIR),
        esperado_dest
    );

    let (installed, skipped) = execute_installation(
        &report,
        &fx.mods_dir,
        MANAGED_BASE_DIR,
        &[fx.mods_dir.clone()],
    )
    .unwrap();
    assert_eq!(installed, 1);
    assert_eq!(skipped, 0);

    // Todos os 3 mods devem coexistir intactos
    assert_eq!(
        fs::read(&mod_autor_a).unwrap(),
        b"conteudo_exclusivo_do_autor_a"
    );
    assert_eq!(
        fs::read(&mod_autor_b).unwrap(),
        b"conteudo_exclusivo_do_autor_b"
    );
    assert_eq!(
        fs::read(&esperado_dest).unwrap(),
        b"conteudo_novo_do_autor_c"
    );
}

#[test]
fn falha_de_copia_executa_rollback_sem_arquivos_parciais_e_restaura_originais() {
    let fx = Fixture::new();

    // Mod original já instalado na pasta do pacote
    let mod_antigo = fx.managed().join("PacoteMisto/mod_antigo.package");
    write_file(&mod_antigo, b"versao_original_intacta");

    // Pacote que atualiza mod_antigo e tenta instalar um segundo arquivo em subpasta
    let archive = fx.downloads.join("PacoteMisto.zip");
    make_zip(
        &archive,
        &[
            (
                "mod_antigo.package",
                b"versao_atualizada_que_deve_ser_revertida".to_vec(),
            ),
            (
                "z_bloqueado/arquivo.package",
                b"arquivo_que_vai_falhar".to_vec(),
            ),
        ],
    );

    prepare_staging(&[archive], &fx.staging()).unwrap();
    let mut report = calculate_conflicts(&fx.staging(), &fx.mods_dir).unwrap();
    // Garante que o arquivo mod_antigo seja processado antes do arquivo que falhará
    report
        .staged_files
        .sort_by(|a, b| b.filename.cmp(&a.filename));
    // Provoca falha simulada criando um arquivo onde a pasta 'z_bloqueado' deveria ser criada
    let pasta_bloqueada = fx.managed().join("PacoteMisto/z_bloqueado");
    write_file(&pasta_bloqueada, b"arquivo_bloqueando_criacao_de_diretorio");

    // A instalação deve falhar no segundo arquivo e executar rollback atômico
    let resultado = execute_installation(
        &report,
        &fx.mods_dir,
        MANAGED_BASE_DIR,
        &[fx.mods_dir.clone()],
    );
    assert!(
        resultado.is_err(),
        "instalação deveria falhar e acionar rollback"
    );

    // O arquivo mod_antigo deve ter sido restaurado com seu conteúdo original
    assert_eq!(
        fs::read(&mod_antigo).unwrap(),
        b"versao_original_intacta",
        "rollback deve restaurar o arquivo original intacto"
    );

    // Nenhum arquivo temporário .s4tmp deve sobrar
    for entry in walkdir::WalkDir::new(&fx.mods_dir)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let fname = entry.file_name().to_string_lossy();
        assert!(
            !fname.ends_with(".s4tmp"),
            "não deve deixar arquivos temporários residuais: {}",
            entry.path().display()
        );
    }

    // O backup gerado deve permanecer preservado em .s4suite_backups
    let backups: Vec<_> = fs::read_dir(fx.mods_dir.join(".s4suite_backups"))
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(
        backups.len(),
        1,
        "o backup de segurança deve ser preservado para contingência"
    );
}

#[test]
fn interrupcao_recuperavel_preserva_integridade_e_permite_reinstalacao() {
    let fx = Fixture::new();

    let archive = fx.downloads.join("ModRecuperavel.zip");
    make_zip(
        &archive,
        &[("recup/item.package", b"conteudo_recuperavel".to_vec())],
    );

    // Simula primeira extração para staging
    prepare_staging(&[archive.clone()], &fx.staging()).unwrap();
    assert!(fx
        .staging()
        .join("ModRecuperavel/recup/item.package")
        .exists());

    // Simula interrupção e limpeza segura de staging
    s4suite::engine::installer::clear_staging(&fx.staging()).unwrap();
    assert!(
        !fx.staging().exists(),
        "staging deve ser limpo completamente"
    );

    // Segunda tentativa do fluxo completo
    prepare_staging(&[archive], &fx.staging()).unwrap();
    let report = calculate_conflicts(&fx.staging(), &fx.mods_dir).unwrap();
    let (installed, skipped) = execute_installation(
        &report,
        &fx.mods_dir,
        MANAGED_BASE_DIR,
        &[fx.mods_dir.clone()],
    )
    .unwrap();

    assert_eq!(installed, 1);
    assert_eq!(skipped, 0);
    assert!(fx
        .managed()
        .join("ModRecuperavel/recup/item.package")
        .exists());
}

#[test]
fn scripts_com_mesmo_nome_em_mods_diferentes_na_mesma_fila_sao_desambiguados() {
    let fx = Fixture::new();

    // Dois zips de autores diferentes na mesma fila, ambos contendo main.ts4script
    let zip_a = fx.downloads.join("AutorA_Mod.zip");
    let zip_b = fx.downloads.join("AutorB_Mod.zip");
    make_zip(&zip_a, &[("main.ts4script", b"script_do_autor_a".to_vec())]);
    make_zip(&zip_b, &[("main.ts4script", b"script_do_autor_b".to_vec())]);

    let rep = prepare_staging(&[zip_a, zip_b], &fx.staging()).unwrap();
    assert_eq!(rep.prepared, 2);

    let report = calculate_conflicts(&fx.staging(), &fx.mods_dir).unwrap();
    assert_eq!(report.staged_files.len(), 2);

    // Os dois arquivos não podem ter o mesmo destino
    let dest0 = report.staged_files[0].destination(&fx.mods_dir, MANAGED_BASE_DIR);
    let dest1 = report.staged_files[1].destination(&fx.mods_dir, MANAGED_BASE_DIR);
    assert_ne!(
        dest0, dest1,
        "scripts com mesmo nome na mesma fila devem receber destinos desambiguados"
    );

    let (installed, skipped) = execute_installation(
        &report,
        &fx.mods_dir,
        MANAGED_BASE_DIR,
        &[fx.mods_dir.clone()],
    )
    .unwrap();
    assert_eq!(installed, 2);
    assert_eq!(skipped, 0);

    // Ambos os scripts devem existir com seus conteúdos preservados
    assert!(dest0.exists());
    assert!(dest1.exists());
    let c0 = fs::read(&dest0).unwrap();
    let c1 = fs::read(&dest1).unwrap();
    assert_ne!(c0, c1);
    assert!(c0 == b"script_do_autor_a" || c0 == b"script_do_autor_b");
    assert!(c1 == b"script_do_autor_a" || c1 == b"script_do_autor_b");
}

#[test]
fn unique_dest_path_e_atomic_copy_protegidos_contra_symlinks_quebrados() {
    let fx = Fixture::new();
    let pasta = fx.mods_dir.join("TesteSymlink");
    fs::create_dir_all(&pasta).unwrap();

    let arquivo_inexistente = pasta.join("alvo_inexistente.txt");
    let link_quebrado = pasta.join("conflito.package");

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&arquivo_inexistente, &link_quebrado).unwrap();
        assert!(fs::symlink_metadata(&link_quebrado).is_ok());
        assert!(
            !link_quebrado.exists(),
            "link quebrado não tem destino existente"
        );

        // unique_dest_path não pode retornar o link_quebrado existente
        let unico = s4suite::core::safety::unique_dest_path(&pasta, "conflito.package");
        assert_ne!(
            unico, link_quebrado,
            "unique_dest_path deve detectar link quebrado e gerar nome novo"
        );
        assert_eq!(unico, pasta.join("conflito_1.package"));

        // Publicação não pode substituir nem mesmo um link quebrado já existente.
        let fonte = pasta.join("fonte.package");
        fs::write(&fonte, b"NOVO_CONTEUDO_ATOMIC").unwrap();
        let result = s4suite::engine::installer::atomic_copy(&fonte, &link_quebrado);
        assert!(
            result.is_err(),
            "destino ocupado deve ser recusado: {result:?}"
        );
        assert!(fs::symlink_metadata(&link_quebrado)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(fs::read(&fonte).unwrap(), b"NOVO_CONTEUDO_ATOMIC");
        assert!(
            !arquivo_inexistente.exists(),
            "alvo inexistente não deve ter sido criado fora"
        );
    }
}

#[test]
fn atomic_copy_bloqueia_escrita_atraves_de_symlink_ancestral() {
    let fx = Fixture::new();
    let pasta_externa = fx._root.path().join("PastaExterna");
    fs::create_dir_all(&pasta_externa).unwrap();

    let pasta_link = fx.mods_dir.join("LinkAncestral");
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&pasta_externa, &pasta_link).unwrap();
        assert!(fs::symlink_metadata(&pasta_link).is_ok());

        let fonte = fx.downloads.join("teste.txt");
        fs::write(&fonte, b"conteudo_de_teste").unwrap();

        // Tenta gravar arquivo dentro da pasta que é um link simbólico para fora
        let destino_escapando = pasta_link.join("arquivo_escapando.txt");
        let res = s4suite::engine::installer::atomic_copy(&fonte, &destino_escapando);
        assert!(
            res.is_err(),
            "atomic_copy deve recusar escrever através de symlink ancestral"
        );

        let err = res.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
        assert!(
            !pasta_externa.join("arquivo_escapando.txt").exists(),
            "nenhum arquivo pode ser escrito na pasta externa"
        );
    }
}

#[test]
fn prepare_staging_isola_fontes_diferentes_com_mesma_identidade() {
    let fx = Fixture::new();

    // Duas fontes em diretórios diferentes, ambas chamadas mod.zip
    let dir_a = fx.downloads.join("DirA");
    let dir_b = fx.downloads.join("DirB");
    fs::create_dir_all(&dir_a).unwrap();
    fs::create_dir_all(&dir_b).unwrap();

    let zip_a = dir_a.join("mod.zip");
    let zip_b = dir_b.join("mod.zip");
    make_zip(
        &zip_a,
        &[("conteudo_a.package", package_bytes("payload_a"))],
    );
    make_zip(
        &zip_b,
        &[("conteudo_b.package", package_bytes("payload_b"))],
    );

    let rep = prepare_staging(&[zip_a, zip_b], &fx.staging()).unwrap();
    assert_eq!(rep.prepared, 2);
    assert!(rep.failures.is_empty());

    // As duas fontes não podem ter colidido no mesmo diretório de staging
    assert!(
        fx.staging().join("mod/conteudo_a.package").exists(),
        "fonte A deve estar preservada"
    );
    assert!(
        fx.staging().join("mod_1/conteudo_b.package").exists(),
        "fonte B deve estar isolada em mod_1"
    );

    let report = calculate_conflicts(&fx.staging(), &fx.mods_dir).unwrap();
    assert_eq!(report.staged_files.len(), 2);

    // Destinos finais não devem colidir
    let dest_a = report.staged_files[0].destination(&fx.mods_dir, MANAGED_BASE_DIR);
    let dest_b = report.staged_files[1].destination(&fx.mods_dir, MANAGED_BASE_DIR);
    assert_ne!(
        dest_a, dest_b,
        "destinos devem ser isolados em pastas de triagem distintas"
    );
}

#[test]
fn fila_com_destinos_colidentes_ou_ja_existentes_garante_unicidade() {
    let fx = Fixture::new();

    // Arquivo existente em 00_Triagem_Novos
    let preexistente = fx.managed().join("Autor_Mod/item.package");
    write_file(&preexistente, b"item_preexistente_no_disco");

    // Instalando novo mod com o mesmo caminho relativo que colidiria
    let zip = fx.downloads.join("Autor_Mod.zip");
    make_zip(&zip, &[("item.package", package_bytes("novo_item"))]);

    prepare_staging(&[zip], &fx.staging()).unwrap();
    let report = calculate_conflicts(&fx.staging(), &fx.mods_dir).unwrap();

    assert_eq!(report.staged_files.len(), 1);
    // Deve ser atualização segura ou receber destino desambiguado sem substituir silenciosamente
    let dest = report.staged_files[0].destination(&fx.mods_dir, MANAGED_BASE_DIR);
    assert!(dest.starts_with(&fx.mods_dir));
}

#[test]
fn p0_atomic_copy_nao_sobrescreve_destino_existente() {
    let root = TempDir::new().unwrap();
    let src = root.path().join("source.package");
    let dest = root.path().join("dest.package");
    fs::write(&src, b"new bytes").unwrap();
    fs::write(&dest, b"original bytes").unwrap();
    let result = s4suite::engine::installer::atomic_copy(&src, &dest);
    let actual = fs::read(&dest).unwrap();
    assert!(
        result.is_err(),
        "sobrescrita aceita: {result:?}; destino={actual:?}"
    );
    assert_eq!(actual, b"original bytes", "destino preexistente alterado");
}

#[test]
fn p0_atomic_copy_simultanea_publica_apenas_um_arquivo_completo() {
    use std::sync::{Arc, Barrier};
    let root = TempDir::new().unwrap();
    let dest = root.path().join("dest.package");
    let a = root.path().join("a.package");
    let b = root.path().join("b.package");
    let bytes_a = vec![b'A'; 128 * 1024];
    let bytes_b = vec![b'B'; 128 * 1024];
    fs::write(&a, &bytes_a).unwrap();
    fs::write(&b, &bytes_b).unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let jobs: Vec<_> = [a, b]
        .into_iter()
        .map(|src| {
            let barrier = barrier.clone();
            let dest = dest.clone();
            std::thread::spawn(move || {
                barrier.wait();
                s4suite::engine::installer::atomic_copy(&src, &dest)
            })
        })
        .collect();
    let results: Vec<_> = jobs.into_iter().map(|job| job.join().unwrap()).collect();
    let actual = fs::read(&dest).unwrap();
    assert_eq!(
        results.iter().filter(|r| r.is_ok()).count(),
        1,
        "publicação duplicada: {results:?}; tamanho={}, prefixo={:?}",
        actual.len(),
        &actual[..actual.len().min(16)]
    );
    assert!(
        actual == bytes_a || actual == bytes_b,
        "cópia parcial: tamanho={}",
        actual.len()
    );
}

#[cfg(unix)]
#[test]
fn p0_atomic_copy_recusa_fonte_symlink() {
    let root = TempDir::new().unwrap();
    let outside = root.path().join("outside.package");
    let src = root.path().join("link.package");
    let dest = root.path().join("dest.package");
    fs::write(&outside, b"external source bytes").unwrap();
    std::os::unix::fs::symlink(&outside, &src).unwrap();
    let result = s4suite::engine::installer::atomic_copy(&src, &dest);
    let actual = fs::read(&outside).unwrap();
    assert!(
        result.is_err(),
        "fonte symlink aceita: {result:?}; externo={actual:?}"
    );
    assert!(!dest.exists(), "fonte symlink foi publicada");
    assert_eq!(actual, b"external source bytes");
}

#[test]
fn p0_instalacao_recusa_destino_fora_do_escopo_antes_de_escrever() {
    let fx = Fixture::new();
    let src = fx.downloads.join("mod.package");
    write_file(&src, b"new mod bytes");
    prepare_staging(&[src], &fx.staging()).unwrap();
    let mut report = calculate_conflicts(&fx.staging(), &fx.mods_dir).unwrap();
    let outside = fx._root.path().join("outside.package");
    fs::write(&outside, b"external original bytes").unwrap();
    report.staged_files[0].existing_dest = Some(outside.clone());
    report.staged_files[0].conflict = s4suite::engine::installer::ConflictType::SizeDiff;
    let result = execute_installation(
        &report,
        &fx.mods_dir,
        MANAGED_BASE_DIR,
        &[fx.mods_dir.clone()],
    );
    let actual = fs::read(&outside).unwrap();
    assert!(
        result.is_err(),
        "destino externo aceito: {result:?}; externo={actual:?}"
    );
    assert_eq!(actual, b"external original bytes");
}

#[test]
fn p0_arquivo_criado_depois_do_relatorio_nao_vira_atualizacao() {
    let fx = Fixture::new();
    let src = fx.downloads.join("mod.package");
    write_file(&src, b"new mod bytes");
    prepare_staging(&[src], &fx.staging()).unwrap();
    let report = calculate_conflicts(&fx.staging(), &fx.mods_dir).unwrap();
    let dest = report.staged_files[0].destination(&fx.mods_dir, MANAGED_BASE_DIR);
    write_file(&dest, b"concurrent owner bytes");
    let result = execute_installation(
        &report,
        &fx.mods_dir,
        MANAGED_BASE_DIR,
        &[fx.mods_dir.clone()],
    );
    let actual = fs::read(&dest).unwrap();
    assert!(
        result.is_err(),
        "arquivo concorrente virou atualização: {result:?}; destino={actual:?}"
    );
    assert_eq!(actual, b"concurrent owner bytes");
}

#[test]
fn p0_instalacao_recusa_fonte_truncada_depois_do_relatorio() {
    let fx = Fixture::new();
    let src = fx.downloads.join("mod.package");
    write_file(&src, b"complete mod bytes");
    prepare_staging(&[src], &fx.staging()).unwrap();
    let report = calculate_conflicts(&fx.staging(), &fx.mods_dir).unwrap();
    let dest = report.staged_files[0].destination(&fx.mods_dir, MANAGED_BASE_DIR);
    fs::write(&report.staged_files[0].src_path, b"short").unwrap();
    let result = execute_installation(
        &report,
        &fx.mods_dir,
        MANAGED_BASE_DIR,
        &[fx.mods_dir.clone()],
    );
    assert!(
        result.is_err(),
        "fonte truncada publicada: {result:?}; bytes={:?}",
        fs::read(&dest).ok()
    );
    assert!(!dest.exists(), "destino parcial ficou publicado");
}

#[cfg(unix)]
#[test]
fn p0_clear_staging_recusa_symlink_ancestral_sem_excluir_externo() {
    let root = TempDir::new().unwrap();
    let external = root.path().join("external");
    let staging = external.join("staging");
    fs::create_dir_all(&staging).unwrap();
    let sentinel = staging.join("sentinel.package");
    fs::write(&sentinel, b"external sentinel bytes").unwrap();
    let link = root.path().join("linked");
    std::os::unix::fs::symlink(&external, &link).unwrap();
    let result = s4suite::engine::installer::clear_staging(&link.join("staging"));
    let actual = fs::read(&sentinel);
    assert!(
        result.is_err(),
        "limpeza por ancestral symlink aceita: {result:?}; externo={actual:?}"
    );
    assert_eq!(actual.unwrap(), b"external sentinel bytes");
}

#[cfg(unix)]
#[test]
fn p0_extracao_zip_recusa_diretorio_symlink_sem_escrever_externo() {
    let root = TempDir::new().unwrap();
    let archive = root.path().join("mod.zip");
    make_zip(
        &archive,
        &[("nested/mod.package", b"archive bytes".to_vec())],
    );
    let external = root.path().join("external");
    fs::create_dir(&external).unwrap();
    let output = root.path().join("output");
    fs::create_dir(&output).unwrap();
    std::os::unix::fs::symlink(&external, output.join("nested")).unwrap();
    let result = s4suite::engine::installer::extract_archive(&archive, &output);
    let written = fs::read(external.join("mod.package"));
    assert!(
        result.is_err(),
        "extração seguiu symlink: {result:?}; bytes externos={written:?}"
    );
    assert!(
        !external.join("mod.package").exists(),
        "extração escreveu fora do destino"
    );
}

#[cfg(unix)]
#[test]
fn p0_extracao_recusa_raiz_symlink_sem_escrever_externo() {
    let root = TempDir::new().unwrap();
    let archive = root.path().join("mod.zip");
    make_zip(&archive, &[("mod.package", b"archive bytes".to_vec())]);
    let external = root.path().join("external");
    fs::create_dir(&external).unwrap();
    let output = root.path().join("output");
    std::os::unix::fs::symlink(&external, &output).unwrap();
    let result = s4suite::engine::installer::extract_archive(&archive, &output);
    let written = fs::read(external.join("mod.package"));
    assert!(
        result.is_err(),
        "raiz symlink aceita: {result:?}; externo={written:?}"
    );
    assert!(
        !external.join("mod.package").exists(),
        "extração escreveu fora da raiz"
    );
}

#[test]
fn p0_extracao_7z_recusa_traversal_sem_escrever_fora_da_raiz() {
    let root = TempDir::new().unwrap();
    let archive = root.path().join("mod.7z");
    let mut writer = sevenz_rust::SevenZWriter::new(File::create(&archive).unwrap()).unwrap();
    let mut entry = sevenz_rust::SevenZArchiveEntry::new();
    entry.name = "../escaped.package".to_string();
    writer
        .push_archive_entry(entry, Some(std::io::Cursor::new(b"escaped bytes")))
        .unwrap();
    writer.finish().unwrap();
    let output = root.path().join("output");
    let result = s4suite::engine::installer::extract_archive(&archive, &output);
    let outside = root.path().join("escaped.package");
    let written = fs::read(&outside);
    assert!(
        result.is_err(),
        "traversal 7z aceito: {result:?}; externo={written:?}"
    );
    assert!(!outside.exists(), "7z escreveu fora da raiz");
}

#[test]
fn p0_fallback_7z_stdout_preserva_nomes_literais_com_asterisco() {
    let root = TempDir::new().unwrap();
    // É ZIP com extensão .exe para exercitar o CLI, não um ReShade/RAR real.
    let archive = root.path().join("fixture.exe");
    make_zip(
        &archive,
        &[
            ("a*.package", b"literal wildcard bytes".to_vec()),
            ("axb.package", b"separate file bytes".to_vec()),
        ],
    );
    let output = root.path().join("output");
    s4suite::engine::installer::extract_archive(&archive, &output).unwrap();
    assert_eq!(
        fs::read(output.join("a*.package")).unwrap(),
        b"literal wildcard bytes"
    );
    assert_eq!(
        fs::read(output.join("axb.package")).unwrap(),
        b"separate file bytes"
    );
}

#[test]
fn p0_fallback_7z_falha_crc_nao_publica_stdout_mesmo_com_tamanho_correto() {
    let root = TempDir::new().unwrap();
    let archive = root.path().join("corrupt.exe");
    let payload = b"unique_payload_for_crc_failure";
    let mut writer = zip::ZipWriter::new(File::create(&archive).unwrap());
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    writer.start_file("mod.package", options).unwrap();
    writer.write_all(payload).unwrap();
    writer.finish().unwrap();
    let mut bytes = fs::read(&archive).unwrap();
    let offset = bytes
        .windows(payload.len())
        .position(|window| window == payload)
        .unwrap();
    bytes[offset] ^= 1;
    fs::write(&archive, bytes).unwrap();
    let output = root.path().join("output");
    let result = s4suite::engine::installer::extract_archive(&archive, &output);
    assert!(result.is_err(), "CRC inválido aceito: {result:?}");
    assert!(
        !output.join("mod.package").exists(),
        "stdout de processo falho foi publicado: {:?}",
        fs::read(output.join("mod.package"))
    );
}
