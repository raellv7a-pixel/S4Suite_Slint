//! Testes do formato DBPF — o motor mais crítico do app.
//!
//! Todo `.package` do jogo passa por aqui. Um erro de offset não dá exceção:
//! grava um arquivo que o jogo abre e lê errado, e o usuário só descobre quando
//! o conteúdo some do CAS. Por isso os testes verificam bytes, não só que a
//! função retornou `Ok`.

use s4suite::engine::dbpf::{DBPFError, DBPFReader, DBPFWriter, PackageResource, ResourceKey};
use std::io::{Cursor, Seek, SeekFrom, Write};

fn key(type_id: u32, instance: u32) -> ResourceKey {
    ResourceKey { type_id, group_id: 0, instance_ex: 0, instance_low: instance }
}

fn resource(k: ResourceKey, data: &[u8]) -> PackageResource {
    PackageResource {
        key: k,
        data: data.to_vec(),
        mem_size: data.len() as u32,
        compressed: 0,
    }
}

fn write_to_buffer(resources: &[PackageResource]) -> Cursor<Vec<u8>> {
    let mut buf = Cursor::new(Vec::new());
    DBPFWriter::write_package(&mut buf, resources).unwrap();
    buf.seek(SeekFrom::Start(0)).unwrap();
    buf
}

#[test]
fn package_escrito_comeca_com_a_assinatura_dbpf() {
    let buf = write_to_buffer(&[resource(key(0x0333406C, 1), b"conteudo")]);
    assert_eq!(&buf.get_ref()[0..4], b"DBPF");
}

#[test]
fn cada_recurso_volta_com_a_mesma_chave_e_o_mesmo_tamanho() {
    let recursos = vec![
        resource(key(0x0333406C, 1), b"primeiro"),
        resource(key(0x034AEECB, 2), b"segundo bem maior que o primeiro"),
        resource(key(0x0333406C, 3), b"terceiro"),
    ];

    let mut buf = write_to_buffer(&recursos);
    let (header, index) = DBPFReader::read_index(&mut buf).unwrap();

    assert_eq!(header.index_count, 3);
    assert_eq!(index.len(), 3);

    // A busca é por chave porque o índice sai ordenado por TGI, não na ordem
    // de escrita — ver `indice_sai_ordenado_por_tgi`.
    for escrito in &recursos {
        let lido = index
            .iter()
            .find(|e| e.key == escrito.key)
            .unwrap_or_else(|| panic!("recurso {} sumiu do índice", escrito.key));
        assert_eq!(lido.file_size as usize, escrito.data.len());
        assert_eq!(lido.mem_size, escrito.mem_size);
    }
}

/// O índice sai ordenado por TGI. Não é detalhe de implementação: é o que
/// permite ao jogo achar um recurso por busca binária em vez de varrer o
/// arquivo inteiro, e é a razão de o merger existir.
#[test]
fn indice_sai_ordenado_por_tgi() {
    let recursos = vec![
        resource(key(0x034AEECB, 9), b"c"),
        resource(key(0x0333406C, 5), b"a"),
        resource(key(0x0333406C, 1), b"b"),
    ];

    let mut buf = write_to_buffer(&recursos);
    let (_, index) = DBPFReader::read_index(&mut buf).unwrap();

    let chaves: Vec<_> = index.iter().map(|e| e.key).collect();
    let mut esperado = chaves.clone();
    esperado.sort();
    assert_eq!(chaves, esperado, "o índice não saiu ordenado por TGI");

    // O primeiro é o de menor type_id; a instância desempata dentro do tipo.
    assert_eq!(chaves[0], key(0x0333406C, 1));
    assert_eq!(chaves[1], key(0x0333406C, 5));
    assert_eq!(chaves[2], key(0x034AEECB, 9));
}

/// O offset gravado no índice tem que apontar para o byte exato do recurso.
/// Errar aqui não gera erro nenhum: o jogo lê lixo no lugar do conteúdo.
#[test]
fn offset_do_indice_aponta_para_os_bytes_certos() {
    let recursos = vec![
        resource(key(0x0333406C, 1), b"AAAA"),
        resource(key(0x0333406C, 2), b"BBBBBBBB"),
        resource(key(0x0333406C, 3), b"CC"),
    ];

    let mut buf = write_to_buffer(&recursos);
    let (_, index) = DBPFReader::read_index(&mut buf).unwrap();
    let bytes = buf.into_inner();

    for (escrito, lido) in recursos.iter().zip(index.iter()) {
        let inicio = lido.location_offset as usize;
        let fim = inicio + lido.file_size as usize;
        assert_eq!(
            &bytes[inicio..fim],
            escrito.data.as_slice(),
            "o offset do recurso {} aponta para outro lugar",
            escrito.key
        );
    }
}

#[test]
fn package_sem_recursos_e_valido_e_volta_vazio() {
    let mut buf = write_to_buffer(&[]);
    let (header, index) = DBPFReader::read_index(&mut buf).unwrap();

    assert_eq!(header.index_count, 0);
    assert!(index.is_empty());
}

#[test]
fn arquivo_sem_assinatura_e_recusado_em_vez_de_lido_como_lixo() {
    let mut buf = Cursor::new(b"NAO SOU UM PACKAGE, SOU UM TEXTO QUALQUER".repeat(4).to_vec());
    let erro = DBPFReader::read_index(&mut buf).unwrap_err();
    assert!(matches!(erro, DBPFError::InvalidMagic), "erro inesperado: {}", erro);
}

#[test]
fn cabecalho_truncado_e_recusado() {
    // Menos que os 96 bytes do cabeçalho.
    let mut buf = Cursor::new(b"DBPF".to_vec());
    let erro = DBPFReader::read_index(&mut buf).unwrap_err();
    assert!(matches!(erro, DBPFError::HeaderTruncated), "erro inesperado: {}", erro);
}

/// Um package cujo índice aponta para fora do arquivo é lixo: ler ali devolveria
/// bytes de outro lugar da memória ou estouraria o buffer.
#[test]
fn indice_apontando_para_fora_do_arquivo_e_recusado() {
    let mut buf = write_to_buffer(&[resource(key(0x0333406C, 1), b"conteudo")]);

    // Reescreve o offset do índice para muito além do fim.
    buf.seek(SeekFrom::Start(64)).unwrap();
    buf.write_all(&u32::MAX.to_le_bytes()).unwrap();
    buf.seek(SeekFrom::Start(0)).unwrap();

    let erro = DBPFReader::read_index(&mut buf).unwrap_err();
    assert!(
        matches!(erro, DBPFError::IndexOutOfBounds(_)),
        "erro inesperado: {}",
        erro
    );
}

/// Recursos de tipos diferentes não podem se misturar: o jogo procura por
/// TGI, e trocar o type_id de um recurso o torna invisível.
#[test]
fn tipos_diferentes_nao_se_misturam_no_indice() {
    let recursos = vec![
        resource(key(0x0333406C, 1), b"tuning"),
        resource(key(0x00B2D882, 1), b"imagem"),
        resource(key(0x034AEECB, 1), b"outro"),
    ];

    let mut buf = write_to_buffer(&recursos);
    let (_, index) = DBPFReader::read_index(&mut buf).unwrap();

    let tipos: Vec<u32> = index.iter().map(|e| e.key.type_id).collect();
    assert!(tipos.contains(&0x0333406C));
    assert!(tipos.contains(&0x00B2D882));
    assert!(tipos.contains(&0x034AEECB));
    // Mesma instância, tipos diferentes: são três recursos, não um.
    assert_eq!(index.len(), 3);
}

/// Um recurso grande exercita o caminho em que offset e tamanho passam do que
/// cabe num campo pequeno; um package real tem dezenas de MB.
#[test]
fn recurso_grande_sobrevive_ao_ciclo_de_escrita_e_leitura() {
    let grande = vec![0xABu8; 2 * 1024 * 1024];
    let recursos = vec![
        resource(key(0x0333406C, 1), b"pequeno antes"),
        resource(key(0x0333406C, 2), &grande),
        resource(key(0x0333406C, 3), b"pequeno depois"),
    ];

    let mut buf = write_to_buffer(&recursos);
    let (_, index) = DBPFReader::read_index(&mut buf).unwrap();
    let bytes = buf.into_inner();

    let entrada = &index[1];
    let inicio = entrada.location_offset as usize;
    let fim = inicio + entrada.file_size as usize;
    assert_eq!(entrada.file_size as usize, grande.len());
    assert_eq!(&bytes[inicio..fim], grande.as_slice());
}

#[test]
fn chave_de_recurso_e_exibida_no_formato_tgi() {
    let k = ResourceKey {
        type_id: 0x0333406C,
        group_id: 0x80000000,
        instance_ex: 0x0000ABCD,
        instance_low: 0x12345678,
    };
    assert_eq!(k.to_string(), "0333406C:80000000:0000ABCD12345678");
}
