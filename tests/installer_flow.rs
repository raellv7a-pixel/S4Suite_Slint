//! Testes de integração do fluxo do instalador: seleção -> staging ->
//! conflitos -> instalação em Mods.

use s4suite::engine::installer::{
    calculate_conflicts, collect_dependencies, detect_mutually_exclusive, execute_installation,
    install_identity_name, is_valid_mod_file,
    prepare_staging, ConflictType, MANAGED_BASE_DIR, STAGING_DIR_NAME,
};
use s4suite::bridge::state::PendingInstall;
use std::fs;
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
        Self { _root: root, mods_dir, downloads }
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
    assert_eq!(install_identity_name(Path::new("Meu Mod v2.1.zip")), "Meu Mod v2.1");
    assert_eq!(install_identity_name(Path::new("/tmp/UI-Cheats.package")), "UI-Cheats");
    assert_eq!(
        install_identity_name(Path::new("Mod X — Tradução PT-BR.package")),
        "Mod X — Tradução PT-BR"
    );
    assert_eq!(install_identity_name(Path::new("mod:teste*final.zip")), "mod_teste_final");
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

    assert!(err.to_string().contains("instalar.exe"), "erro inesperado: {}", err);
    assert!(!fx.staging().join("suspeito").exists(), "staging não foi limpo após bloqueio");
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
    make_zip(&archive, &[("alpha.package", package_bytes("versao nova, bem maior"))]);

    prepare_staging(&[archive], &fx.staging()).unwrap();
    let report = calculate_conflicts(&fx.staging(), &fx.mods_dir).unwrap();

    assert_eq!(report.updates, 1);
    assert_eq!(report.new_files, 0);
    // Atualização sobrescreve onde o mod já estava, não cria uma segunda cópia.
    assert_eq!(report.staged_files[0].destination(&fx.mods_dir, MANAGED_BASE_DIR), instalado);
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
    let (installed, skipped) =
        execute_installation(&report, &fx.mods_dir, MANAGED_BASE_DIR, &[fx.mods_dir.clone()]).unwrap();

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
    assert!(script.exists(), "script deveria estar raso na base gerenciada");
    let profundidade = script.strip_prefix(&fx.mods_dir).unwrap().components().count();
    assert_eq!(profundidade, 2, "script fundo demais: o jogo não o carregaria");
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
    let (installed, skipped) =
        execute_installation(&report, &fx.mods_dir, MANAGED_BASE_DIR, &[fx.mods_dir.clone()]).unwrap();

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
    make_zip(&archive, &[("beta.package", package_bytes("conteudo novo e diferente"))]);

    prepare_staging(&[archive], &fx.staging()).unwrap();
    let report = calculate_conflicts(&fx.staging(), &fx.mods_dir).unwrap();
    execute_installation(&report, &fx.mods_dir, MANAGED_BASE_DIR, &[fx.mods_dir.clone()]).unwrap();

    let backups: Vec<_> = fs::read_dir(fx.mods_dir.join(".s4suite_backups"))
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(backups.len(), 1);
    assert_eq!(fs::read(backups[0].path()).unwrap(), antigo);
    assert_ne!(fs::read(&instalado).unwrap(), antigo, "o mod deveria ter sido atualizado");
}

#[test]
fn staging_e_recriado_do_zero_a_cada_instalacao() {
    let fx = Fixture::new();
    write_file(&fx.staging().join("lixo_da_rodada_anterior.package"), &package_bytes("velho"));

    let archive = fx.downloads.join("novo.zip");
    make_zip(&archive, &[("novo.package", package_bytes("novo"))]);
    prepare_staging(&[archive], &fx.staging()).unwrap();

    assert!(!fx.staging().join("lixo_da_rodada_anterior.package").exists());
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

    assert_eq!(opcoes.len(), 1, "as variantes pertencem a um único grupo");
    assert_eq!(
        opcoes[0].files,
        vec![
            PathBuf::from("CabeloPack/Options/Cabelo Loiro.package"),
            PathBuf::from("CabeloPack/Options/Cabelo Ruivo.package"),
        ]
    );
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
fn grupos_de_zips_e_pastas_distintos_nao_se_misturam() {
    let fx = Fixture::new();
    let first = fx.downloads.join("A.zip");
    let second = fx.downloads.join("B.zip");
    make_zip(
        &first,
        &[
            ("Hair/Options/red.package", b"red".to_vec()),
            ("Hair/Options/blue.package", b"blue".to_vec()),
            ("Dress/ONLY_ONE/short.package", b"short".to_vec()),
            ("Dress/ONLY_ONE/long.package", b"long".to_vec()),
            ("common.package", b"common".to_vec()),
        ],
    );
    make_zip(
        &second,
        &[
            ("Hair/Options/black.package", b"black".to_vec()),
            ("Hair/Options/white.package", b"white".to_vec()),
        ],
    );
    prepare_staging(&[first, second], &fx.staging()).unwrap();
    let mut report = calculate_conflicts(&fx.staging(), &fx.mods_dir).unwrap();
    let groups = detect_mutually_exclusive(&report.staged_files);
    assert_eq!(groups.len(), 3);
    let hair = groups
        .iter()
        .find(|group| group.folder == Path::new("A/Hair/Options"))
        .unwrap();
    report
        .select_variants(hair, &[PathBuf::from("A/Hair/Options/red.package")])
        .unwrap();
    assert_eq!(report.new_files, 6);
    execute_installation(
        &report,
        &fx.mods_dir,
        MANAGED_BASE_DIR,
        &[fx.mods_dir.clone()],
    )
    .unwrap();
    assert!(!fx.managed().join("A/Hair/Options/blue.package").exists());
    for (path, bytes) in [
        ("A/Hair/Options/red.package", b"red".as_slice()),
        ("A/Dress/ONLY_ONE/short.package", b"short".as_slice()),
        ("A/Dress/ONLY_ONE/long.package", b"long".as_slice()),
        ("A/common.package", b"common".as_slice()),
        ("B/Hair/Options/black.package", b"black".as_slice()),
        ("B/Hair/Options/white.package", b"white".as_slice()),
    ] {
        assert_eq!(fs::read(fx.managed().join(path)).unwrap(), bytes);
    }
}

#[test]
fn selecao_manual_recalcula_conflitos_e_recusa_itens_de_outro_grupo() {
    let fx = Fixture::new();
    let archive = fx.downloads.join("Variants.zip");
    make_zip(
        &archive,
        &[
            ("Choose One/new.package", b"new".to_vec()),
            ("Choose One/update.package", b"updated".to_vec()),
            ("Choose One/same.package", b"same".to_vec()),
            ("Choose One/unselected.package", b"skip".to_vec()),
            ("common.package", b"common".to_vec()),
        ],
    );
    write_file(&fx.mods_dir.join("old/update.package"), b"old");
    write_file(&fx.mods_dir.join("old/same.package"), b"same");
    prepare_staging(&[archive], &fx.staging()).unwrap();
    let mut report = calculate_conflicts(&fx.staging(), &fx.mods_dir).unwrap();
    let group = detect_mutually_exclusive(&report.staged_files).remove(0);
    assert!(report.select_variants(&group, &[]).is_err());
    assert!(report
        .select_variants(&group, &[PathBuf::from("Variants/common.package")])
        .is_err());
    let selected = [
        PathBuf::from("Variants/Choose One/new.package"),
        PathBuf::from("Variants/Choose One/update.package"),
        PathBuf::from("Variants/Choose One/same.package"),
    ];
    report.select_variants(&group, &selected).unwrap();
    assert_eq!(
        (report.new_files, report.updates, report.exact_matches),
        (2, 1, 1)
    );
    let outcome = execute_installation(
        &report,
        &fx.mods_dir,
        MANAGED_BASE_DIR,
        &[fx.mods_dir.clone()],
    )
    .unwrap();
    assert_eq!(outcome, (3, 1));
    assert_eq!(
        fs::read(fx.mods_dir.join("old/update.package")).unwrap(),
        b"updated"
    );
    assert_eq!(
        fs::read(fx.mods_dir.join(".s4suite_backups/update.package")).unwrap(),
        b"old"
    );
    assert!(!fx
        .managed()
        .join("Variants/Choose One/unselected.package")
        .exists());
    assert_eq!(
        fs::read(fx.managed().join("Variants/common.package")).unwrap(),
        b"common"
    );
}

#[test]
fn instalar_todas_as_possiveis_variantes_preserva_o_pacote() {
    let fx = Fixture::new();
    let archive = fx.downloads.join("Clothes.zip");
    make_zip(
        &archive,
        &[
            ("Options/dress.package", b"dress".to_vec()),
            ("Options/skirt.package", b"skirt".to_vec()),
        ],
    );
    prepare_staging(&[archive], &fx.staging()).unwrap();
    let mut report = calculate_conflicts(&fx.staging(), &fx.mods_dir).unwrap();
    let group = detect_mutually_exclusive(&report.staged_files).remove(0);
    report.select_variants(&group, &group.files).unwrap();
    assert_eq!(report.new_files, 2);
    execute_installation(
        &report,
        &fx.mods_dir,
        MANAGED_BASE_DIR,
        &[fx.mods_dir.clone()],
    )
    .unwrap();
    assert_eq!(
        fs::read(fx.managed().join("Clothes/Options/dress.package")).unwrap(),
        b"dress"
    );
    assert_eq!(
        fs::read(fx.managed().join("Clothes/Options/skirt.package")).unwrap(),
        b"skirt"
    );
}

#[test]
fn opcoes_isoladas_em_fontes_independentes_nao_geram_grupo_global() {
    let fx = Fixture::new();
    let first = fx.downloads.join("A.zip");
    let second = fx.downloads.join("B.zip");
    make_zip(&first, &[("Options/top.package", b"top".to_vec())]);
    make_zip(&second, &[("Options/skirt.package", b"skirt".to_vec())]);
    prepare_staging(&[first, second], &fx.staging()).unwrap();
    let report = calculate_conflicts(&fx.staging(), &fx.mods_dir).unwrap();
    assert!(detect_mutually_exclusive(&report.staged_files).is_empty());
}

#[test]
fn fontes_homonimas_mantem_staging_e_decisoes_independentes() {
    let fx = Fixture::new();
    let first = fx.downloads.join("A/Mod.zip");
    let second = fx.downloads.join("B/Mod.zip");
    fs::create_dir_all(first.parent().unwrap()).unwrap();
    fs::create_dir_all(second.parent().unwrap()).unwrap();
    make_zip(
        &first,
        &[
            ("Options/first.package", b"first".to_vec()),
            ("Options/second.package", b"second".to_vec()),
        ],
    );
    make_zip(
        &second,
        &[
            ("Options/third.package", b"third".to_vec()),
            ("Options/fourth.package", b"fourth".to_vec()),
        ],
    );
    prepare_staging(&[first, second], &fx.staging()).unwrap();
    let mut report = calculate_conflicts(&fx.staging(), &fx.mods_dir).unwrap();
    let groups = detect_mutually_exclusive(&report.staged_files);
    assert_eq!(groups.len(), 2);
    assert_ne!(groups[0].source, groups[1].source);
    report
        .select_variants(&groups[0], &groups[0].files[..1])
        .unwrap();
    assert!(groups[1].files.iter().all(|path| report
        .staged_files
        .iter()
        .any(|file| file.rel_path == *path)));
    assert_eq!(report.new_files, 3);
}

#[test]
fn decisoes_todos_unico_e_manual_isolam_os_grupos_e_preservam_arquivos_comuns() {
    let fx = Fixture::new();
    let archive = fx.downloads.join("Mixed.zip");
    make_zip(
        &archive,
        &[
            ("A/Options/a.package", b"a".to_vec()),
            ("A/Options/b.package", b"b".to_vec()),
            ("B/Options/c.package", b"c".to_vec()),
            ("B/Options/d.package", b"d".to_vec()),
            ("C/Options/e.package", b"e".to_vec()),
            ("C/Options/f.package", b"f".to_vec()),
            ("C/Options/g.package", b"g".to_vec()),
            ("common.package", b"common".to_vec()),
        ],
    );
    prepare_staging(&[archive], &fx.staging()).unwrap();
    let report = calculate_conflicts(&fx.staging(), &fx.mods_dir).unwrap();
    let mut pending = PendingInstall::new(report, String::new());
    pending.confirm_group().unwrap(); // Instalar todos no primeiro grupo.
    pending.set_mode("one");
    pending.toggle_file(Path::new("Mixed/B/Options/d.package"));
    pending.toggle_file(Path::new("Mixed/C/Options/g.package")); // Outro grupo não é selecionável.
    assert_eq!(
        pending.selected,
        vec![PathBuf::from("Mixed/B/Options/d.package")]
    );
    pending.confirm_group().unwrap();
    assert_eq!(pending.mode, "all");
    pending.set_mode("manual");
    let files = pending.groups.front().unwrap().files.clone();
    for file in &files {
        pending.toggle_file(file);
    }
    assert!(pending.confirm_group().is_err()); // Vazio não avança nem descarta.
    assert_eq!(pending.groups.len(), 1);
    pending.toggle_file(Path::new("Mixed/C/Options/e.package"));
    pending.toggle_file(Path::new("Mixed/C/Options/g.package"));
    pending.confirm_group().unwrap();
    assert!(pending.groups.is_empty());
    assert_eq!(pending.report.new_files, 6);
    execute_installation(
        &pending.report,
        &fx.mods_dir,
        MANAGED_BASE_DIR,
        &[fx.mods_dir.clone()],
    )
    .unwrap();
    for path in [
        "Mixed/A/Options/a.package",
        "Mixed/A/Options/b.package",
        "Mixed/B/Options/d.package",
        "Mixed/C/Options/e.package",
        "Mixed/C/Options/g.package",
        "Mixed/common.package",
    ] {
        assert!(fx.managed().join(path).is_file(), "{path}");
    }
    assert!(!fx.managed().join("Mixed/B/Options/c.package").exists());
    assert!(!fx.managed().join("Mixed/C/Options/f.package").exists());
}
#[test]
fn roupas_e_fontes_avulsas_nao_viram_variantes_por_substrings() {
    let fx = Fixture::new();
    let archive = fx.downloads.join("Fashion Options.zip");
    make_zip(
        &archive,
        &[
            ("Adoption/pick_top.package", b"top".to_vec()),
            (
                "Optional accessories/exclusive_skirt.package",
                b"skirt".to_vec(),
            ),
            ("Clothes/options_dress.package", b"dress".to_vec()),
        ],
    );
    let loose = fx.downloads.join("choose_one_shoes.package");
    write_file(&loose, b"shoes");
    prepare_staging(&[archive, loose], &fx.staging()).unwrap();
    let report = calculate_conflicts(&fx.staging(), &fx.mods_dir).unwrap();
    assert!(detect_mutually_exclusive(&report.staged_files).is_empty());
    execute_installation(
        &report,
        &fx.mods_dir,
        MANAGED_BASE_DIR,
        &[fx.mods_dir.clone()],
    )
    .unwrap();
    assert_eq!(
        fs::read(
            fx.managed()
                .join("Fashion Options/Adoption/pick_top.package")
        )
        .unwrap(),
        b"top"
    );
    assert_eq!(
        fs::read(
            fx.managed()
                .join("Fashion Options/Optional accessories/exclusive_skirt.package")
        )
        .unwrap(),
        b"skirt"
    );
    assert_eq!(
        fs::read(
            fx.managed()
                .join("Fashion Options/Clothes/options_dress.package")
        )
        .unwrap(),
        b"dress"
    );
    assert_eq!(
        fs::read(
            fx.managed()
                .join("choose_one_shoes/choose_one_shoes.package")
        )
        .unwrap(),
        b"shoes"
    );
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
    make_zip(&archive, &[("mod.package", b"DBPF conteudo qualquer".to_vec())]);

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
        key: ResourceKey { type_id: 0x03B33DDF, group_id: 0, instance_ex: 0, instance_low: 7 },
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
