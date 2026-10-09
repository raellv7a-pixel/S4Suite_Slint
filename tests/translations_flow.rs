//! Testes do gerenciador de traduções: instalação a partir de arquivo avulso ou
//! compactado, atualização com backup, listagem e remoção.

use s4suite::core::safety::SafetyError;
use s4suite::engine::installer::STAGING_DIR_NAME;
use s4suite::engine::translations::{
    install_translation, list_installed, prepare_translation_removal, remove_translation,
    translations_dir, TRANSLATIONS_DIR,
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

struct Fixture {
    _root: TempDir,
    downloads: PathBuf,
    mods_dir: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = TempDir::new().unwrap();
        let downloads = root.path().join("Downloads");
        let mods_dir = root.path().join("Mods");
        fs::create_dir_all(&downloads).unwrap();
        fs::create_dir_all(&mods_dir).unwrap();
        Self { _root: root, downloads, mods_dir }
    }

    fn roots(&self) -> Vec<PathBuf> {
        vec![self.mods_dir.clone()]
    }

    fn trans_dir(&self) -> PathBuf {
        translations_dir(&self.mods_dir)
    }
}

#[test]
fn package_avulso_vai_para_a_pasta_de_traducoes() {
    let fx = Fixture::new();
    let source = fx.downloads.join("Traducao PT-BR.package");
    write_file(&source, b"conteudo-da-traducao");

    let report = install_translation(&source, &fx.mods_dir, &fx.roots()).unwrap();

    assert_eq!(report.installed, 1);
    // Um arquivo solto é a tradução inteira: nada de pasta intermediária, que
    // era o nome ilegível que a lista da aba acabava exibindo.
    let instalada = fx.trans_dir().join("Traducao PT-BR.package");
    assert!(instalada.exists(), "tradução não chegou em {}", TRANSLATIONS_DIR);
}

#[test]
fn zip_e_extraido_em_vez_de_copiado_como_zip() {
    let fx = Fixture::new();
    let source = fx.downloads.join("PackTraducoes.zip");
    make_zip(
        &source,
        &[
            ("interface_ptbr.package", b"strings-da-interface".to_vec()),
            ("dialogos_ptbr.package", b"strings-de-dialogo".to_vec()),
        ],
    );

    let report = install_translation(&source, &fx.mods_dir, &fx.roots()).unwrap();

    assert_eq!(report.installed, 2);
    // O bug que este teste tranca: o `.zip` era copiado como `.zip` para dentro
    // de Mods, onde o jogo não lê nada.
    assert!(!fx.trans_dir().join("PackTraducoes.zip").exists());
    let base = fx.trans_dir().join("PackTraducoes");
    assert!(base.join("interface_ptbr.package").exists());
    assert!(base.join("dialogos_ptbr.package").exists());
}

#[test]
fn arquivo_sem_package_util_e_recusado_com_erro() {
    let fx = Fixture::new();
    let source = fx.downloads.join("LeiaMe.zip");
    make_zip(&source, &[("leiame.txt", b"instrucoes".to_vec())]);

    let erro = install_translation(&source, &fx.mods_dir, &fx.roots()).unwrap_err();

    assert!(
        erro.to_string().contains("útil"),
        "erro deveria explicar que não há arquivo instalável: {}",
        erro
    );
}

#[test]
fn zip_com_executavel_e_bloqueado() {
    let fx = Fixture::new();
    let source = fx.downloads.join("Traducao.zip");
    make_zip(
        &source,
        &[
            ("traducao.package", b"strings".to_vec()),
            ("instalador.exe", b"MZ-binario".to_vec()),
        ],
    );

    let erro = install_translation(&source, &fx.mods_dir, &fx.roots()).unwrap_err();

    assert!(erro.to_string().contains("executável"), "erro inesperado: {}", erro);
    assert!(list_installed(&fx.mods_dir).is_empty());
}

#[test]
fn traducao_identica_nao_e_reinstalada() {
    let fx = Fixture::new();
    let source = fx.downloads.join("Traducao.package");
    write_file(&source, b"mesmo-conteudo");

    install_translation(&source, &fx.mods_dir, &fx.roots()).unwrap();
    let segunda = install_translation(&source, &fx.mods_dir, &fx.roots()).unwrap();

    assert_eq!(segunda.installed, 0);
    assert_eq!(segunda.skipped, 1);
}

#[test]
fn versao_nova_atualiza_no_lugar_e_guarda_backup() {
    let fx = Fixture::new();
    let source = fx.downloads.join("Traducao.package");
    write_file(&source, b"versao-1");
    install_translation(&source, &fx.mods_dir, &fx.roots()).unwrap();

    write_file(&source, b"versao-2-bem-maior-que-a-anterior");
    let report = install_translation(&source, &fx.mods_dir, &fx.roots()).unwrap();

    assert_eq!(report.updated, 1);
    assert_eq!(report.installed, 1);

    let instalada = fx.trans_dir().join("Traducao.package");
    assert_eq!(fs::read(&instalada).unwrap(), b"versao-2-bem-maior-que-a-anterior");

    let backups = fx.mods_dir.join(".s4suite_backups");
    let guardados: Vec<_> = fs::read_dir(&backups).unwrap().filter_map(|e| e.ok()).collect();
    assert_eq!(guardados.len(), 1, "a versão substituída deveria estar no backup");
}

/// Uma tradução guardada fora de `01_Traducoes` precisa ser reconhecida, senão
/// o usuário fica com duas cópias da mesma tradução carregando no jogo.
#[test]
fn copia_ja_instalada_fora_da_pasta_de_traducoes_e_atualizada_no_lugar() {
    let fx = Fixture::new();
    let antiga = fx.mods_dir.join("Traducoes Antigas/Traducao.package");
    write_file(&antiga, b"versao-1");

    let source = fx.downloads.join("Traducao.package");
    write_file(&source, b"versao-2-maior");
    let report = install_translation(&source, &fx.mods_dir, &fx.roots()).unwrap();

    assert_eq!(report.updated, 1);
    assert_eq!(fs::read(&antiga).unwrap(), b"versao-2-maior");
    assert!(
        !fx.trans_dir().join("traducao/Traducao.package").exists(),
        "não deveria criar uma segunda cópia em {}",
        TRANSLATIONS_DIR
    );
}

#[test]
fn staging_e_removido_no_sucesso_e_no_erro() {
    let fx = Fixture::new();
    let staging = fx.mods_dir.join(STAGING_DIR_NAME);

    let bom = fx.downloads.join("Boa.package");
    write_file(&bom, b"strings");
    install_translation(&bom, &fx.mods_dir, &fx.roots()).unwrap();
    assert!(!staging.exists(), "staging sobrou após instalação bem-sucedida");

    let ruim = fx.downloads.join("Ruim.zip");
    make_zip(&ruim, &[("leiame.txt", b"nada util".to_vec())]);
    install_translation(&ruim, &fx.mods_dir, &fx.roots()).unwrap_err();
    assert!(!staging.exists(), "staging sobrou após falha");
}

#[test]
fn listagem_soma_o_tamanho_da_pasta_e_ordena_por_nome() {
    let fx = Fixture::new();
    write_file(&fx.trans_dir().join("zulu.package"), &vec![0u8; 100]);
    write_file(&fx.trans_dir().join("Alpha/parte1.package"), &vec![0u8; 30]);
    write_file(&fx.trans_dir().join("Alpha/parte2.package"), &vec![0u8; 70]);

    let itens = list_installed(&fx.mods_dir);

    assert_eq!(itens.len(), 2, "a pasta conta como uma entrada só");
    assert_eq!(itens[0].name, "Alpha");
    assert_eq!(itens[0].size, 100, "o tamanho da pasta é a soma do conteúdo");
    assert_eq!(itens[1].name, "zulu.package");
}

#[test]
fn remocao_apaga_arquivo_e_pasta_pelo_nome_exibido() {
    let fx = Fixture::new();
    write_file(&fx.trans_dir().join("solta.package"), b"x");
    write_file(&fx.trans_dir().join("Pacote/dentro.package"), b"y");

    remove_translation(&fx.mods_dir, "solta.package", &fx.roots()).unwrap();
    remove_translation(&fx.mods_dir, "Pacote", &fx.roots()).unwrap();

    assert!(list_installed(&fx.mods_dir).is_empty());
}

/// O nome vem da UI. Concatená-lo direto no caminho deixaria `..` escapar de
/// `Mods` — a remoção resolve contra a listagem real e recusa o resto.
#[test]
fn remocao_recusa_nome_que_tenta_sair_da_pasta() {
    let fx = Fixture::new();
    let fora = fx.mods_dir.parent().unwrap().join("importante.package");
    write_file(&fora, b"nao me apague");
    fs::create_dir_all(fx.trans_dir()).unwrap();

    let erro = remove_translation(&fx.mods_dir, "../../importante.package", &fx.roots());

    assert!(erro.is_err());
    assert!(fora.exists(), "arquivo fora de Mods foi removido");
}

#[test]
fn preparar_remocao_nao_modifica_disco_e_captura_raiz_e_nome() {
    let fx = Fixture::new();
    let target = fx.trans_dir().join("solta.package");
    write_file(&target, b"strings");
    write_file(&fx.trans_dir().join("Pacote/parte.package"), b"outro");
    let before = list_installed(&fx.mods_dir);

    let pending = prepare_translation_removal(&fx.mods_dir, "solta.package").unwrap();

    assert_eq!(pending.name(), "solta.package");
    assert_eq!(pending.mods_dir(), dunce::canonicalize(&fx.mods_dir).unwrap());
    assert_eq!(list_installed(&fx.mods_dir), before);
    assert_eq!(fs::read(&target).unwrap(), b"strings");
}

#[test]
fn remocao_pendente_executa_arquivo_e_pasta_reais() {
    let fx = Fixture::new();
    write_file(&fx.trans_dir().join("solta.package"), b"x");
    write_file(&fx.trans_dir().join("Pacote/sub/parte.package"), b"y");
    let file = prepare_translation_removal(&fx.mods_dir, "solta.package").unwrap();
    let directory = prepare_translation_removal(&fx.mods_dir, "Pacote").unwrap();

    file.execute().unwrap();
    directory.execute().unwrap();

    assert!(list_installed(&fx.mods_dir).is_empty());
    assert!(fx.mods_dir.is_dir());
    assert!(fx.trans_dir().is_dir());
}

#[test]
fn remocao_pendente_recusa_substituicao_de_arquivo_do_mesmo_tamanho() {
    let fx = Fixture::new();
    let target = fx.trans_dir().join("solta.package");
    let original = fx.downloads.join("original.package");
    write_file(&target, b"original");
    let pending = prepare_translation_removal(&fx.mods_dir, "solta.package").unwrap();
    fs::rename(&target, &original).unwrap();
    write_file(&target, b"substitu");

    assert!(matches!(pending.execute(), Err(SafetyError::ChangedTarget(_))));
    assert_eq!(fs::read(&target).unwrap(), b"substitu");
    assert_eq!(fs::read(&original).unwrap(), b"original");
}

#[test]
fn remocao_pendente_recusa_substituicao_de_pasta() {
    let fx = Fixture::new();
    let target = fx.trans_dir().join("Pacote");
    let original = fx.downloads.join("Pacote-original");
    write_file(&target.join("parte.package"), b"original");
    let pending = prepare_translation_removal(&fx.mods_dir, "Pacote").unwrap();
    fs::rename(&target, &original).unwrap();
    write_file(&target.join("parte.package"), b"substituto");

    assert!(matches!(pending.execute(), Err(SafetyError::ChangedTarget(_))));
    assert_eq!(fs::read(target.join("parte.package")).unwrap(), b"substituto");
    assert_eq!(fs::read(original.join("parte.package")).unwrap(), b"original");
}

#[test]
fn remocao_pendente_recusa_arquivo_alterado_sem_substituicao() {
    let fx = Fixture::new();
    let target = fx.trans_dir().join("solta.package");
    write_file(&target, b"original");
    let pending = prepare_translation_removal(&fx.mods_dir, "solta.package").unwrap();
    fs::write(&target, b"conteudo alterado").unwrap();

    assert!(matches!(pending.execute(), Err(SafetyError::ChangedTarget(_))));
    assert_eq!(fs::read(&target).unwrap(), b"conteudo alterado");
}

#[test]
fn remocao_pendente_recusa_alvo_inexistente_e_nome_fora_do_escopo() {
    let fx = Fixture::new();
    let outside = fx.downloads.join("importante.package");
    write_file(&outside, b"preservar");
    fs::create_dir_all(fx.trans_dir()).unwrap();

    for name in ["inexistente.package", "../importante.package", "", ".", ".."] {
        assert!(matches!(
            prepare_translation_removal(&fx.mods_dir, name),
            Err(SafetyError::InvalidPath(_))
        ));
    }
    assert_eq!(fs::read(&outside).unwrap(), b"preservar");
}

#[test]
fn remocao_pendente_nao_reseleciona_outra_traducao_quando_alvo_some() {
    let fx = Fixture::new();
    let target = fx.trans_dir().join("solta.package");
    let moved = fx.trans_dir().join("outro.package");
    write_file(&target, b"original");
    let pending = prepare_translation_removal(&fx.mods_dir, "solta.package").unwrap();
    fs::rename(&target, &moved).unwrap();

    assert!(pending.execute().is_err());
    assert_eq!(fs::read(&moved).unwrap(), b"original");
}

#[test]
fn remocao_pendente_recusa_raiz_mods_substituida() {
    let fx = Fixture::new();
    let target = fx.trans_dir().join("solta.package");
    write_file(&target, b"original");
    let pending = prepare_translation_removal(&fx.mods_dir, "solta.package").unwrap();
    let original_root = fx.mods_dir.parent().unwrap().join("Mods-original");
    fs::rename(&fx.mods_dir, &original_root).unwrap();
    write_file(&target, b"substituto");

    assert!(matches!(pending.execute(), Err(SafetyError::ChangedTarget(_))));
    assert_eq!(fs::read(&target).unwrap(), b"substituto");
    assert_eq!(
        fs::read(translations_dir(&original_root).join("solta.package")).unwrap(),
        b"original"
    );
}

#[test]
fn remocao_pendente_recusa_pasta_de_traducoes_substituida() {
    let fx = Fixture::new();
    let target = fx.trans_dir().join("solta.package");
    write_file(&target, b"original");
    let pending = prepare_translation_removal(&fx.mods_dir, "solta.package").unwrap();
    let original_directory = fx.downloads.join("traducoes-originais");
    fs::rename(fx.trans_dir(), &original_directory).unwrap();
    write_file(&target, b"substituto");

    assert!(matches!(pending.execute(), Err(SafetyError::ChangedTarget(_))));
    assert_eq!(fs::read(&target).unwrap(), b"substituto");
    assert_eq!(
        fs::read(original_directory.join("solta.package")).unwrap(),
        b"original"
    );
}

#[test]
fn remocao_imediata_preserva_escopo_adicional_e_raiz_protegida() {
    let fx = Fixture::new();
    let target = fx.trans_dir().join("solta.package");
    write_file(&target, b"preservar");

    assert!(matches!(
        remove_translation(&fx.mods_dir, "solta.package", &[fx.downloads.clone()]),
        Err(SafetyError::OutsideAllowedScope(_, _))
    ));
    assert!(remove_translation(&fx.mods_dir, "solta.package", &[target.clone()]).is_err());
    assert_eq!(fs::read(&target).unwrap(), b"preservar");
}

#[cfg(unix)]
#[test]
fn preparo_recusa_links_para_arquivo_pasta_e_link_interno() {
    use std::os::unix::fs::symlink;
    let fx = Fixture::new();
    let outside = fx.downloads.join("importante.package");
    write_file(&outside, b"preservar");
    fs::create_dir_all(fx.trans_dir().join("Pacote")).unwrap();
    symlink(&outside, fx.trans_dir().join("arquivo.package")).unwrap();
    symlink(&fx.downloads, fx.trans_dir().join("pasta")).unwrap();
    symlink(&outside, fx.trans_dir().join("Pacote/parte.package")).unwrap();

    for name in ["arquivo.package", "pasta", "Pacote"] {
        assert!(prepare_translation_removal(&fx.mods_dir, name).is_err());
    }
    assert_eq!(fs::read(&outside).unwrap(), b"preservar");
    assert!(fs::symlink_metadata(fx.trans_dir().join("arquivo.package"))
        .unwrap().file_type().is_symlink());
}

#[cfg(unix)]
#[test]
fn preparo_recusa_pasta_de_traducoes_e_raiz_mods_como_links() {
    use std::os::unix::fs::symlink;
    let fx = Fixture::new();
    write_file(&fx.downloads.join("solta.package"), b"preservar");
    symlink(&fx.downloads, fx.trans_dir()).unwrap();

    assert!(prepare_translation_removal(&fx.mods_dir, "solta.package").is_err());
    assert_eq!(fs::read(fx.downloads.join("solta.package")).unwrap(), b"preservar");
    fs::remove_file(fx.trans_dir()).unwrap();

    let real_root = fx.downloads.join("Mods-reais");
    write_file(&translations_dir(&real_root).join("solta.package"), b"original");
    fs::remove_dir(&fx.mods_dir).unwrap();
    symlink(&real_root, &fx.mods_dir).unwrap();
    assert!(prepare_translation_removal(&fx.mods_dir, "solta.package").is_err());
    assert_eq!(
        fs::read(translations_dir(&real_root).join("solta.package")).unwrap(),
        b"original"
    );
}

#[cfg(unix)]
#[test]
fn execucao_recusa_alvo_substituido_por_link_sem_apagar_destino() {
    use std::os::unix::fs::symlink;
    let fx = Fixture::new();
    let target = fx.trans_dir().join("solta.package");
    let outside = fx.downloads.join("importante.package");
    write_file(&target, b"original");
    write_file(&outside, b"preservar");
    let pending = prepare_translation_removal(&fx.mods_dir, "solta.package").unwrap();
    let original = fx.downloads.join("original.package");
    fs::rename(&target, &original).unwrap();
    symlink(&outside, &target).unwrap();

    assert!(pending.execute().is_err());
    assert_eq!(fs::read(&outside).unwrap(), b"preservar");
    assert_eq!(fs::read(&original).unwrap(), b"original");
    assert!(fs::symlink_metadata(&target).unwrap().file_type().is_symlink());
}

#[cfg(unix)]
#[test]
fn execucao_recusa_pasta_de_traducoes_trocada_por_link_fora_do_escopo() {
    use std::os::unix::fs::symlink;
    let fx = Fixture::new();
    write_file(&fx.trans_dir().join("solta.package"), b"original");
    write_file(&fx.downloads.join("solta.package"), b"preservar");
    let pending = prepare_translation_removal(&fx.mods_dir, "solta.package").unwrap();
    let original = fx.mods_dir.join("Traducoes-originais");
    fs::rename(fx.trans_dir(), &original).unwrap();
    symlink(&fx.downloads, fx.trans_dir()).unwrap();

    assert!(pending.execute().is_err());
    assert_eq!(fs::read(fx.downloads.join("solta.package")).unwrap(), b"preservar");
    assert_eq!(fs::read(original.join("solta.package")).unwrap(), b"original");
    assert!(fs::symlink_metadata(fx.trans_dir()).unwrap().file_type().is_symlink());
}

#[test]
fn remocao_pendente_recusa_alteracao_em_descendente_da_pasta() {
    let fx = Fixture::new();
    let descendant = fx.trans_dir().join("Pacote/sub/parte.package");
    write_file(&descendant, b"original");
    let pending = prepare_translation_removal(&fx.mods_dir, "Pacote").unwrap();
    fs::write(&descendant, b"conteudo alterado").unwrap();

    assert!(matches!(pending.execute(), Err(SafetyError::ChangedTarget(_))));
    assert_eq!(fs::read(&descendant).unwrap(), b"conteudo alterado");
}

#[cfg(unix)]
#[test]
fn preparo_recusa_tipo_especial_sem_alterar_a_entrada() {
    use std::os::unix::net::UnixListener;
    let fx = Fixture::new();
    fs::create_dir_all(fx.trans_dir()).unwrap();
    let target = fx.trans_dir().join("socket");
    let _listener = UnixListener::bind(&target).unwrap();

    assert!(matches!(
        prepare_translation_removal(&fx.mods_dir, "socket"),
        Err(SafetyError::InvalidPath(_))
    ));
    assert!(fs::symlink_metadata(&target).is_ok());
}

#[cfg(unix)]
#[test]
fn preparo_recusa_link_de_traducoes_mesmo_dentro_de_mods() {
    use std::os::unix::fs::symlink;
    let fx = Fixture::new();
    let real_directory = fx.mods_dir.join("Traducoes-reais");
    write_file(&real_directory.join("solta.package"), b"preservar");
    symlink(&real_directory, fx.trans_dir()).unwrap();

    assert!(prepare_translation_removal(&fx.mods_dir, "solta.package").is_err());
    assert_eq!(fs::read(real_directory.join("solta.package")).unwrap(), b"preservar");
}

#[cfg(unix)]
#[test]
fn execucao_recusa_raiz_mods_trocada_por_link() {
    use std::os::unix::fs::symlink;
    let fx = Fixture::new();
    write_file(&fx.trans_dir().join("solta.package"), b"original");
    let pending = prepare_translation_removal(&fx.mods_dir, "solta.package").unwrap();
    let original_root = fx.mods_dir.parent().unwrap().join("Mods-original");
    fs::rename(&fx.mods_dir, &original_root).unwrap();
    let outside_root = fx.downloads.join("outro-jogo/Mods");
    write_file(&translations_dir(&outside_root).join("solta.package"), b"preservar");
    symlink(&outside_root, &fx.mods_dir).unwrap();

    assert!(pending.execute().is_err());
    assert_eq!(
        fs::read(translations_dir(&outside_root).join("solta.package")).unwrap(),
        b"preservar"
    );
    assert_eq!(
        fs::read(translations_dir(&original_root).join("solta.package")).unwrap(),
        b"original"
    );
}

#[test]
fn traducoes_nao_sao_despejadas_na_raiz_de_mods() {
    let fx = Fixture::new();
    let source = fx.downloads.join("Traducao.package");
    write_file(&source, b"strings");

    install_translation(&source, &fx.mods_dir, &fx.roots()).unwrap();

    let na_raiz: Vec<_> = fs::read_dir(&fx.mods_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_file())
        .collect();
    assert!(na_raiz.is_empty(), "nada deveria ficar solto na raiz de Mods");
}
