//! Testes do importador de Sims e Lotes: preparo das fontes, classificação do
//! conteúdo e importação para as pastas do jogo.

use s4suite::engine::tray::{
    clear_tray_work_dir, import_tray_item, prepare_tray_candidates, sanitize_import_name,
};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn write_file(path: &Path, content: &[u8]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, content).unwrap();
}

fn make_zip(path: &Path, entries: &[(&str, Vec<u8>)]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    let file = fs::File::create(path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    let opts = zip::write::SimpleFileOptions::default();
    for (name, content) in entries {
        zip.start_file(*name, opts).unwrap();
        zip.write_all(content).unwrap();
    }
    zip.finish().unwrap();
}

fn package_bytes(payload: &str) -> Vec<u8> {
    let mut data = b"DBPF".to_vec();
    data.extend_from_slice(&[0u8; 92]);
    data.extend_from_slice(payload.as_bytes());
    data
}

struct Fixture {
    _root: TempDir,
    downloads: PathBuf,
    work_dir: PathBuf,
    tray_dir: PathBuf,
    mods_dir: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = TempDir::new().unwrap();
        let downloads = root.path().join("Downloads");
        let work_dir = root.path().join("work/tray_staging");
        let tray_dir = root.path().join("Tray");
        let mods_dir = root.path().join("Mods");
        fs::create_dir_all(&downloads).unwrap();
        fs::create_dir_all(&tray_dir).unwrap();
        fs::create_dir_all(&mods_dir).unwrap();
        Self { _root: root, downloads, work_dir, tray_dir, mods_dir }
    }

    fn roots(&self) -> Vec<PathBuf> {
        vec![self.tray_dir.clone(), self.mods_dir.clone()]
    }
}

#[test]
fn nome_de_importacao_e_sanitizado() {
    // Caractere proibido é removido, e o espaço duplo que a remoção deixa
    // para trás é colapsado — nada de pasta chamada "Maria  Silva".
    assert_eq!(sanitize_import_name(Some("Maria / Silva")), "Maria Silva");
    assert_eq!(sanitize_import_name(Some("  Mod_v2-final!  ")), "Mod_v2-final");
    // Sem nada aproveitável, cai no rótulo padrão em vez de virar pasta vazia.
    assert_eq!(sanitize_import_name(Some("///")), "Imported_Item");
    assert!(!sanitize_import_name(None).is_empty());
}

#[test]
fn zip_com_sim_e_cc_e_classificado_corretamente() {
    let fx = Fixture::new();
    let archive = fx.downloads.join("MariaSilva.zip");
    make_zip(
        &archive,
        &[
            ("Maria.trayitem", b"tray-binary".to_vec()),
            ("Maria.householdbinary", b"household".to_vec()),
            ("cabelo.package", package_bytes("cc1")),
            ("roupa.package", package_bytes("cc2")),
            ("leiame.txt", b"ignore".to_vec()),
        ],
    );

    let (candidatos, falhas) = prepare_tray_candidates(&[archive], &fx.work_dir, None).unwrap();

    assert!(falhas.is_empty());
    assert_eq!(candidatos.len(), 1);

    let c = &candidatos[0];
    assert_eq!(c.analysis.tray_files.len(), 2);
    assert_eq!(c.analysis.package_files.len(), 2);
    assert_eq!(c.total_files(), 4);
    assert_eq!(c.kind_label(), "Sim / Lote + CC");
}

#[test]
fn zip_apenas_com_cc_e_rotulado_como_tal() {
    let fx = Fixture::new();
    let archive = fx.downloads.join("PackCC.zip");
    make_zip(&archive, &[("a.package", package_bytes("a")), ("b.package", package_bytes("b"))]);

    let (candidatos, _) = prepare_tray_candidates(&[archive], &fx.work_dir, None).unwrap();

    assert_eq!(candidatos[0].kind_label(), "Somente CC");
    assert!(candidatos[0].analysis.tray_files.is_empty());
}

#[test]
fn arquivo_de_tray_solto_tambem_e_aceito() {
    let fx = Fixture::new();
    let solto = fx.downloads.join("MeuSim.trayitem");
    write_file(&solto, b"tray-binary-content");

    let (candidatos, falhas) = prepare_tray_candidates(&[solto], &fx.work_dir, None).unwrap();

    assert!(falhas.is_empty());
    assert_eq!(candidatos.len(), 1);
    assert_eq!(candidatos[0].kind_label(), "Sim / Lote");
}

#[test]
fn fonte_sem_conteudo_util_vira_falha_e_nao_candidato() {
    let fx = Fixture::new();
    let vazio = fx.downloads.join("so_textos.zip");
    make_zip(&vazio, &[("leiame.txt", b"nada util".to_vec())]);

    let (candidatos, falhas) = prepare_tray_candidates(&[vazio], &fx.work_dir, None).unwrap();

    assert!(candidatos.is_empty());
    assert_eq!(falhas.len(), 1);
}

#[test]
fn uma_fonte_ruim_nao_aborta_as_boas() {
    let fx = Fixture::new();
    let bom = fx.downloads.join("bom.zip");
    make_zip(&bom, &[("sim.trayitem", b"tray".to_vec())]);
    let ruim = fx.downloads.join("ruim.zip");
    write_file(&ruim, b"isto nao e um zip");

    let (candidatos, falhas) = prepare_tray_candidates(&[bom, ruim], &fx.work_dir, None).unwrap();

    assert_eq!(candidatos.len(), 1);
    assert_eq!(falhas.len(), 1);
}

/// Dois arquivos de mesmo nome não podem se sobrescrever no diretório de
/// trabalho — cada fonte recebe uma pasta própria prefixada por índice.
#[test]
fn fontes_homonimas_nao_se_sobrescrevem() {
    let fx = Fixture::new();
    let a = fx.downloads.join("pasta_a/sim.zip");
    let b = fx.downloads.join("pasta_b/sim.zip");
    make_zip(&a, &[("primeiro.trayitem", b"um".to_vec())]);
    make_zip(&b, &[("segundo.trayitem", b"dois".to_vec())]);

    let (candidatos, _) = prepare_tray_candidates(&[a, b], &fx.work_dir, None).unwrap();

    assert_eq!(candidatos.len(), 2);
    let nomes: Vec<&str> = candidatos
        .iter()
        .flat_map(|c| c.analysis.tray_files.iter())
        .map(|f| f.rel_name.as_str())
        .collect();
    assert!(nomes.contains(&"primeiro.trayitem"));
    assert!(nomes.contains(&"segundo.trayitem"));
}

#[test]
fn preparo_limpa_o_trabalho_da_rodada_anterior() {
    let fx = Fixture::new();
    write_file(&fx.work_dir.join("sobra_antiga/x.trayitem"), b"velho");

    let archive = fx.downloads.join("novo.zip");
    make_zip(&archive, &[("novo.trayitem", b"tray".to_vec())]);
    prepare_tray_candidates(&[archive], &fx.work_dir, None).unwrap();

    assert!(!fx.work_dir.join("sobra_antiga").exists());
}

#[test]
fn importacao_separa_tray_do_cc_nas_pastas_certas() {
    let fx = Fixture::new();
    let archive = fx.downloads.join("Maria.zip");
    make_zip(
        &archive,
        &[
            ("Maria.trayitem", b"tray-binary".to_vec()),
            ("cabelo.package", package_bytes("cc")),
        ],
    );

    let (candidatos, _) = prepare_tray_candidates(&[archive], &fx.work_dir, None).unwrap();
    let c = &candidatos[0];

    let (tray_n, cc_n, pulados) =
        import_tray_item(&c.analysis, &fx.tray_dir, &c.cc_dir(&fx.mods_dir), &fx.roots()).unwrap();

    assert_eq!(tray_n, 1);
    assert_eq!(cc_n, 1);
    assert_eq!(pulados, 0);

    // Arquivos de Tray vão para a pasta Tray do jogo.
    assert!(fx.tray_dir.join("Maria.trayitem").exists());

    // O CC vai para uma subpasta identificada em Mods, não solto na raiz.
    let destino_cc = fx.mods_dir.join("Imported_Sims").join(&c.analysis.detected_name);
    assert!(destino_cc.join("cabelo.package").exists());
    assert!(!fx.mods_dir.join("cabelo.package").exists());
}

#[test]
fn limpeza_do_diretorio_de_trabalho_e_idempotente() {
    let fx = Fixture::new();
    write_file(&fx.work_dir.join("algo.trayitem"), b"x");

    clear_tray_work_dir(&fx.work_dir).unwrap();
    assert!(!fx.work_dir.exists());

    // Chamar de novo com o diretório já ausente não pode falhar.
    clear_tray_work_dir(&fx.work_dir).unwrap();
}

#[test]
fn destino_do_cc_pode_ser_trocado_por_item() {
    let fx = Fixture::new();
    let archive = fx.downloads.join("SimComCabelo.zip");
    make_zip(
        &archive,
        &[
            ("Sim.trayitem", b"tray-binary".to_vec()),
            ("cabelo.package", package_bytes("cc")),
        ],
    );

    let (mut candidatos, _) = prepare_tray_candidates(&[archive], &fx.work_dir, None).unwrap();
    // Um Sim que só traz cabelos costuma ir para a pasta de cabelos, não para
    // uma pasta com o nome do Sim.
    let escolhida = fx.mods_dir.join("CC/Cabelos");
    candidatos[0].cc_target = Some(escolhida.clone());

    let c = &candidatos[0];
    import_tray_item(&c.analysis, &fx.tray_dir, &c.cc_dir(&fx.mods_dir), &fx.roots()).unwrap();

    assert!(escolhida.join("cabelo.package").exists());
    assert!(
        !fx.mods_dir.join("Imported_Sims").exists(),
        "o destino padrão não deveria ter sido usado"
    );
    // Os binários de Tray não seguem o CC: eles só funcionam na pasta Tray.
    assert!(fx.tray_dir.join("Sim.trayitem").exists());
}

#[test]
fn destino_padrao_e_uma_subpasta_com_o_nome_do_item() {
    let fx = Fixture::new();
    let archive = fx.downloads.join("Maria.zip");
    make_zip(&archive, &[("cabelo.package", package_bytes("cc"))]);

    let (candidatos, _) = prepare_tray_candidates(&[archive], &fx.work_dir, None).unwrap();
    let c = &candidatos[0];

    assert_eq!(
        c.cc_dir(&fx.mods_dir),
        fx.mods_dir.join("Imported_Sims").join(&c.analysis.detected_name)
    );
    assert!(!c.skipped, "um item recém-analisado entra na fila para importar");
}
