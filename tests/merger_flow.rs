//! Testes do unificador: merge DBPF real e o destino dos arquivos originais.

use s4suite::engine::dbpf::{DBPFReader, DBPFWriter, PackageResource, ResourceKey};
use s4suite::engine::disabled::DisabledManager;
use s4suite::engine::merger::{
    common_ancestor, merge_sims4_packages, run_merge_queue, AuthorizedMergeJob, JobStatus,
    MergeJob, MergeReview, MergeTask, PostMergeAction,
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

    let jobs = [job(
        "Manter",
        originais.clone(),
        fx.output_dir.clone(),
        PostMergeAction::Keep,
    )];
    let outcome = run_merge_queue(&jobs, &fx.manager(), |_, _| {}, |_, _, _, _| {})
        .pop()
        .unwrap()
        .post
        .unwrap();

    assert_eq!(outcome.affected, 0);
    assert!(originais.iter().all(|p| p.exists()));
}

#[test]
fn pos_merge_desativar_renomeia_originais_para_disabled() {
    let fx = Fixture::new();
    write_package(&fx.input_dir.join("a.package"), &[1], 100);
    write_package(&fx.input_dir.join("b.package"), &[2], 100);
    let originais = fx.inputs();

    let jobs = [job(
        "Desativar",
        originais.clone(),
        fx.output_dir.clone(),
        PostMergeAction::Disable,
    )];
    let outcome = run_merge_queue(&jobs, &fx.manager(), |_, _| {}, |_, _, _, _| {})
        .pop()
        .unwrap()
        .post
        .unwrap();

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

    let jobs = [job(
        "Excluir",
        originais.clone(),
        fx.output_dir.clone(),
        PostMergeAction::Delete,
    )];
    let outcome = run_merge_queue(&jobs, &fx.manager(), |_, _| {}, |_, _, _, _| {})
        .pop()
        .unwrap()
        .post
        .unwrap();

    assert_eq!(outcome.affected, 1);
    assert!(outcome.freed_bytes > 0);
    assert!(!originais[0].exists());
}

/// Entradas com origem comum ampla não recebem autorização destrutiva.
#[test]
fn pos_merge_excluir_recusa_origens_sem_escopo_seguro() {
    let fx = Fixture::new();
    let external = TempDir::new().unwrap();
    write_package(&fx.input_dir.join("a.package"), &[1], 100);
    let intruso = external.path().join("arquivo_de_fora.package");
    write_package(&intruso, &[9], 100);
    let mut alvos = fx.inputs();
    alvos.push(intruso.clone());
    let candidate = raw_job(
        "Fora",
        alvos.clone(),
        fx.output_dir.clone(),
        PostMergeAction::Delete,
    );
    assert!(MergeReview::prepare(candidate).is_err());
    assert!(alvos.iter().all(|path| path.exists()));
}

#[test]
fn pos_merge_backup_gera_zip_antes_de_remover_os_originais() {
    let fx = Fixture::new();
    write_package(&fx.input_dir.join("a.package"), &[1], 500);
    write_package(&fx.input_dir.join("b.package"), &[2], 500);
    let originais = fx.inputs();

    let jobs = [job(
        "Backup",
        originais.clone(),
        fx.output_dir.clone(),
        PostMergeAction::Backup,
    )];
    let outcome = run_merge_queue(&jobs, &fx.manager(), |_, _| {}, |_, _, _, _| {})
        .pop()
        .unwrap()
        .post
        .unwrap();

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

fn raw_job(nome: &str, entradas: Vec<PathBuf>, saida: PathBuf, pos: PostMergeAction) -> MergeJob {
    MergeJob {
        name: nome.to_string(),
        input_files: entradas,
        output_dir: saida,
        max_size_bytes: 1_000_000_000,
        post_action: pos,
    }
}

fn authorize(candidate: MergeJob) -> AuthorizedMergeJob {
    let review = MergeReview::prepare(candidate).unwrap();
    let phrase = if review.requires_delete_confirmation() {
        "EXCLUIR ORIGINAIS"
    } else {
        ""
    };
    review.approve(phrase).unwrap()
}

fn job(
    nome: &str,
    entradas: Vec<PathBuf>,
    saida: PathBuf,
    pos: PostMergeAction,
) -> AuthorizedMergeJob {
    authorize(raw_job(nome, entradas, saida, pos))
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
        job(
            "Cabelos",
            vec![grupo_a],
            saida_a.clone(),
            PostMergeAction::Keep,
        ),
        job(
            "Roupas",
            vec![grupo_b],
            saida_b.clone(),
            PostMergeAction::Keep,
        ),
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
        job(
            "Manter",
            vec![manter.clone()],
            fx.output_dir.join("m"),
            PostMergeAction::Keep,
        ),
        job(
            "Apagar",
            vec![apagar.clone()],
            fx.output_dir.join("a"),
            PostMergeAction::Delete,
        ),
    ];

    let outcomes = run_merge_queue(&jobs, &fx.manager(), |_, _| {}, |_, _, _, _| {});

    assert!(outcomes.iter().all(|o| o.status == JobStatus::Done));
    assert!(manter.exists(), "o job com 'manter' não pode apagar nada");
    assert!(
        !apagar.exists(),
        "o job com 'excluir' deveria ter removido o original"
    );
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
    assert!(
        outcomes[0].post.is_none(),
        "a pós-ação não pode rodar num merge parcial"
    );
    assert!(bom.exists(), "nenhum original pode ser apagado aqui");
    assert!(corrompido.exists());
}

/// Uma tarefa que falha não pode levar as seguintes junto: a fila é o único
/// lugar onde o usuário deixa vários grupos rodando sem olhar.
#[test]
fn tarefa_que_falha_nao_interrompe_a_fila() {
    let fx = Fixture::new();
    let broken = fx.input_dir.join("Quebrado/invalid.package");
    write_package(&broken, &[8], 64);
    let bom = fx.input_dir.join("Bom/ok.package");
    write_package(&bom, &[1], 64);

    let jobs = vec![
        job(
            "Quebrado",
            vec![broken.clone()],
            fx.output_dir.join("v"),
            PostMergeAction::Delete,
        ),
        job(
            "Bom",
            vec![bom],
            fx.output_dir.join("b"),
            PostMergeAction::Keep,
        ),
    ];
    fs::remove_file(&broken).unwrap();

    let outcomes = run_merge_queue(&jobs, &fx.manager(), |_, _| {}, |_, _, _, _| {});

    assert_eq!(outcomes.len(), 2);
    assert_ne!(outcomes[0].status, JobStatus::Done);
    assert_eq!(
        outcomes[1].status,
        JobStatus::Done,
        "a segunda tarefa tinha que rodar"
    );
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
        authorize(MergeJob {
            name: "Cabelos".to_string(),
            input_files: vec![grupo_a.join("a1.package"), grupo_a.join("a2.package")],
            output_dir: fx.output_dir.clone(),
            max_size_bytes: 1024 * 1024 * 1024,
            post_action: PostMergeAction::Keep,
        }),
        authorize(MergeJob {
            name: "Roupas".to_string(),
            input_files: vec![grupo_b.join("b1.package")],
            output_dir: fx.output_dir.clone(),
            max_size_bytes: 1024 * 1024 * 1024,
            post_action: PostMergeAction::Keep,
        }),
    ];

    let outcomes = run_merge_queue(&jobs, &fx.manager(), |_, _| {}, |_, _, _, _| {});
    assert!(outcomes.iter().all(|o| o.status == JobStatus::Done));

    let gerados: Vec<PathBuf> = fs::read_dir(&fx.output_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|x| x == "package").unwrap_or(false))
        .collect();
    assert_eq!(
        gerados.len(),
        2,
        "cada tarefa precisa do seu próprio arquivo: {:?}",
        gerados
    );

    let total: usize = gerados
        .iter()
        .map(|p| {
            let mut f = File::open(p).unwrap();
            DBPFReader::read_index(&mut f).unwrap().1.len()
        })
        .sum();
    assert_eq!(
        total, 6,
        "os 6 recursos das duas tarefas precisam sobreviver"
    );

    assert!(fx
        .output_dir
        .join("Cabelos_Merged_Part001.package")
        .exists());
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

fn write_cc(path: &Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let resources = [
        PackageResource {
            key: ResourceKey {
                type_id: 0x034AEECB,
                ..key(1)
            },
            data: b"CAS fixture".to_vec(),
            mem_size: 11,
            compressed: 0,
        },
        PackageResource {
            key: ResourceKey {
                type_id: 0x00B2D882,
                ..key(2)
            },
            data: b"image fixture".to_vec(),
            mem_size: 13,
            compressed: 0,
        },
    ];
    DBPFWriter::write_package(&mut File::create(path).unwrap(), &resources).unwrap();
}

#[test]
fn roupas_e_substrings_genericas_nao_sao_indicios() {
    let root = tempfile::Builder::new()
        .prefix("gameplay-tuning-")
        .tempdir()
        .unwrap();
    let folder = root.path().join("Roupas");
    let names = [
        "beautiful_skin.package",
        "ui_fix_bug_core_default.package",
        "gameplayful_dress.package",
        "overridetop.package",
        "XMLace.package",
    ];
    let inputs: Vec<_> = names.iter().map(|name| folder.join(name)).collect();
    for path in &inputs {
        write_cc(path);
    }
    let review = MergeReview::prepare(raw_job(
        "CC",
        inputs,
        root.path().join("out"),
        PostMergeAction::Keep,
    ))
    .unwrap();
    assert!(review.risks().is_empty(), "{:?}", review.risks());
}

#[test]
fn tokens_completos_e_tuning_sao_indicios_nao_vereditos() {
    let fx = Fixture::new();
    let named = fx.input_dir.join("Replacement/coat.package");
    let tuning = fx.input_dir.join("normal.package");
    write_cc(&named);
    write_package(&tuning, &[4], 40);
    let review = MergeReview::prepare(raw_job(
        "Indícios",
        vec![named.clone(), tuning.clone()],
        fx.output_dir.clone(),
        PostMergeAction::Keep,
    ))
    .unwrap();
    assert!(review
        .risks()
        .iter()
        .any(|risk| risk.file == named && risk.reason.contains("indício")));
    assert!(review
        .risks()
        .iter()
        .any(|risk| risk.file == tuning && risk.reason.contains("tuning/XML")));
}

#[test]
fn scripts_somente_no_diretorio_imediato_da_entrada() {
    let fx = Fixture::new();
    let input = fx.input_dir.join("CC/a.package");
    let nearby = fx.input_dir.join("CC/related.ts4script");
    let unrelated = fx.input_dir.join("Other/else.ts4script");
    let descendant = fx.input_dir.join("CC/deep/else.ts4script");
    write_cc(&input);
    for path in [&nearby, &unrelated, &descendant] {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"script fixture").unwrap();
    }
    let review = MergeReview::prepare(raw_job(
        "CC",
        vec![input],
        fx.output_dir.clone(),
        PostMergeAction::Keep,
    ))
    .unwrap();
    assert_eq!(review.risks().len(), 1);
    assert_eq!(review.risks()[0].file, nearby);
    assert!(review.risks()[0]
        .reason
        .contains("associação não comprovada"));
    assert!(MergeReview::prepare(raw_job(
        "Script",
        vec![unrelated],
        fx.output_dir.clone(),
        PostMergeAction::Keep
    ))
    .is_err());
}

#[test]
fn cada_job_revisa_somente_sua_selecao() {
    let fx = Fixture::new();
    let cc = fx.input_dir.join("CC/a.package");
    let tuning = fx.input_dir.join("Tuning/b.package");
    write_cc(&cc);
    write_package(&tuning, &[2], 64);
    let first = MergeReview::prepare(raw_job(
        "CC",
        vec![cc.clone()],
        fx.output_dir.clone(),
        PostMergeAction::Keep,
    ))
    .unwrap();
    let second = MergeReview::prepare(raw_job(
        "Tuning",
        vec![tuning.clone()],
        fx.output_dir.clone(),
        PostMergeAction::Delete,
    ))
    .unwrap();
    assert!(first.risks().is_empty());
    assert!(!second.risks().is_empty());
    assert!(second.risks().iter().all(|risk| risk.file == tuning));
    let tokens = [
        first.approve("").unwrap(),
        second.approve("EXCLUIR ORIGINAIS").unwrap(),
    ];
    let outcomes = run_merge_queue(&tokens, &fx.manager(), |_, _| {}, |_, _, _, _| {});
    assert!(outcomes
        .iter()
        .all(|outcome| outcome.status == JobStatus::Done));
    assert!(cc.exists());
    assert!(!tuning.exists());
}

#[test]
fn delete_exige_frase_literal_sem_normalizacao() {
    let fx = Fixture::new();
    write_package(&fx.input_dir.join("a.package"), &[1], 40);
    for phrase in [
        "",
        "excluir originais",
        "EXCLUIR ORIGINAIS ",
        " EXCLUIR ORIGINAIS",
        "DELETE ORIGINALS",
    ] {
        let review = MergeReview::prepare(raw_job(
            "Delete",
            fx.inputs(),
            fx.output_dir.clone(),
            PostMergeAction::Delete,
        ))
        .unwrap();
        assert!(review.requires_delete_confirmation());
        assert!(review.approve(phrase).is_err());
    }
    let token = authorize(raw_job(
        "Delete",
        fx.inputs(),
        fx.output_dir.clone(),
        PostMergeAction::Delete,
    ));
    assert_eq!(token.job().post_action, PostMergeAction::Delete);
}

#[test]
fn configuracao_alterada_exige_outra_revisao_e_nao_muda_token() {
    let fx = Fixture::new();
    let a = fx.input_dir.join("a.package");
    let b = fx.input_dir.join("b.package");
    write_cc(&a);
    write_cc(&b);
    let initial = raw_job(
        "Inicial",
        vec![a],
        fx.output_dir.clone(),
        PostMergeAction::Keep,
    );
    let token = authorize(initial.clone());
    for change in 0..3 {
        let mut modified = token.job().clone();
        match change {
            0 => modified.input_files = vec![b.clone()],
            1 => modified.output_dir = fx.output_dir.join("outro"),
            _ => modified.post_action = PostMergeAction::Delete,
        }
        assert_ne!(token.job(), &modified);
        let other = authorize(modified.clone());
        assert_eq!(other.job(), &modified);
        assert_eq!(token.job(), &initial);
    }
}

#[test]
fn alteracoes_apos_aprovacao_preservam_todos_os_originais() {
    for kind in 0..5 {
        let fx = Fixture::new();
        let path = fx.input_dir.join("a.package");
        let script = fx.input_dir.join("associated.ts4script");
        write_package(&path, &[1], 64);
        fs::write(&script, b"original script").unwrap();
        fs::create_dir_all(&fx.output_dir).unwrap();
        let token = job(
            "Guard",
            vec![path.clone()],
            fx.output_dir.clone(),
            PostMergeAction::Delete,
        );
        match kind {
            0 => write_package(&path, &[2], 64),
            1 => {
                let replacement = fx.input_dir.join("replacement");
                write_package(&replacement, &[1], 64);
                fs::rename(replacement, &path).unwrap();
            }
            2 => {
                fs::write(&script, b"modified script").unwrap();
            }
            3 => {
                fs::write(fx.input_dir.join("new.ts4script"), b"new script").unwrap();
            }
            _ => {
                fs::rename(&fx.output_dir, fx._root.path().join("old-output")).unwrap();
                fs::create_dir_all(&fx.output_dir).unwrap();
            }
        }
        let outcomes = run_merge_queue(&[token], &fx.manager(), |_, _| {}, |_, _, _, _| {});
        assert_eq!(outcomes[0].status, JobStatus::Failed, "kind {kind}");
        assert!(outcomes[0].error.is_some());
        assert!(outcomes[0].post.is_none());
        assert!(path.exists());
    }
}

#[test]
fn destino_antes_ausente_criado_externamente_invalida_token() {
    let fx = Fixture::new();
    write_package(&fx.input_dir.join("a.package"), &[1], 64);
    let token = job(
        "Guard",
        fx.inputs(),
        fx.output_dir.clone(),
        PostMergeAction::Delete,
    );
    fs::create_dir_all(&fx.output_dir).unwrap();
    let outcomes = run_merge_queue(&[token], &fx.manager(), |_, _| {}, |_, _, _, _| {});
    assert_eq!(outcomes[0].status, JobStatus::Failed);
    assert!(fx.input_dir.join("a.package").exists());
}

#[test]
fn mudanca_durante_merge_bloqueia_pos_acao() {
    for mutate_script in [false, true] {
        let fx = Fixture::new();
        let input = fx.input_dir.join("a.package");
        let script = fx.input_dir.join("associated.ts4script");
        write_package(&input, &[1, 2], 64);
        fs::write(&script, b"script").unwrap();
        let token = job(
            "Guard",
            vec![input.clone()],
            fx.output_dir.clone(),
            PostMergeAction::Delete,
        );
        let changed = std::sync::atomic::AtomicBool::new(false);
        let outcomes = run_merge_queue(
            &[token],
            &fx.manager(),
            |_, _| {},
            |_, _, _, text| {
                if text.starts_with("Processando Recursos")
                    && !changed.swap(true, std::sync::atomic::Ordering::SeqCst)
                {
                    if mutate_script {
                        fs::write(&script, b"new script").unwrap();
                    } else {
                        write_package(&input, &[3, 4], 64);
                    }
                }
            },
        );
        assert_eq!(outcomes[0].status, JobStatus::Partial);
        assert!(outcomes[0].error.is_some());
        assert!(outcomes[0].post.is_none());
        assert!(input.exists());
    }
}

#[test]
fn entradas_duplicadas_diretorios_e_destinos_amplos_sao_recusados() {
    let fx = Fixture::new();
    let input = fx.input_dir.join("a.package");
    write_cc(&input);
    assert!(MergeReview::prepare(raw_job(
        "Dup",
        vec![input.clone(), input.clone()],
        fx.output_dir.clone(),
        PostMergeAction::Keep
    ))
    .is_err());
    let folder = fx.input_dir.join("folder.package");
    fs::create_dir(&folder).unwrap();
    assert!(MergeReview::prepare(raw_job(
        "Dir",
        vec![folder],
        fx.output_dir.clone(),
        PostMergeAction::Keep
    ))
    .is_err());
    for destination in [
        PathBuf::from("/"),
        PathBuf::from("/home"),
        std::env::temp_dir(),
        dirs::home_dir().unwrap(),
    ] {
        assert!(MergeReview::prepare(raw_job(
            "Broad",
            vec![input.clone()],
            destination,
            PostMergeAction::Keep
        ))
        .is_err());
    }
    assert!(MergeReview::prepare(raw_job(
        "File",
        vec![input.clone()],
        input,
        PostMergeAction::Keep
    ))
    .is_err());
}

#[test]
fn payload_ausente_ou_zlib_invalido_nunca_autoriza_exclusao() {
    for kind in 0..3 {
        let fx = Fixture::new();
        let bad = fx.input_dir.join("bad.package");
        let good = fx.input_dir.join("good.package");
        write_package(&bad, &[1], 64);
        write_package(&good, &[2], 64);
        if kind < 2 {
            let mut bytes = fs::read(&bad).unwrap();
            let index = u32::from_le_bytes(bytes[64..68].try_into().unwrap()) as usize;
            let offset = if kind == 0 {
                u32::MAX
            } else {
                (bytes.len() - 1) as u32
            };
            bytes[index + 20..index + 24].copy_from_slice(&offset.to_le_bytes());
            fs::write(&bad, bytes).unwrap();
        } else {
            DBPFWriter::write_package(
                &mut File::create(&bad).unwrap(),
                &[PackageResource {
                    key: key(1),
                    data: b"not zlib".to_vec(),
                    mem_size: 64,
                    compressed: 0x5A42,
                }],
            )
            .unwrap();
        }
        let token = job(
            "Payload",
            vec![bad.clone(), good.clone()],
            fx.output_dir.clone(),
            PostMergeAction::Delete,
        );
        let outcomes = run_merge_queue(&[token], &fx.manager(), |_, _| {}, |_, _, _, _| {});
        assert_eq!(outcomes[0].status, JobStatus::Partial);
        assert!(outcomes[0].post.is_none());
        assert!(!outcomes[0].report.as_ref().unwrap().failed_files.is_empty());
        assert!(bad.exists() && good.exists());
    }
}

#[test]
fn pacote_unico_corrompido_falha_sem_pos_acao() {
    let fx = Fixture::new();
    let input = fx.input_dir.join("bad.package");
    fs::write(&input, b"invalid DBPF").unwrap();
    let token = job(
        "Failed",
        vec![input.clone()],
        fx.output_dir.clone(),
        PostMergeAction::Delete,
    );
    let outcomes = run_merge_queue(&[token], &fx.manager(), |_, _| {}, |_, _, _, _| {});
    assert_eq!(outcomes[0].status, JobStatus::Failed);
    assert!(outcomes[0].post.is_none());
    assert!(input.exists());
}

#[test]
fn backup_colisao_preserva_arquivo_anterior_e_originais() {
    let fx = Fixture::new();
    let input = fx.input_dir.join("a.package");
    write_package(&input, &[1], 64);
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let existing: Vec<_> = (seconds.saturating_sub(60)..=seconds + 60)
        .map(|stamp| {
            fx.input_dir
                .join(format!("originais_pre_merge_{stamp}.zip"))
        })
        .collect();
    for path in &existing {
        fs::write(path, b"previous backup").unwrap();
    }
    let token = job(
        "Backup",
        vec![input.clone()],
        fx.output_dir.clone(),
        PostMergeAction::Backup,
    );
    let outcomes = run_merge_queue(&[token], &fx.manager(), |_, _| {}, |_, _, _, _| {});
    assert_eq!(outcomes[0].status, JobStatus::Partial);
    assert!(outcomes[0].post.as_ref().unwrap().error.is_some());
    assert!(input.exists());
    for path in existing {
        assert_eq!(fs::read(path).unwrap(), b"previous backup");
    }
}

#[cfg(unix)]
#[test]
fn links_e_hardlinks_ambiguos_nao_recebem_autorizacao() {
    use std::os::unix::fs::symlink;
    let fx = Fixture::new();
    let input = fx.input_dir.join("a.package");
    write_package(&input, &[1], 64);
    let link = fx.input_dir.join("link.package");
    symlink(&input, &link).unwrap();
    assert!(MergeReview::prepare(raw_job(
        "Link",
        vec![link],
        fx.output_dir.clone(),
        PostMergeAction::Delete
    ))
    .is_err());
    let dir_link = fx._root.path().join("alias");
    symlink(&fx.input_dir, &dir_link).unwrap();
    assert!(MergeReview::prepare(raw_job(
        "Ancestor",
        vec![dir_link.join("a.package")],
        fx.output_dir.clone(),
        PostMergeAction::Keep
    ))
    .is_err());
    let output_link = fx._root.path().join("output-link");
    symlink(&fx.input_dir, &output_link).unwrap();
    assert!(MergeReview::prepare(raw_job(
        "Output",
        vec![input.clone()],
        output_link.join("new"),
        PostMergeAction::Keep
    ))
    .is_err());
    let hardlink = fx.input_dir.join("hard.package");
    fs::hard_link(&input, &hardlink).unwrap();
    assert!(MergeReview::prepare(raw_job(
        "Hard",
        vec![input, hardlink],
        fx.output_dir.clone(),
        PostMergeAction::Keep
    ))
    .is_err());
}

#[cfg(unix)]
#[test]
fn symlink_substituido_apos_aprovar_preserva_alvo_externo() {
    use std::os::unix::fs::symlink;
    let fx = Fixture::new();
    let input = fx.input_dir.join("a.package");
    let external = fx._root.path().join("external.package");
    write_package(&input, &[1], 64);
    write_package(&external, &[2], 64);
    let token = job(
        "Link",
        vec![input.clone()],
        fx.output_dir.clone(),
        PostMergeAction::Delete,
    );
    fs::remove_file(&input).unwrap();
    symlink(&external, &input).unwrap();
    let outcomes = run_merge_queue(&[token], &fx.manager(), |_, _| {}, |_, _, _, _| {});
    assert_eq!(outcomes[0].status, JobStatus::Failed);
    assert!(outcomes[0].post.is_none());
    assert!(external.exists());
    assert!(fs::symlink_metadata(input)
        .unwrap()
        .file_type()
        .is_symlink());
}

#[test]
fn nome_de_saida_existente_nunca_sobrescreve_um_original() {
    let fx = Fixture::new();
    let input = fx.input_dir.join("Grupo_Merged_Part001.package");
    write_package(&input, &[1], 64);
    let original = fs::read(&input).unwrap();
    let token = job(
        "Grupo",
        vec![input.clone()],
        fx.input_dir.clone(),
        PostMergeAction::Keep,
    );
    let outcomes = run_merge_queue(&[token], &fx.manager(), |_, _| {}, |_, _, _, _| {});
    assert_eq!(outcomes[0].status, JobStatus::Done);
    assert_eq!(fs::read(input).unwrap(), original);
    assert!(fx.input_dir.join("Grupo_Merged_2_Part001.package").exists());
}

#[test]
fn zlib_valido_permanece_comprimido_sem_mudar_a_estrategia() {
    use std::io::Write;
    let fx = Fixture::new();
    let input = fx.input_dir.join("compressed.package");
    let payload = b"synthetic tuning payload";
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(payload).unwrap();
    let compressed = encoder.finish().unwrap();
    DBPFWriter::write_package(
        &mut File::create(&input).unwrap(),
        &[PackageResource {
            key: key(1),
            data: compressed.clone(),
            mem_size: payload.len() as u32,
            compressed: 0x5A42,
        }],
    )
    .unwrap();
    let token = job(
        "Zlib",
        vec![input.clone()],
        fx.output_dir.clone(),
        PostMergeAction::Keep,
    );
    let outcomes = run_merge_queue(&[token], &fx.manager(), |_, _| {}, |_, _, _, _| {});
    assert_eq!(outcomes[0].status, JobStatus::Done);
    let mut result = File::open(&outcomes[0].report.as_ref().unwrap().output_packages[0]).unwrap();
    let (_, index) = DBPFReader::read_index(&mut result).unwrap();
    assert_eq!(index[0].compressed, 0x5A42);
    assert_eq!(index[0].file_size as usize, compressed.len());
    assert!(input.exists());
}

#[cfg(unix)]
#[test]
fn troca_do_destino_por_link_durante_merge_nao_publica_fora() {
    use std::os::unix::fs::symlink;
    let fx = Fixture::new();
    let input = fx.input_dir.join("a.package");
    let outside = fx._root.path().join("outside");
    fs::create_dir(&outside).unwrap();
    write_package(&input, &[1], 64);
    let token = job(
        "Swap",
        vec![input.clone()],
        fx.output_dir.clone(),
        PostMergeAction::Delete,
    );
    let changed = std::sync::atomic::AtomicBool::new(false);
    let outcomes = run_merge_queue(
        &[token],
        &fx.manager(),
        |_, _| {},
        |_, _, _, text| {
            if text.starts_with("Processando Recursos")
                && !changed.swap(true, std::sync::atomic::Ordering::SeqCst)
            {
                fs::rename(&fx.output_dir, fx._root.path().join("old-output")).unwrap();
                symlink(&outside, &fx.output_dir).unwrap();
            }
        },
    );
    assert_eq!(outcomes[0].status, JobStatus::Failed);
    assert!(outcomes[0].post.is_none());
    assert!(input.exists());
    assert_eq!(fs::read_dir(outside).unwrap().count(), 0);
}

#[cfg(unix)]
#[test]
fn ancestral_de_entrada_substituido_por_link_invalida_token() {
    use std::os::unix::fs::symlink;
    let fx = Fixture::new();
    let input = fx.input_dir.join("a.package");
    write_package(&input, &[1], 64);
    let token = job(
        "Ancestor",
        vec![input],
        fx.output_dir.clone(),
        PostMergeAction::Delete,
    );
    let old = fx._root.path().join("old-input");
    fs::rename(&fx.input_dir, &old).unwrap();
    symlink(&old, &fx.input_dir).unwrap();
    let outcomes = run_merge_queue(&[token], &fx.manager(), |_, _| {}, |_, _, _, _| {});
    assert_eq!(outcomes[0].status, JobStatus::Failed);
    assert!(outcomes[0].post.is_none());
    assert!(old.join("a.package").exists());
}
