//! Testes do gerenciador ReShade: detecção de ambiente Linux, detecção de
//! instalação em disco e a separação entre extração crua e extração filtrada.

use s4suite::engine::installer::{extract_archive, extract_archive_to_staging};
use s4suite::engine::reshade::{
    detect_environment, detect_installation, is_reshade_dll, uninstall_reshade, LinuxEnvType,
    MANIFEST_NAME,
};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

/// Bytes que passam pela checagem de assinatura do ReShade.
fn reshade_dll_bytes() -> Vec<u8> {
    let mut data = b"MZ".to_vec();
    data.extend_from_slice(&[0u8; 128]);
    data.extend_from_slice(b"ReShade 5.9.2");
    data
}

fn write_file(path: &Path, content: &[u8]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, content).unwrap();
}

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
    game_bin: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = TempDir::new().unwrap();
        let game_bin = root.path().join("The Sims 4/Game/Bin");
        fs::create_dir_all(&game_bin).unwrap();
        Self { _root: root, game_bin }
    }
}

#[test]
fn ambiente_linux_e_detectado_pelo_caminho() {
    let (tipo, cmd) = detect_environment(Path::new("/home/u/.steam/steamapps/common/The Sims 4/Game/Bin"));
    assert_eq!(tipo, LinuxEnvType::Steam);
    assert!(cmd.contains("%command%"), "Steam precisa da linha de opções de inicialização");

    let (tipo, _) = detect_environment(Path::new("/home/u/Games/lutris/sims4/Game/Bin"));
    assert_eq!(tipo, LinuxEnvType::LutrisBottles);

    let (tipo, _) = detect_environment(Path::new("/opt/jogos/sims4/Game/Bin"));
    assert_eq!(tipo, LinuxEnvType::Generic);
}

#[test]
fn todo_ambiente_orienta_o_override_da_dll() {
    for caminho in ["/x/steamapps/y", "/x/lutris/y", "/x/outro/y"] {
        let (_, cmd) = detect_environment(Path::new(caminho));
        assert!(
            cmd.contains("dxgi=n,b"),
            "instrução sem o override do dxgi para {}: {}",
            caminho,
            cmd
        );
    }
}

#[test]
fn assinatura_distingue_dll_do_reshade_de_outra_qualquer() {
    let fx = Fixture::new();

    let reshade = fx.game_bin.join("dxgi.dll");
    write_file(&reshade, &reshade_dll_bytes());
    assert!(is_reshade_dll(&reshade));

    let outra = fx.game_bin.join("d3d11.dll");
    write_file(&outra, b"MZ uma dll qualquer do sistema");
    assert!(!is_reshade_dll(&outra));

    assert!(!is_reshade_dll(&fx.game_bin.join("inexistente.dll")));
}

#[test]
fn instalacao_ausente_e_reportada_como_nao_instalada() {
    let fx = Fixture::new();
    let status = detect_installation(&fx.game_bin);

    assert!(!status.installed);
    assert!(status.injected_dll.is_none());
    assert_eq!(status.presets_count, 0);
}

/// A fonte da verdade é o disco: um ReShade instalado por fora, sem manifesto
/// da S4Suite, ainda precisa ser reconhecido.
#[test]
fn instalacao_feita_por_fora_e_reconhecida_sem_manifesto() {
    let fx = Fixture::new();
    write_file(&fx.game_bin.join("dxgi.dll"), &reshade_dll_bytes());

    let status = detect_installation(&fx.game_bin);

    assert!(status.installed);
    assert_eq!(status.injected_dll.as_deref(), Some("dxgi.dll"));
    assert!(status.manifest.is_none());
}

#[test]
fn manifesto_e_presets_entram_no_status() {
    let fx = Fixture::new();
    write_file(&fx.game_bin.join("d3d11.dll"), &reshade_dll_bytes());
    write_file(
        &fx.game_bin.join(MANIFEST_NAME),
        br#"{"installed_dll":"d3d11.dll","dll_sha256":"abc","installed_at":"1","presets":[]}"#,
    );
    write_file(&fx.game_bin.join("presets/cinematic.ini"), b"[preset]");
    write_file(&fx.game_bin.join("presets/vibrante.ini"), b"[preset]");

    let status = detect_installation(&fx.game_bin);

    assert!(status.installed);
    assert_eq!(status.injected_dll.as_deref(), Some("d3d11.dll"));
    assert_eq!(status.manifest.unwrap().installed_dll, "d3d11.dll");
    assert_eq!(status.presets_count, 2);
}

#[test]
fn desinstalar_remove_a_dll_e_restaura_o_backup() {
    let fx = Fixture::new();
    let dll = fx.game_bin.join("dxgi.dll");
    write_file(&dll, &reshade_dll_bytes());
    write_file(&fx.game_bin.join("dxgi.dll.backup.1"), b"MZ dll original do sistema");
    write_file(&fx.game_bin.join(MANIFEST_NAME), b"{}");

    uninstall_reshade(&fx.game_bin, &[fx.game_bin.clone()]).unwrap();

    assert!(!is_reshade_dll(&dll), "a DLL do ReShade deveria ter saído");
    assert_eq!(fs::read(&dll).unwrap(), b"MZ dll original do sistema");
    assert!(!fx.game_bin.join(MANIFEST_NAME).exists());
    assert!(!detect_installation(&fx.game_bin).installed);
}

/// Regressão: `install_reshade` extraía o setup por `extract_archive_to_staging`,
/// que bloqueia `.dll` e `.exe`. Como o instalador do ReShade é exatamente isso,
/// a instalação falhava sempre. A extração crua é o caminho correto ali.
#[test]
fn extracao_crua_aceita_dll_que_o_filtro_de_mods_bloqueia() {
    let fx = Fixture::new();
    let setup = fx._root.path().join("ReShade_Setup.zip");
    make_zip(&setup, &[("ReShade64.dll", reshade_dll_bytes())]);

    let filtrada = fx._root.path().join("saida_filtrada");
    let erro = extract_archive_to_staging(&setup, &filtrada).unwrap_err();
    assert!(
        erro.to_string().contains("reshade64.dll"),
        "o filtro de mods deveria recusar a DLL: {}",
        erro
    );

    let crua = fx._root.path().join("saida_crua");
    extract_archive(&setup, &crua).unwrap();
    assert!(crua.join("ReShade64.dll").exists());
    assert!(is_reshade_dll(&crua.join("ReShade64.dll")));
}

#[test]
fn remocao_recusada_nao_restaura_backup_sobre_dll_protegida() {
    let fx = Fixture::new();
    let dll = fx.game_bin.join("dxgi.dll");
    let bytes = reshade_dll_bytes();
    write_file(&dll, &bytes);
    let backup = fx.game_bin.join("dxgi.dll.backup.1");
    write_file(&backup, b"original");
    let unrelated = fx._root.path().join("other");
    fs::create_dir_all(&unrelated).unwrap();
    assert!(uninstall_reshade(&fx.game_bin, &[unrelated]).is_err());
    assert_eq!(fs::read(&dll).unwrap(), bytes);
    assert_eq!(fs::read(&backup).unwrap(), b"original");
}
