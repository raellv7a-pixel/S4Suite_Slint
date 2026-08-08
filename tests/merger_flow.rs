//! Testes do unificador: merge DBPF real e o destino dos arquivos originais.

use s4suite::engine::dbpf::{DBPFReader, DBPFWriter, PackageResource, ResourceKey};
use s4suite::engine::disabled::DisabledManager;
use s4suite::engine::merger::{
    apply_post_merge_action, common_ancestor, merge_sims4_packages, MergeTask, PostMergeAction,
};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn key(instance: u32) -> ResourceKey {
    ResourceKey { type_id: 0x0333406C, group_id: 0, instance_ex: 0, instance_low: instance }
}

/// Escreve um package com N recursos de `payload_size` bytes cada.
fn write_package(path: &Path, instances: &[u32], payload_size: usize) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    let resources: Vec<PackageResource> = instances
        .iter()
        .map(|i| PackageResource {
            key: key(*i),
            data: vec![(*i % 256) as u8; payload_size],
            mem_size: payload_size as u32,
            compressed: 0,
        })
        .collect();
    let mut f = File::create(path).unwrap();
    DBPFWriter::write_package(&mut f, &resources).unwrap();
}

struct Fixture {
    _root: TempDir,
    input_dir: PathBuf,
    output_dir: PathBuf,
    config_dir: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = TempDir::new().unwrap();
        let input_dir = root.path().join("PacksOriginais");
        let output_dir = root.path().join("Unificados");
        let config_dir = root.path().join("config");
        fs::create_dir_all(&input_dir).unwrap();
        Self { _root: root, input_dir, output_dir, config_dir }
    }

    fn manager(&self) -> DisabledManager {
        DisabledManager::new(&self.config_dir)
    }

    fn inputs(&self) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = fs::read_dir(&self.input_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().map(|x| x == "package").unwrap_or(false))
            .collect();
        v.sort();
        v
    }
}

#[test]
fn merge_reune_recursos_de_varios_packages_em_um_so() {
    let fx = Fixture::new();
    write_package(&fx.input_dir.join("a.package"), &[1, 2], 500);
    write_package(&fx.input_dir.join("b.package"), &[3, 4], 500);

    let task = MergeTask {
        input_files: fx.inputs(),
        output_dir: fx.output_dir.clone(),
        max_size_bytes: 1024 * 1024 * 1024,
    };
    let report = merge_sims4_packages(&task, |_, _, _| {}).unwrap();

    assert!(report.success);
    assert_eq!(report.success_count, 2);
    assert_eq!(report.output_packages.len(), 1);

    let mut merged = File::open(&report.output_packages[0]).unwrap();
    let (_h, index) = DBPFReader::read_index(&mut merged).unwrap();
    assert_eq!(index.len(), 4, "os 4 recursos deveriam estar na parte unificada");
}

#[test]
fn merge_divide_em_partes_ao_estourar_o_limite_de_tamanho() {
    let fx = Fixture::new();
    // 6 recursos de 100 KB. Com limite de 200 KB (margem de 90% = 180 KB),
    // cabem no máximo 2 por parte.
    write_package(&fx.input_dir.join("grande.package"), &[1, 2, 3, 4, 5, 6], 100 * 1024);

    let task = MergeTask {
        input_files: fx.inputs(),
        output_dir: fx.output_dir.clone(),
        max_size_bytes: 200 * 1024,
    };
    let report = merge_sims4_packages(&task, |_, _, _| {}).unwrap();

    assert!(report.output_packages.len() >= 3, "esperava várias partes, veio {}", report.output_packages.len());
    for part in &report.output_packages {
        assert!(Path::new(part).exists());
    }
}

#[test]
fn merge_reporta_progresso_ate_o_fim() {
    let fx = Fixture::new();
    write_package(&fx.input_dir.join("a.package"), &[1, 2, 3], 200);

    let ticks = std::sync::Mutex::new(Vec::new());
    let task = MergeTask {
        input_files: fx.inputs(),
        output_dir: fx.output_dir.clone(),
        max_size_bytes: 1024 * 1024 * 1024,
    };
    merge_sims4_packages(&task, |cur, total, _| {
        ticks.lock().unwrap().push((cur, total));
    })
    .unwrap();

    let ticks = ticks.into_inner().unwrap();
    assert!(!ticks.is_empty(), "o merge não reportou progresso nenhum");
    assert!(
        ticks.iter().all(|(cur, total)| *cur <= *total),
        "progresso passou de 100%: {:?}",
        ticks
    );
    let (last_cur, last_total) = *ticks.last().unwrap();
    assert_eq!(last_cur, last_total, "o último tick deveria fechar em 100%");
}

#[test]
fn arquivo_corrompido_nao_impede_o_merge_dos_demais() {
    let fx = Fixture::new();
    write_package(&fx.input_dir.join("bom.package"), &[1, 2], 300);
    fs::write(fx.input_dir.join("corrompido.package"), b"isto nao e um DBPF").unwrap();

    let task = MergeTask {
        input_files: fx.inputs(),
        output_dir: fx.output_dir.clone(),
        max_size_bytes: 1024 * 1024 * 1024,
    };
    let report = merge_sims4_packages(&task, |_, _, _| {}).unwrap();

    assert!(report.partial, "deveria ser um merge parcial");
    assert_eq!(report.success_count, 1);
    assert_eq!(report.failed_files.len(), 1);
    assert_eq!(report.output_packages.len(), 1);
}

#[test]
fn raiz_de_seguranca_e_a_pasta_comum_dos_originais() {
    let base = PathBuf::from("/tmp/Mods");
    let paths = vec![
        base.join("a/x.package"),
        base.join("a/b/y.package"),
        base.join("c/z.package"),
    ];

    assert_eq!(common_ancestor(&paths).unwrap(), base);
    assert_eq!(common_ancestor(&[]), None);
}

#[test]
fn pos_merge_manter_nao_toca_em_nada() {
    let fx = Fixture::new();
    write_package(&fx.input_dir.join("a.package"), &[1], 100);
    let originais = fx.inputs();

    let outcome =
        apply_post_merge_action(PostMergeAction::Keep, &originais, &fx.input_dir, &fx.manager());

    assert_eq!(outcome.affected, 0);
    assert!(originais.iter().all(|p| p.exists()));
}

#[test]
fn pos_merge_desativar_renomeia_originais_para_disabled() {
    let fx = Fixture::new();
    write_package(&fx.input_dir.join("a.package"), &[1], 100);
    write_package(&fx.input_dir.join("b.package"), &[2], 100);
    let originais = fx.inputs();

    let outcome =
        apply_post_merge_action(PostMergeAction::Disable, &originais, &fx.input_dir, &fx.manager());

    assert_eq!(outcome.affected, 2);
    assert_eq!(outcome.failed, 0);
    assert!(originais.iter().all(|p| !p.exists()));
    assert!(fx.input_dir.join("a.package.disabled").exists());
    assert!(fx.input_dir.join("b.package.disabled").exists());
}

#[test]
fn pos_merge_excluir_remove_os_originais() {
    let fx = Fixture::new();
    write_package(&fx.input_dir.join("a.package"), &[1], 1000);
    let originais = fx.inputs();

    let outcome =
        apply_post_merge_action(PostMergeAction::Delete, &originais, &fx.input_dir, &fx.manager());

    assert_eq!(outcome.affected, 1);
    assert!(outcome.freed_bytes > 0);
    assert!(!originais[0].exists());
}

/// A raiz de segurança é a pasta de origem: um caminho de fora que entre na
/// lista não pode ser apagado.
#[test]
fn pos_merge_excluir_recusa_caminho_fora_da_pasta_de_origem() {
    let fx = Fixture::new();
    write_package(&fx.input_dir.join("a.package"), &[1], 100);
    let intruso = fx._root.path().join("arquivo_de_fora.package");
    write_package(&intruso, &[9], 100);

    let mut alvos = fx.inputs();
    alvos.push(intruso.clone());

    let outcome =
        apply_post_merge_action(PostMergeAction::Delete, &alvos, &fx.input_dir, &fx.manager());

    assert_eq!(outcome.affected, 1);
    assert_eq!(outcome.failed, 1);
    assert!(intruso.exists(), "arquivo fora da raiz de segurança foi apagado");
}

#[test]
fn pos_merge_backup_gera_zip_antes_de_remover_os_originais() {
    let fx = Fixture::new();
    write_package(&fx.input_dir.join("a.package"), &[1], 500);
    write_package(&fx.input_dir.join("b.package"), &[2], 500);
    let originais = fx.inputs();

    let outcome =
        apply_post_merge_action(PostMergeAction::Backup, &originais, &fx.input_dir, &fx.manager());

    assert!(outcome.error.is_none());
    assert_eq!(outcome.affected, 2);

    let zip_path = outcome.backup_path.expect("backup deveria ter um caminho");
    assert!(zip_path.exists());

    // O zip precisa conter os dois originais antes de eles serem removidos.
    let mut archive = zip::ZipArchive::new(File::open(&zip_path).unwrap()).unwrap();
    assert_eq!(archive.len(), 2);
    let nomes: Vec<String> = (0..archive.len())
        .map(|i| archive.by_index(i).unwrap().name().to_string())
        .collect();
    assert!(nomes.contains(&"a.package".to_string()));
    assert!(nomes.contains(&"b.package".to_string()));

    assert!(originais.iter().all(|p| !p.exists()));
}
