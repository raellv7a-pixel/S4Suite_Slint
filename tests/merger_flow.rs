//! Testes do unificador: merge DBPF real e o destino dos arquivos originais.

use s4suite::engine::dbpf::{DBPFReader, DBPFWriter, PackageResource, ResourceKey};
use s4suite::engine::disabled::DisabledManager;
use s4suite::engine::merger::{
    apply_post_merge_action_validated, common_ancestor, merge_sims4_packages, run_merge_queue,
    JobStatus, MergeJob, MergeReport, MergeTask, PostMergeAction,
};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn key(instance: u32) -> ResourceKey {
    ResourceKey {
        type_id: 0x0333406C,
        group_id: 0,
        instance_ex: 0,
        instance_low: instance,
    }
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
        Self {
            _root: root,
            input_dir,
            output_dir,
            config_dir,
        }
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

    fn run_merge(&self, inputs: &[PathBuf]) -> MergeReport {
        let task = MergeTask {
            input_files: inputs.to_vec(),
            output_dir: self.output_dir.clone(),
            max_size_bytes: 1_000_000_000,
            name: None,
        };
        merge_sims4_packages(&task, |_, _, _| {}).unwrap()
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
    let user_entries: Vec<_> = index
        .into_iter()
        .filter(|e| e.key.type_id != s4suite::engine::manifest::S4S_MANIFEST_TYPE)
        .collect();
    assert_eq!(
        user_entries.len(),
        4,
        "os 4 recursos deveriam estar na parte unificada"
    );
    assert!(s4suite::engine::manifest::MergedPackageManifest::read_from_package_file(
        &PathBuf::from(&report.output_packages[0])
    )
    .unwrap()
    .is_some());
}

#[test]
fn merge_divide_em_partes_ao_estourar_o_limite_de_tamanho() {
    let fx = Fixture::new();
    // 6 recursos de 100 KB. Com limite de 200 KB (margem de 90% = 180 KB),
    // cabem no máximo 2 por parte.
    write_package(
        &fx.input_dir.join("grande.package"),
        &[1, 2, 3, 4, 5, 6],
        100 * 1024,
    );

    let task = MergeTask {
        input_files: fx.inputs(),
        output_dir: fx.output_dir.clone(),
        max_size_bytes: 200 * 1024,
        name: None,
    };
    let report = merge_sims4_packages(&task, |_, _, _| {}).unwrap();

    assert!(
        report.output_packages.len() >= 3,
        "esperava várias partes, veio {}",
        report.output_packages.len()
    );
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
    fs::write(
        fx.input_dir.join("corrompido.package"),
        b"isto nao e um DBPF",
    )
    .unwrap();

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

    let report = fx.run_merge(&originais);
    let outcome = apply_post_merge_action_validated(
        PostMergeAction::Keep,
        &report,
        &originais,
        &fx.input_dir,
        &fx.manager(),
    );
    assert_eq!(outcome.affected, 0);
    assert!(originais.iter().all(|p| p.exists()));
}

#[test]
fn pos_merge_desativar_renomeia_originais_para_disabled() {
    let fx = Fixture::new();
    write_package(&fx.input_dir.join("a.package"), &[1], 100);
    write_package(&fx.input_dir.join("b.package"), &[2], 100);
    let originais = fx.inputs();

    let report = fx.run_merge(&originais);
    let outcome = apply_post_merge_action_validated(
        PostMergeAction::Disable,
        &report,
        &originais,
        &fx.input_dir,
        &fx.manager(),
    );
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

    let report = fx.run_merge(&originais);
    let outcome = apply_post_merge_action_validated(
        PostMergeAction::Delete,
        &report,
        &originais,
        &fx.input_dir,
        &fx.manager(),
    );
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

    let report = fx.run_merge(&fx.inputs());
    let outcome = apply_post_merge_action_validated(
        PostMergeAction::Delete,
        &report,
        &alvos,
        &fx.input_dir,
        &fx.manager(),
    );
    assert_eq!(outcome.affected, 0);
    assert!(outcome.error.is_some());
    assert!(
        alvos.iter().all(|p| p.exists()),
        "autorização divergente deve preservar todo o conjunto"
    );
    assert!(
        intruso.exists(),
        "arquivo fora da raiz de segurança foi apagado"
    );
}

#[test]
fn pos_merge_backup_gera_zip_antes_de_remover_os_originais() {
    let fx = Fixture::new();
    write_package(&fx.input_dir.join("a.package"), &[1], 500);
    write_package(&fx.input_dir.join("b.package"), &[2], 500);
    let originais = fx.inputs();
    let expected: Vec<_> = originais
        .iter()
        .map(|p| {
            (
                p.file_name().unwrap().to_string_lossy().to_string(),
                digest(p),
            )
        })
        .collect();

    let report = fx.run_merge(&originais);
    let outcome = apply_post_merge_action_validated(
        PostMergeAction::Backup,
        &report,
        &originais,
        &fx.input_dir,
        &fx.manager(),
    );
    assert!(outcome.error.is_none());
    assert_eq!(outcome.affected, 2);

    let zip_path = outcome.backup_path.expect("backup deveria ter um caminho");
    assert!(zip_path.exists());

    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut archive = zip::ZipArchive::new(File::open(&zip_path).unwrap()).unwrap();
    assert_eq!(archive.len(), expected.len());
    for (name, expected_hash) in expected {
        let mut entry = archive.by_name(&name).unwrap();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        assert_eq!(Sha256::digest(&bytes).as_slice(), expected_hash);
    }

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
    let vazio = fx.input_dir.join("Vazio");
    fs::create_dir_all(&vazio).unwrap();
    let bom = fx.input_dir.join("Bom/ok.package");
    write_package(&bom, &[1], 64);

    let jobs = vec![
        job(
            "Vazio",
            Vec::new(),
            fx.output_dir.join("v"),
            PostMergeAction::Keep,
        ),
        job(
            "Bom",
            vec![bom],
            fx.output_dir.join("b"),
            PostMergeAction::Keep,
        ),
    ];

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
            DBPFReader::read_index(&mut f)
                .unwrap()
                .1
                .into_iter()
                .filter(|e| e.key.type_id != s4suite::engine::manifest::S4S_MANIFEST_TYPE)
                .count()
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

    assert!(fx
        .output_dir
        .join("Cabelos_Merged_Part001.package")
        .exists());
    assert!(fx
        .output_dir
        .join("Cabelos_Merged_2_Part001.package")
        .exists());
    assert!(segundo.output_packages[0].contains("Cabelos_Merged_2"));
}

// --- Testes de regressão P0: estabilização do Merger DBPF ---

#[test]
fn package_truncado_na_leitura_de_payload_nao_gera_merge_com_sucesso_e_protege_originais() {
    let fx = Fixture::new();
    let bom = fx.input_dir.join("bom.package");
    write_package(&bom, &[1, 2], 500);

    // Cria um package com índice válido, mas trunca o arquivo no meio para simular download incompleto
    let truncado = fx.input_dir.join("truncado.package");
    write_package(&truncado, &[10, 11], 2000);
    let tamanho_original = fs::metadata(&truncado).unwrap().len();
    // Corta o arquivo pela metade (o índice DBPF no início ainda existe, mas os dados finais faltam)
    let f = fs::OpenOptions::new().write(true).open(&truncado).unwrap();
    f.set_len(tamanho_original / 2).unwrap();
    drop(f);

    let jobs = vec![MergeJob {
        name: "Protecao".to_string(),
        input_files: vec![bom.clone(), truncado.clone()],
        output_dir: fx.output_dir.clone(),
        max_size_bytes: 1_000_000_000,
        post_action: PostMergeAction::Delete,
    }];

    let outcomes = run_merge_queue(&jobs, &fx.manager(), |_, _| {}, |_, _, _, _| {});

    assert_ne!(
        outcomes[0].status,
        JobStatus::Done,
        "merge com arquivo truncado não pode dar Done"
    );
    assert!(
        outcomes[0].post.is_none(),
        "pós-ação destrutiva NUNCA pode rodar se houver falha de leitura"
    );

    let report = outcomes[0]
        .report
        .as_ref()
        .expect("deve haver relatório gerado");
    assert!(
        !report.success,
        "merge com payload truncado não pode ter success = true"
    );
    assert!(
        !report.failed_files.is_empty(),
        "arquivo truncado deve ser registrado em failed_files"
    );

    // Ambos os originais DEVEM permanecer intactos no disco
    assert!(
        bom.exists(),
        "arquivo bom original não pode ser apagado se o merge foi incompleto"
    );
    assert!(truncado.exists(), "arquivo truncado não pode ser apagado");

    // Nenhum arquivo temporário .s4tmp deve permanecer
    for entry in walkdir::WalkDir::new(&fx.output_dir)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let fname = entry.file_name().to_string_lossy();
        assert!(
            !fname.ends_with(".s4tmp"),
            "não deve deixar arquivos temporários: {}",
            entry.path().display()
        );
    }
}

#[test]
fn falha_de_leitura_durante_extracao_de_recurso_rejeita_sucesso_total() {
    let fx = Fixture::new();
    let a = fx.input_dir.join("valido.package");
    write_package(&a, &[1], 500);

    // Cria arquivo corrupto
    let b = fx.input_dir.join("quebrado.package");
    fs::write(
        &b,
        b"DBPF_cabecalho_falso_com_bytes_corrompidos_sem_indice_valido",
    )
    .unwrap();

    let task = MergeTask {
        input_files: vec![a.clone(), b.clone()],
        output_dir: fx.output_dir.clone(),
        max_size_bytes: 1_000_000_000,
        name: Some("TesteLeitura".to_string()),
    };

    let report = merge_sims4_packages(&task, |_, _, _| {}).unwrap();

    assert!(
        !report.success,
        "falha de leitura deve impedir success = true"
    );
    assert!(report.partial, "deve ser marcado como parcial");
    assert_eq!(report.failed_files.len(), 1);
    assert_eq!(report.success_count, 1);

    // As partes geradas devem conter apenas recursos verificados
    for part in &report.output_packages {
        let mut f = File::open(part).unwrap();
        let (header, index) = DBPFReader::read_index(&mut f).unwrap();
        assert_eq!(header.major, 2);
        let user_entries: Vec<_> = index
            .into_iter()
            .filter(|e| e.key.type_id != s4suite::engine::manifest::S4S_MANIFEST_TYPE)
            .collect();
        assert_eq!(
            user_entries.len(),
            1,
            "só o recurso do arquivo válido deve estar presente"
        );
    }
}

#[test]
fn multiplas_partes_com_validacao_atomica_e_sem_residuos_temporarios() {
    let fx = Fixture::new();
    // 9 recursos de 50 KB cada = 450 KB total. Com limite de 100 KB (90 KB seguro),
    // deve dividir em pelo menos 5 partes.
    let pkg = fx.input_dir.join("multiplo.package");
    write_package(&pkg, &[1, 2, 3, 4, 5, 6, 7, 8, 9], 50 * 1024);

    let task = MergeTask {
        input_files: vec![pkg],
        output_dir: fx.output_dir.clone(),
        max_size_bytes: 100 * 1024,
        name: Some("MultiPartes".to_string()),
    };

    let report = merge_sims4_packages(&task, |_, _, _| {}).unwrap();

    assert!(report.success);
    assert!(report.output_packages.len() >= 5);

    let mut total_recursos = 0;
    for part in &report.output_packages {
        let part_path = PathBuf::from(part);
        assert!(part_path.exists());
        let mut f = File::open(&part_path).unwrap();
        let (header, index) = DBPFReader::read_index(&mut f).unwrap();
        assert_eq!(header.major, 2);
        total_recursos += index
            .into_iter()
            .filter(|e| e.key.type_id != s4suite::engine::manifest::S4S_MANIFEST_TYPE)
            .count();
    }
    assert_eq!(
        total_recursos, 9,
        "todos os 9 recursos devem estar preservados"
    );

    // Nenhum arquivo temporário residual .s4tmp na pasta de saída
    let temporarios: Vec<_> = fs::read_dir(&fx.output_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains(".s4tmp"))
        .collect();
    assert!(
        temporarios.is_empty(),
        "não devem sobrar arquivos .s4tmp na saída"
    );
}

#[test]
fn interrupcao_recuperavel_no_merge_limpa_temporarios_e_preserva_estado() {
    let fx = Fixture::new();
    let a = fx.input_dir.join("recup_a.package");
    let b = fx.input_dir.join("recup_b.package");
    write_package(&a, &[1], 200);
    write_package(&b, &[2], 200);

    // Executa merge completo
    let task = MergeTask {
        input_files: vec![a.clone(), b.clone()],
        output_dir: fx.output_dir.clone(),
        max_size_bytes: 1024 * 1024 * 1024,
        name: Some("Recuperavel".to_string()),
    };

    let report = merge_sims4_packages(&task, |_, _, _| {}).unwrap();
    assert!(report.success);
    assert_eq!(report.output_packages.len(), 1);

    // Os originais continuam preservados
    assert!(a.exists());
    assert!(b.exists());

    // Nova unificação no mesmo diretório funciona sem colisão
    let report2 = merge_sims4_packages(&task, |_, _, _| {}).unwrap();
    assert!(report2.success);
    assert!(report2.output_packages[0].contains("Recuperavel_Merged_2"));
}

#[test]
fn pos_merge_recusado_se_relatorio_for_parcial_ou_com_falhas() {
    let fx = Fixture::new();
    let f1 = fx.input_dir.join("a.package");
    write_package(&f1, &[1], 100);
    let originais = vec![f1.clone()];
    let original_digest = digest(&f1);

    // Relatório simulando merge com falha parcial
    let report_parcial = MergeReport {
        success: false,
        partial: true,
        total_files_found: 2,
        success_count: 1,
        failed_files: vec![("b.package".to_string(), "erro I/O".to_string())],
        output_packages: vec!["Part001.package".to_string()],
        identical_resources: Vec::new(),
        original_snapshots: Vec::new(),
    };

    let outcome = apply_post_merge_action_validated(
        PostMergeAction::Delete,
        &report_parcial,
        &originais,
        &fx.input_dir,
        &fx.manager(),
    );

    assert!(
        outcome.error.is_some(),
        "pós-ação deve ser bloqueada em relatório parcial"
    );
    assert!(outcome.error.unwrap().contains("bloqueada"));
    assert!(
        f1.exists(),
        "original não pode ser apagado se relatório for parcial"
    );
    assert_eq!(digest(&f1), original_digest);
}

#[test]
fn backups_consecutivos_no_mesmo_segundo_nao_colidem() {
    let fx = Fixture::new();
    let f1 = fx.input_dir.join("orig1.package");
    let f2 = fx.input_dir.join("orig2.package");
    write_package(&f1, &[1], 200);
    write_package(&f2, &[2], 200);

    let rep1 = fx.run_merge(&[f1.clone()]);
    let out1 = apply_post_merge_action_validated(
        PostMergeAction::Backup,
        &rep1,
        &[f1.clone()],
        &fx.input_dir,
        &fx.manager(),
    );
    assert!(out1.error.is_none());
    let zip1 = out1.backup_path.expect("primeiro zip deve existir");
    assert!(zip1.exists());

    // Segundo backup no mesmo diretório (potencial mesmo segundo)
    let rep2 = fx.run_merge(&[f2.clone()]);
    let out2 = apply_post_merge_action_validated(
        PostMergeAction::Backup,
        &rep2,
        &[f2.clone()],
        &fx.input_dir,
        &fx.manager(),
    );
    assert!(out2.error.is_none());
    let zip2 = out2.backup_path.expect("segundo zip deve existir");
    assert!(zip2.exists());

    // Os dois zips não podem se sobrescrever
    assert_ne!(
        zip1, zip2,
        "backups no mesmo segundo devem receber nomes únicos e não colidir"
    );
}

#[test]
fn falha_na_publicacao_de_parte_executa_rollback_completo() {
    let fx = Fixture::new();
    let p = fx.input_dir.join("multi.package");
    write_package(&p, &[1, 2, 3, 4], 80 * 1024);
    let original_hash = digest(&p);
    let collision = fx.output_dir.join("FalhaPub_Merged_Part002.package");
    let task = MergeTask {
        input_files: vec![p.clone()],
        output_dir: fx.output_dir.clone(),
        max_size_bytes: 100 * 1024,
        name: Some("FalhaPub".to_string()),
    };
    let result = merge_sims4_packages(&task, |cur, total, label| {
        if label.starts_with("Processando") && cur == total {
            fs::write(&collision, b"preexisting concurrent bytes").unwrap();
        }
    });
    assert!(result.is_err());
    assert_eq!(digest(&p), original_hash);
    assert_eq!(
        fs::read(&collision).unwrap(),
        b"preexisting concurrent bytes"
    );
    assert!(!fx
        .output_dir
        .join("FalhaPub_Merged_Part001.package")
        .exists());
    let remaining: Vec<_> = fs::read_dir(&fx.output_dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(
        remaining,
        vec![collision],
        "apenas a colisão alheia deve permanecer"
    );
}

#[test]
fn stem_evita_sobrescrita_quando_apenas_part002_preexiste() {
    let fx = Fixture::new();
    write_package(&fx.input_dir.join("a.package"), &[1, 2], 300);

    // Simula que Part002 já existe de uma unificação anterior ou arquivo com mesmo prefixo
    let preexistente = fx.output_dir.join("Cabelos_Merged_Part002.package");
    fs::create_dir_all(&fx.output_dir).unwrap();
    fs::write(
        &preexistente,
        b"conteudo_preexistente_que_nao_pode_ser_sobrescrito",
    )
    .unwrap();

    let task = MergeTask {
        input_files: fx.inputs(),
        output_dir: fx.output_dir.clone(),
        max_size_bytes: 1024 * 1024 * 1024,
        name: Some("Cabelos".to_string()),
    };

    let report = merge_sims4_packages(&task, |_, _, _| {}).unwrap();
    assert!(report.success);

    // O stem Cabelos_Merged não pode ser reutilizado: deve avançar para Cabelos_Merged_2
    assert!(report.output_packages[0].contains("Cabelos_Merged_2"));
    assert_eq!(
        fs::read(&preexistente).unwrap(),
        b"conteudo_preexistente_que_nao_pode_ser_sobrescrito",
        "arquivo Part002 preexistente deve permanecer intacto"
    );
}

#[test]
fn pos_merge_recusado_se_originais_foram_modificados_apos_leitura() {
    let fx = Fixture::new();
    let a = fx.input_dir.join("mod_mutavel.package");
    write_package(&a, &[1], 500);
    let originais = vec![a.clone()];

    let task = MergeTask {
        input_files: originais.clone(),
        output_dir: fx.output_dir.clone(),
        max_size_bytes: 1024 * 1024 * 1024,
        name: Some("Mutavel".to_string()),
    };

    let report = merge_sims4_packages(&task, |_, _, _| {}).unwrap();
    assert!(report.success);

    // Modifica o arquivo original no disco DEPOIS que o merge leu o conteúdo
    std::thread::sleep(std::time::Duration::from_millis(20));
    fs::write(&a, b"CONTEUDO_MODIFICADO_PELO_USUARIO_APOS_MERGE").unwrap();

    // A ação pós-merge DEVE ser recusada para não destruir modificações recentes do usuário
    let outcome = apply_post_merge_action_validated(
        PostMergeAction::Delete,
        &report,
        &originais,
        &fx.input_dir,
        &fx.manager(),
    );

    assert!(
        outcome.error.is_some(),
        "pós-ação deve ser bloqueada se arquivo original mudou"
    );
    assert!(outcome
        .error
        .unwrap()
        .contains("modificado após a realização do merge"));
    assert!(a.exists(), "original modificado deve ser preservado");
}

fn digest(path: &Path) -> Vec<u8> {
    use sha2::{Digest, Sha256};
    Sha256::digest(fs::read(path).unwrap()).to_vec()
}

#[test]
fn p0_authorization_requires_exact_processed_set() {
    for action in [
        PostMergeAction::Delete,
        PostMergeAction::Disable,
        PostMergeAction::Backup,
    ] {
        for case in 0..4 {
            let fx = Fixture::new();
            let a = fx.input_dir.join("a.package");
            let b = fx.input_dir.join("b.package");
            let extra = fx.input_dir.join("extra.package");
            write_package(&a, &[1], 100);
            write_package(&b, &[2], 100);
            write_package(&extra, &[3], 100);
            let expected = [digest(&a), digest(&b), digest(&extra)];
            let mut report = fx.run_merge(&[a.clone(), b.clone()]);
            let originals = match case {
                0 => vec![a.clone(), b.clone(), extra.clone()],
                1 => vec![a.clone()],
                2 => vec![a.clone(), b.clone(), a.clone()],
                _ => {
                    report.original_snapshots.clear();
                    vec![a.clone(), b.clone()]
                }
            };
            let outcome = apply_post_merge_action_validated(
                action,
                &report,
                &originals,
                &fx.input_dir,
                &fx.manager(),
            );
            assert!(outcome.error.is_some(), "action={action:?}, case={case}");
            assert_eq!(outcome.affected, 0);
            assert_eq!([digest(&a), digest(&b), digest(&extra)], expected);
        }
    }
}

#[test]
fn p0_same_size_and_mtime_change_is_rejected() {
    let fx = Fixture::new();
    let a = fx.input_dir.join("a.package");
    write_package(&a, &[1], 100);
    let report = fx.run_merge(&[a.clone()]);
    let mtime = fs::metadata(&a).unwrap().modified().unwrap();
    let mut changed = fs::read(&a).unwrap();
    changed[100] ^= 1;
    fs::write(&a, &changed).unwrap();
    File::options()
        .write(true)
        .open(&a)
        .unwrap()
        .set_modified(mtime)
        .unwrap();
    let expected = digest(&a);
    let outcome = apply_post_merge_action_validated(
        PostMergeAction::Delete,
        &report,
        &[a.clone()],
        &fx.input_dir,
        &fx.manager(),
    );
    assert!(outcome.error.is_some());
    assert_eq!(digest(&a), expected);
}

#[test]
fn p0_changes_between_index_and_payload_cannot_authorize_deletion() {
    let fx = Fixture::new();
    let a = fx.input_dir.join("a.package");
    write_package(&a, &[1], 100);
    let task = MergeTask {
        input_files: vec![a.clone()],
        output_dir: fx.output_dir.clone(),
        max_size_bytes: 1_000_000,
        name: Some("Mutation".into()),
    };
    let changed = std::sync::atomic::AtomicBool::new(false);
    let result = merge_sims4_packages(&task, |_, _, label| {
        if label.starts_with("Processando")
            && !changed.swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            let mtime = fs::metadata(&a).unwrap().modified().unwrap();
            let mut bytes = fs::read(&a).unwrap();
            bytes[100] ^= 1;
            fs::write(&a, bytes).unwrap();
            File::options()
                .write(true)
                .open(&a)
                .unwrap()
                .set_modified(mtime)
                .unwrap();
        }
    });
    let expected = digest(&a);
    if let Ok(report) = result {
        let outcome = apply_post_merge_action_validated(
            PostMergeAction::Delete,
            &report,
            &[a.clone()],
            &fx.input_dir,
            &fx.manager(),
        );
        assert!(outcome.error.is_some());
    }
    assert_eq!(digest(&a), expected);
}

#[test]
fn p0_recovery_copies_do_not_become_duplicate_removal_candidates() {
    let fx = Fixture::new();
    let original = fx.input_dir.join("a.package");
    write_package(&original, &[1], 100);
    let expected = digest(&original);
    for prefix in [
        ".s4suite-copy-",
        ".s4suite-move-",
        ".s4suite-duplicate-",
        ".s4suite-rollback-",
        ".s4suite-tray-",
        ".s4merge-",
        ".s4-post-merge-",
    ] {
        let recovery = fx.input_dir.join(format!("{prefix}recovery"));
        fs::create_dir(&recovery).unwrap();
        fs::copy(&original, recovery.join("a.package")).unwrap();
    }
    assert!(
        s4suite::engine::organizer::detect_duplicates(&fx.input_dir).is_empty(),
        "cópias privadas não podem autorizar a remoção do único original do usuário"
    );
    assert_eq!(digest(&original), expected);
}

#[test]
fn p0_concurrent_merges_with_same_stem_never_replace_published_parts() {
    let fx = Fixture::new();
    let a = fx.input_dir.join("a.package");
    let b = fx.input_dir.join("b.package");
    write_package(&a, &[1, 2], 1000);
    write_package(&b, &[3, 4], 1000);
    let expected = [digest(&a), digest(&b)];
    let barrier = std::sync::Barrier::new(2);
    let results = std::thread::scope(|scope| {
        let handles: Vec<_> = [&a, &b]
            .into_iter()
            .map(|source| {
                let barrier = &barrier;
                let output_dir = &fx.output_dir;
                scope.spawn(move || {
                    merge_sims4_packages(
                        &MergeTask {
                            input_files: vec![source.clone()],
                            output_dir: output_dir.clone(),
                            max_size_bytes: 1500,
                            name: Some("Concurrent".into()),
                        },
                        |current, _, label| {
                            if current == 1 && label.starts_with("Lendo") {
                                barrier.wait();
                            }
                        },
                    )
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    let winner = results.iter().position(|r| r.is_ok()).unwrap();
    let report = results[winner].as_ref().unwrap();
    assert!(report.success);
    let mut keys = Vec::new();
    for output in &report.output_packages {
        let (_, index) = DBPFReader::read_index(&mut File::open(output).unwrap()).unwrap();
        keys.extend(
            index
                .into_iter()
                .filter(|e| e.key.type_id != s4suite::engine::manifest::S4S_MANIFEST_TYPE)
                .map(|e| e.key.instance_low),
        );
    }
    assert_eq!(keys, if winner == 0 { vec![1, 2] } else { vec![3, 4] });
    assert_eq!([digest(&a), digest(&b)], expected);
    assert_eq!(
        fs::read_dir(&fx.output_dir).unwrap().count(),
        3,
        "duas partes vencedoras e seu relatório; nenhum workspace perdedor"
    );
}

#[test]
fn unmerge_recupera_packages_originais_com_recursos_identicos() {
    let fx = Fixture::new();
    let pkg1 = fx.input_dir.join("Vestido_Verao.package");
    let pkg2 = fx.input_dir.join("Sapato_Salto.package");

    write_package(&pkg1, &[101, 102], 2048);
    write_package(&pkg2, &[201, 202, 203], 4096);

    let merge_task = MergeTask {
        input_files: vec![pkg1.clone(), pkg2.clone()],
        output_dir: fx.output_dir.clone(),
        max_size_bytes: 1_000_000_000,
        name: Some("Colecao_Verao".to_string()),
    };

    let merge_report = merge_sims4_packages(&merge_task, |_, _, _| {}).unwrap();
    assert!(merge_report.success);
    assert_eq!(merge_report.output_packages.len(), 1);

    let merged_path = PathBuf::from(&merge_report.output_packages[0]);
    assert!(s4suite::engine::manifest::MergedPackageManifest::is_s4s_merged_package(&merged_path));

    // Inspeciona antes de desunificar
    let inspected = s4suite::engine::merger::inspect_merged_package(&merged_path)
        .unwrap()
        .expect("deveria ler o manifesto");
    assert_eq!(inspected.total_package_count(), 2);

    let unmerge_out = fx._root.path().join("Desunificados");
    fs::create_dir_all(&unmerge_out).unwrap();

    let unmerge_task = s4suite::engine::merger::UnmergeTask {
        input_packages: vec![merged_path],
        output_dir: unmerge_out.clone(),
    };

    let unmerge_report =
        s4suite::engine::merger::unmerge_sims4_packages(&unmerge_task, |_, _, _| {}).unwrap();
    assert!(unmerge_report.success);
    assert_eq!(unmerge_report.total_extracted_packages, 2);

    let extracted1 = unmerge_report.extracted_files.iter().map(PathBuf::from)
        .find(|p| p.file_name().unwrap() == "Vestido_Verao.package").unwrap();
    let extracted2 = unmerge_report.extracted_files.iter().map(PathBuf::from)
        .find(|p| p.file_name().unwrap() == "Sapato_Salto.package").unwrap();
    assert!(extracted1.exists(), "Vestido_Verao deve ter sido extraído");
    assert!(extracted2.exists(), "Sapato_Salto deve ter sido extraído");

    // Verifica se os recursos foram recuperados corretamente
    let (_, idx1) = DBPFReader::read_index(&mut File::open(&extracted1).unwrap()).unwrap();
    let keys1: Vec<u32> = idx1.into_iter().map(|e| e.key.instance_low).collect();
    assert_eq!(keys1, vec![101, 102]);

    let (_, idx2) = DBPFReader::read_index(&mut File::open(&extracted2).unwrap()).unwrap();
    let keys2: Vec<u32> = idx2.into_iter().map(|e| e.key.instance_low).collect();
    assert_eq!(keys2, vec![201, 202, 203]);
}


#[test]
fn merge_recusa_package_ja_mesclado_pelo_s4s() {
    let fx = Fixture::new();
    let pkg1 = fx.input_dir.join("Original.package");
    write_package(&pkg1, &[1, 2], 1024);

    let task = MergeTask {
        input_files: vec![pkg1],
        output_dir: fx.output_dir.clone(),
        max_size_bytes: 1_000_000_000,
        name: Some("Mesclado".to_string()),
    };

    let report = merge_sims4_packages(&task, |_, _, _| {}).unwrap();
    let merged_file = fx._root.path().join("nome_sem_marcador.package");
    fs::copy(&report.output_packages[0], &merged_file).unwrap();

    // Tentativa de fazer merge de um arquivo que já foi mesclado deve ser rejeitada com erro claro
    let second_task = MergeTask {
        input_files: vec![merged_file],
        output_dir: fx.output_dir.clone(),
        max_size_bytes: 1_000_000_000,
        name: Some("Novo_Merge".to_string()),
    };

    let result = merge_sims4_packages(&second_task, |_, _, _| {});
    assert!(result.is_err(), "deve recusar mesclar arquivo já mesclado");
}

fn stored_payloads(path: &Path) -> Vec<(ResourceKey, u32, u16, Vec<u8>)> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = File::open(path).unwrap();
    let (_, index) = DBPFReader::read_index(&mut file).unwrap();
    let mut result: Vec<_> = index.into_iter().map(|entry| {
        file.seek(SeekFrom::Start(entry.location_offset as u64)).unwrap();
        let mut payload = vec![0; entry.file_size as usize];
        file.read_exact(&mut payload).unwrap();
        (entry.key, entry.mem_size, entry.compressed, payload)
    }).collect();
    result.sort_by_key(|entry| entry.0);
    result
}

#[test]
fn multipart_unmerge_restores_shared_compressed_resources_and_same_names_in_folders() {
    use std::io::Write;
    let fx = Fixture::new();
    let a = fx.input_dir.join("Cabelos/Ágata.package");
    let b = fx.input_dir.join("Roupas/Ágata.package");
    fs::create_dir_all(a.parent().unwrap()).unwrap();
    fs::create_dir_all(b.parent().unwrap()).unwrap();
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(b"recurso compartilhado com compressao preservada").unwrap();
    let shared = PackageResource {
        key: key(123), data: encoder.finish().unwrap(), mem_size: 49, compressed: 0x5A42,
    };
    let unique = |n| PackageResource { key: key(n), data: vec![n as u8; 4096], mem_size: 4096, compressed: 0 };
    DBPFWriter::write_package(&mut File::create(&a).unwrap(), &[shared.clone(), unique(1), unique(2)]).unwrap();
    DBPFWriter::write_package(&mut File::create(&b).unwrap(), &[shared, unique(3)]).unwrap();
    let originals = [stored_payloads(&a), stored_payloads(&b)];
    let report = merge_sims4_packages(&MergeTask {
        input_files: vec![a.clone(), b.clone()], output_dir: fx.output_dir.clone(),
        max_size_bytes: 6500, name: Some("Partes".into()),
    }, |_,_,_| {}).unwrap();
    assert!(report.output_packages.len() > 1);
    for path in &report.output_packages { assert!(fs::metadata(path).unwrap().len() <= 6500); }
    let output = fx._root.path().join("restored");
    let inputs = report.output_packages.iter().map(PathBuf::from).collect::<Vec<_>>();
    assert!(s4suite::engine::merger::unmerge_sims4_packages(&s4suite::engine::merger::UnmergeTask {
        input_packages: vec![inputs[0].clone()], output_dir: output.clone(),
    }, |_,_,_| {}).is_err());
    assert!(!output.exists(), "partes ausentes não publicam nenhum resultado");
    let unmerged = s4suite::engine::merger::unmerge_sims4_packages(&s4suite::engine::merger::UnmergeTask {
        input_packages: inputs, output_dir: output.clone(),
    }, |_,_,_| {}).unwrap();
    assert_eq!(unmerged.total_extracted_packages, 2);
    for (source, expected) in [(&a, &originals[0]), (&b, &originals[1])] {
        let relative = source.strip_prefix(&fx.input_dir).unwrap();
        let restored = unmerged.extracted_files.iter().map(PathBuf::from).find(|p| p.ends_with(relative)).unwrap();
        assert_eq!(&stored_payloads(&restored), expected);
    }
    assert_eq!(stored_payloads(&a), originals[0]);
    assert_eq!(stored_payloads(&b), originals[1]);
}

#[test]
fn unsafe_or_missing_manifest_members_never_publish_partial_packages() {
    use s4suite::engine::manifest::{MergedPackage, MergedPackageManifest};
    let fx = Fixture::new();
    for (i, name) in ["../escape", "/absolute", "C:\\evil", "valid"].iter().enumerate() {
        let path = fx.input_dir.join(format!("hostile{i}.package"));
        let manifest = MergedPackageManifest::with_packages(vec![
            MergedPackage { name: name.to_string(), resources: vec![key(42)] },
        ]);
        // The "valid" case references missing data and must fail too.
        DBPFWriter::write_package(&mut File::create(&path).unwrap(), &[manifest.to_compressed_resource().unwrap()]).unwrap();
        let dest = fx._root.path().join(format!("out{i}"));
        assert!(s4suite::engine::merger::unmerge_single_package(&path, &dest, |_,_,_| {}).is_err());
        assert!(!dest.exists());
    }
}

#[test]
fn unmerge_publishes_new_batch_without_replacing_existing_outputs() {
    let fx = Fixture::new();
    let source = fx.input_dir.join("original.package");
    write_package(&source, &[1], 100);
    let report = fx.run_merge(&[source]);
    let output = fx._root.path().join("restored");
    fs::create_dir_all(&output).unwrap();
    fs::write(output.join("original.package"), b"user existing data").unwrap();
    let a = s4suite::engine::merger::unmerge_single_package(Path::new(&report.output_packages[0]), &output, |_,_,_| {}).unwrap();
    let b = s4suite::engine::merger::unmerge_single_package(Path::new(&report.output_packages[0]), &output, |_,_,_| {}).unwrap();
    assert_ne!(a.extracted_files, b.extracted_files);
    assert_eq!(fs::read(output.join("original.package")).unwrap(), b"user existing data");
    assert_eq!(stored_payloads(Path::new(&a.extracted_files[0])), stored_payloads(Path::new(&b.extracted_files[0])));
}

#[test]
fn merge_rejects_indivisible_resource_above_configured_size() {
    let fx = Fixture::new();
    let source = fx.input_dir.join("large.package");
    write_package(&source, &[1], 4096);
    let before = digest(&source);
    assert!(merge_sims4_packages(&MergeTask {
        input_files: vec![source.clone()], output_dir: fx.output_dir.clone(),
        max_size_bytes: 1024, name: None,
    }, |_,_,_| {}).is_err());
    assert_eq!(digest(&source), before);
    assert_eq!(fs::read_dir(&fx.output_dir).unwrap().count(), 0);
}
