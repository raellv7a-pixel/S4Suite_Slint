//! Testes de integração do fluxo do instalador: seleção -> staging ->
//! conflitos -> instalação em Mods.

use s4suite::engine::installer::{
    calculate_conflicts, execute_installation, install_identity_name, is_valid_mod_file,
    prepare_staging, ConflictType, MANAGED_BASE_DIR, STAGING_DIR_NAME,
};
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
    assert_eq!(install_identity_name(Path::new("Meu Mod v2.1.zip")), "meu_mod_v2_1");
    assert_eq!(install_identity_name(Path::new("/tmp/UI-Cheats.package")), "ui_cheats");
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
    assert!(fx.staging().join("cool_mod/CoolMod/cool.package").exists());
}

#[test]
fn package_avulso_tambem_entra_no_staging() {
    let fx = Fixture::new();
    let loose = fx.downloads.join("UI Cheats.package");
    write_file(&loose, &package_bytes("ui"));

    prepare_staging(&[loose], &fx.staging()).unwrap();

    assert!(fx.staging().join("ui_cheats/UI Cheats.package").exists());
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
    assert_eq!(report.staged_files[0].destination(&fx.mods_dir), instalado);
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
        execute_installation(&report, &fx.mods_dir, &[fx.mods_dir.clone()]).unwrap();

    assert_eq!(installed, 2);
    assert_eq!(skipped, 0);

    // .package preserva a estrutura interna do arquivo compactado.
    assert!(fx
        .managed()
        .join("pacote_completo/mod/texturas/skin.package")
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
        execute_installation(&report, &fx.mods_dir, &[fx.mods_dir.clone()]).unwrap();

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
    execute_installation(&report, &fx.mods_dir, &[fx.mods_dir.clone()]).unwrap();

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
