//! Testes do unificador: merge DBPF real e o destino dos arquivos originais.

use s4suite::engine::dbpf::{DBPFReader, DBPFWriter, PackageResource, ResourceKey};
use s4suite::engine::disabled::DisabledManager;
use s4suite::engine::merger::{
    apply_post_merge_action, common_ancestor, merge_sims4_packages, run_merge_queue, JobStatus,
    MergeJob, MergeTask, PostMergeAction,
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
        name: None,
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
        name: None,
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
        name: None,
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
        name: None,
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

// --- Fila de tarefas ---

fn job(nome: &str, entradas: Vec<PathBuf>, saida: PathBuf, pos: PostMergeAction) -> MergeJob {
    MergeJob {
        name: nome.to_string(),
        input_files: entradas,
        output_dir: saida,
        max_size_bytes: 1_000_000_000,
        post_action: pos,
    }
}

#[test]
fn fila_executa_as_tarefas_em_ordem_e_reporta_cada_estado() {
    let fx = Fixture::new();
    let grupo_a = fx.input_dir.join("Cabelos/a.package");
    let grupo_b = fx.input_dir.join("Roupas/b.package");
    write_package(&grupo_a, &[1, 2], 64);
    write_package(&grupo_b, &[3], 64);

    let saida_a = fx.output_dir.join("Cabelos");
    let saida_b = fx.output_dir.join("Roupas");
    let jobs = vec![
        job("Cabelos", vec![grupo_a], saida_a.clone(), PostMergeAction::Keep),
        job("Roupas", vec![grupo_b], saida_b.clone(), PostMergeAction::Keep),
    ];

    let estados = std::sync::Mutex::new(Vec::new());
    let outcomes = run_merge_queue(
        &jobs,
        &fx.manager(),
        |idx, status| estados.lock().unwrap().push((idx, status)),
        |_, _, _, _| {},
    );

    assert_eq!(outcomes.len(), 2);
    assert!(outcomes.iter().all(|o| o.status == JobStatus::Done));

    // Cada job passa por Running antes de terminar, e na ordem da fila.
    let vistos = estados.lock().unwrap().clone();
    assert_eq!(
        vistos,
        vec![
            (0, JobStatus::Running),
            (0, JobStatus::Done),
            (1, JobStatus::Running),
            (1, JobStatus::Done),
        ]
    );

    // Cada tarefa tem seu próprio destino: nada é misturado.
    assert!(saida_a.exists() && saida_b.exists());
}

#[test]
fn cada_tarefa_da_fila_aplica_a_propria_acao_pos_merge() {
    let fx = Fixture::new();
    let manter = fx.input_dir.join("Manter/x.package");
    let apagar = fx.input_dir.join("Apagar/y.package");
    write_package(&manter, &[1], 64);
    write_package(&apagar, &[2], 64);

    let jobs = vec![
        job("Manter", vec![manter.clone()], fx.output_dir.join("m"), PostMergeAction::Keep),
        job("Apagar", vec![apagar.clone()], fx.output_dir.join("a"), PostMergeAction::Delete),
    ];

    let outcomes = run_merge_queue(&jobs, &fx.manager(), |_, _| {}, |_, _, _, _| {});

    assert!(outcomes.iter().all(|o| o.status == JobStatus::Done));
    assert!(manter.exists(), "o job com 'manter' não pode apagar nada");
    assert!(!apagar.exists(), "o job com 'excluir' deveria ter removido o original");
}

/// Com um merge parcial, apagar ou desativar os originais perderia o conteúdo
/// dos arquivos que não entraram na unificação.
#[test]
fn tarefa_parcial_nao_executa_a_acao_pos_merge() {
    let fx = Fixture::new();
    let bom = fx.input_dir.join("bom.package");
    let corrompido = fx.input_dir.join("corrompido.package");
    write_package(&bom, &[1], 64);
    fs::write(&corrompido, b"isto nao e um DBPF valido").unwrap();

    let jobs = vec![job(
        "Misto",
        vec![bom.clone(), corrompido.clone()],
        fx.output_dir.clone(),
        PostMergeAction::Delete,
    )];

    let outcomes = run_merge_queue(&jobs, &fx.manager(), |_, _| {}, |_, _, _, _| {});

    assert_eq!(outcomes[0].status, JobStatus::Partial);
    assert!(outcomes[0].post.is_none(), "a pós-ação não pode rodar num merge parcial");
    assert!(bom.exists(), "nenhum original pode ser apagado aqui");
    assert!(corrompido.exists());
}

/// Uma tarefa que falha não pode levar as seguintes junto: a fila é o único
/// lugar onde o usuário deixa vários grupos rodando sem olhar.
#[test]
fn tarefa_que_falha_nao_interrompe_a_fila() {
    let fx = Fixture::new();
    let vazio = fx.input_dir.join("Vazio");
    fs::create_dir_all(&vazio).unwrap();
    let bom = fx.input_dir.join("Bom/ok.package");
    write_package(&bom, &[1], 64);

    let jobs = vec![
        job("Vazio", Vec::new(), fx.output_dir.join("v"), PostMergeAction::Keep),
        job("Bom", vec![bom], fx.output_dir.join("b"), PostMergeAction::Keep),
    ];

    let outcomes = run_merge_queue(&jobs, &fx.manager(), |_, _| {}, |_, _, _, _| {});

    assert_eq!(outcomes.len(), 2);
    assert_ne!(outcomes[0].status, JobStatus::Done);
    assert_eq!(outcomes[1].status, JobStatus::Done, "a segunda tarefa tinha que rodar");
}

#[test]
fn progresso_da_fila_identifica_a_tarefa_de_origem() {
    let fx = Fixture::new();
    let a = fx.input_dir.join("A/a.package");
    let b = fx.input_dir.join("B/b.package");
    write_package(&a, &[1], 64);
    write_package(&b, &[2], 64);

    let jobs = vec![
        job("A", vec![a], fx.output_dir.join("a"), PostMergeAction::Keep),
        job("B", vec![b], fx.output_dir.join("b"), PostMergeAction::Keep),
    ];

    let indices = std::sync::Mutex::new(Vec::new());
    run_merge_queue(
        &jobs,
        &fx.manager(),
        |_, _| {},
        |idx, _, _, _| indices.lock().unwrap().push(idx),
    );

    let vistos = indices.lock().unwrap().clone();
    // Sem o índice, a barra de progresso da UI não saberia de qual tarefa é o
    // avanço que está recebendo.
    assert!(vistos.contains(&0) && vistos.contains(&1));
}

/// Duas tarefas para a mesma pasta não podem gravar uma por cima da outra.
///
/// O nome de saída era `Merged_Content_<segundos>_PartNNN`: numa fila rápida as
/// duas tarefas caíam no mesmo segundo, a segunda sobrescrevia a primeira e o
/// conteúdo dela sumia — sem erro na tela, e com os originais já consumidos pela
/// pós-ação.
#[test]
fn duas_tarefas_no_mesmo_destino_nao_se_sobrescrevem() {
    let fx = Fixture::new();
    let grupo_a = fx.input_dir.join("Cabelos");
    let grupo_b = fx.input_dir.join("Roupas");
    write_package(&grupo_a.join("a1.package"), &[1, 2], 400);
    write_package(&grupo_a.join("a2.package"), &[3], 400);
    write_package(&grupo_b.join("b1.package"), &[10, 11, 12], 400);

    let jobs = vec![
        MergeJob {
            name: "Cabelos".to_string(),
            input_files: vec![grupo_a.join("a1.package"), grupo_a.join("a2.package")],
            output_dir: fx.output_dir.clone(),
            max_size_bytes: 1024 * 1024 * 1024,
            post_action: PostMergeAction::Keep,
        },
        MergeJob {
            name: "Roupas".to_string(),
            input_files: vec![grupo_b.join("b1.package")],
            output_dir: fx.output_dir.clone(),
            max_size_bytes: 1024 * 1024 * 1024,
            post_action: PostMergeAction::Keep,
        },
    ];

    let outcomes = run_merge_queue(&jobs, &fx.manager(), |_, _| {}, |_, _, _, _| {});
    assert!(outcomes.iter().all(|o| o.status == JobStatus::Done));

    let gerados: Vec<PathBuf> = fs::read_dir(&fx.output_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|x| x == "package").unwrap_or(false))
        .collect();
    assert_eq!(gerados.len(), 2, "cada tarefa precisa do seu próprio arquivo: {:?}", gerados);

    let total: usize = gerados
        .iter()
        .map(|p| {
            let mut f = File::open(p).unwrap();
            DBPFReader::read_index(&mut f).unwrap().1.len()
        })
        .sum();
    assert_eq!(total, 6, "os 6 recursos das duas tarefas precisam sobreviver");

    assert!(fx.output_dir.join("Cabelos_Merged_Part001.package").exists());
    assert!(fx.output_dir.join("Roupas_Merged_Part001.package").exists());
    assert!(fx.output_dir.join("Cabelos_Merged_report.json").exists());
    assert!(fx.output_dir.join("Roupas_Merged_report.json").exists());
}

/// Unificar o mesmo grupo duas vezes preserva o resultado anterior.
#[test]
fn merge_repetido_do_mesmo_grupo_nao_apaga_o_anterior() {
    let fx = Fixture::new();
    write_package(&fx.input_dir.join("a.package"), &[1, 2], 300);

    let task = |n: &str| MergeTask {
        input_files: fx.inputs(),
        output_dir: fx.output_dir.clone(),
        max_size_bytes: 1024 * 1024 * 1024,
        name: Some(n.to_string()),
    };
    merge_sims4_packages(&task("Cabelos"), |_, _, _| {}).unwrap();
    let segundo = merge_sims4_packages(&task("Cabelos"), |_, _, _| {}).unwrap();

    assert!(fx.output_dir.join("Cabelos_Merged_Part001.package").exists());
    assert!(fx.output_dir.join("Cabelos_Merged_2_Part001.package").exists());
    assert!(segundo.output_packages[0].contains("Cabelos_Merged_2"));
}
