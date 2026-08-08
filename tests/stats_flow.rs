//! Testes das estatísticas do painel: contagens, tamanho por pasta e as listas
//! de detalhe por trás de cada card.

use s4suite::engine::installer::{BACKUP_DIR_NAME, STAGING_DIR_NAME};
use s4suite::engine::stats::{collect_stats, ROOT_FOLDER_LABEL};
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn write_file(path: &Path, size: usize) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, vec![0u8; size]).unwrap();
}

struct Fixture {
    _root: TempDir,
    mods_dir: PathBuf,
    tray_dir: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = TempDir::new().unwrap();
        let mods_dir = root.path().join("Mods");
        let tray_dir = root.path().join("Tray");
        fs::create_dir_all(&mods_dir).unwrap();
        fs::create_dir_all(&tray_dir).unwrap();
        Self { _root: root, mods_dir, tray_dir }
    }

    fn stats(&self) -> s4suite::engine::stats::ModsStats {
        collect_stats(&self.mods_dir, &self.tray_dir)
    }
}

#[test]
fn contagem_separa_packages_de_scripts() {
    let fx = Fixture::new();
    write_file(&fx.mods_dir.join("a.package"), 100);
    write_file(&fx.mods_dir.join("Pasta/b.package"), 200);
    write_file(&fx.mods_dir.join("mod.ts4script"), 300);
    write_file(&fx.mods_dir.join("leiame.txt"), 999);

    let stats = fx.stats();

    assert_eq!(stats.mods_count, 2);
    assert_eq!(stats.scripts_count, 1);
    // O .txt não é mod: não conta e não entra no tamanho.
    assert_eq!(stats.total_size, 600);
}

#[test]
fn tamanho_por_pasta_agrupa_no_primeiro_nivel_e_ordena_pelo_maior() {
    let fx = Fixture::new();
    write_file(&fx.mods_dir.join("solto.package"), 50);
    write_file(&fx.mods_dir.join("Cabelos/a.package"), 300);
    write_file(&fx.mods_dir.join("Cabelos/Autor/b.package"), 200);
    write_file(&fx.mods_dir.join("Roupas/c.package"), 100);

    let stats = fx.stats();

    // O painel existe para responder "o que está ocupando espaço": a resposta
    // tem que estar na primeira linha.
    assert_eq!(stats.folder_sizes[0], ("Cabelos".to_string(), 500));
    assert_eq!(stats.folder_sizes[1], ("Roupas".to_string(), 100));
    assert_eq!(stats.folder_sizes[2], (ROOT_FOLDER_LABEL.to_string(), 50));
}

#[test]
fn pastas_internas_nao_entram_nas_estatisticas() {
    let fx = Fixture::new();
    write_file(&fx.mods_dir.join("real.package"), 100);
    write_file(&fx.mods_dir.join(BACKUP_DIR_NAME).join("copia.package"), 100);
    write_file(&fx.mods_dir.join(STAGING_DIR_NAME).join("temp.package"), 100);

    let stats = fx.stats();

    // Backups e staging são cópias do que já está contado: incluí-los mostraria
    // o dobro do espaço realmente ocupado.
    assert_eq!(stats.mods_count, 1);
    assert_eq!(stats.total_size, 100);
}

#[test]
fn lista_de_scripts_traz_os_caminhos_para_o_detalhe() {
    let fx = Fixture::new();
    write_file(&fx.mods_dir.join("Scripts/a.ts4script"), 10);
    write_file(&fx.mods_dir.join("b.ts4script"), 10);
    write_file(&fx.mods_dir.join("c.package"), 10);

    let stats = fx.stats();

    assert_eq!(stats.scripts.len(), 2);
    assert!(stats.scripts.iter().all(|p| p.extension().unwrap() == "ts4script"));
}

#[test]
fn tray_conta_apenas_arquivos_da_raiz() {
    let fx = Fixture::new();
    write_file(&fx.tray_dir.join("sim.trayitem"), 10);
    write_file(&fx.tray_dir.join("sim.householdbinary"), 10);
    fs::create_dir_all(fx.tray_dir.join("uma_pasta")).unwrap();

    let stats = fx.stats();

    assert_eq!(stats.tray_count, 2);
    assert_eq!(stats.tray_files.len(), 2);
}

#[test]
fn pasta_de_mods_inexistente_nao_quebra_o_painel() {
    let fx = Fixture::new();
    fs::remove_dir_all(&fx.mods_dir).unwrap();
    fs::remove_dir_all(&fx.tray_dir).unwrap();

    let stats = fx.stats();

    assert_eq!(stats.mods_count, 0);
    assert_eq!(stats.total_size, 0);
    assert!(stats.folder_sizes.is_empty());
}
