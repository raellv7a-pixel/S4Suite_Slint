//! Testes do formato DBPF — o motor mais crítico do app.
//!
//! Todo `.package` do jogo passa por aqui. Um erro de offset não dá exceção:
//! grava um arquivo que o jogo abre e lê errado, e o usuário só descobre quando
//! o conteúdo some do CAS. Por isso os testes verificam bytes, não só que a
//! função retornou `Ok`.

use s4suite::engine::dbpf::{DBPFError, DBPFReader, DBPFWriter, IndexEntry, PackageResource, ResourceKey};
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
    esperado.sort_by_key(|k| (k.type_id, k.group_id, k.instance_low, k.instance_ex));
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
    assert_eq!(k.to_string(), "0333406C:80000000:123456780000ABCD");
}

fn put_word(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn word(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

#[test]
fn writer_uses_real_dbpf_21_header_and_extended_entry_layout() {
    let bytes = write_to_buffer(&[resource(key(7, 2), b"abc")]).into_inner();
    assert_eq!(word(&bytes, 4), 2);
    assert_eq!(word(&bytes, 8), 1);
    assert_eq!(word(&bytes, 36), 1);
    assert_eq!(word(&bytes, 40), 0);
    assert_eq!(word(&bytes, 44), 36);
    assert_eq!(word(&bytes, 60), 3);
    assert_eq!(word(&bytes, 64), 99);
    assert_eq!(word(&bytes, 68), 0);
    let index = word(&bytes, 64) as usize;
    assert_eq!(word(&bytes, index + 24), 0x8000_0003);
    assert_eq!(&bytes[index + 34..index + 36], &1u16.to_le_bytes());
}

// Hand-authored, not generated by DBPFWriter: high instance word is shared by
// the index, the low word is per entry. Mirrors S4S DBPF 2.1 field positions.
fn shared_high_word_fixture() -> Vec<u8> {
    let mut bytes = vec![0u8; 96];
    bytes[..4].copy_from_slice(b"DBPF");
    for (offset, value) in [(4, 2), (8, 1), (36, 1), (44, 36), (60, 3), (64, 97)] {
        put_word(&mut bytes, offset, value);
    }
    bytes.push(0xAB);
    for value in [
        4u32, 0x01234567, 0x11223344, 0x55667788, 0x89ABCDEF,
        96, 0x80000001, 1,
    ] {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes.extend_from_slice(&0u16.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes
}

#[test]
fn real_layout_constant_instance_word_and_legacy_offset_are_readable() {
    for legacy in [false, true] {
        let mut bytes = shared_high_word_fixture();
        if legacy {
            put_word(&mut bytes, 64, 0);
            put_word(&mut bytes, 40, 97);
        }
        let (header, index) = DBPFReader::read_index(&mut Cursor::new(bytes)).unwrap();
        assert_eq!(header.index_size, 36);
        assert_eq!(index[0].key.instance_low, 0x01234567);
        assert_eq!(index[0].key.instance_ex, 0x89ABCDEF);
        assert_eq!(index[0].key.to_string(), "11223344:55667788:0123456789ABCDEF");
        assert_eq!(index[0].file_size, 1);
    }
}

#[test]
fn malformed_index_headers_and_payload_bounds_are_rejected() {
    let original = shared_high_word_fixture();
    for (offset, value) in [
        (4, 3), (8, 2), (60, 0), // unsupported DBPF/index versions
        (36, u32::MAX), (36, 2), // count cannot fit index/file
        (44, 35), (44, u32::MAX), (64, 95), (64, u32::MAX),
        (97, 8), // unsupported index flags
        (97 + 20, 0), // resource points into header
        (97 + 20, u32::MAX), // resource points beyond file
        (97 + 24, 0x80000002), // resource overlaps index
    ] {
        let mut bytes = original.clone();
        put_word(&mut bytes, offset, value);
        assert!(DBPFReader::read_index(&mut Cursor::new(bytes)).is_err(),
            "accepted malformed word at {offset}: {value}");
    }
    for end in 0..original.len() {
        assert!(DBPFReader::read_index(&mut Cursor::new(&original[..end])).is_err());
    }
}

#[test]
fn index_sorting_and_serialization_use_actual_instance_order() {
    let keys = [
        ResourceKey { type_id: 7, group_id: 0, instance_low: 2, instance_ex: 0 },
        ResourceKey { type_id: 7, group_id: 0, instance_low: 1, instance_ex: u32::MAX },
    ];
    let mut bytes = write_to_buffer(&[
        resource(keys[0], b"a"), resource(keys[1], b"b"),
    ]);
    let (_, index) = DBPFReader::read_index(&mut bytes).unwrap();
    assert_eq!(index[0].key, keys[1]);
    assert_eq!(index[1].key, keys[0]);
    let offset = word(bytes.get_ref(), 64) as usize;
    assert_eq!(word(bytes.get_ref(), offset + 12), 1);
    assert_eq!(word(bytes.get_ref(), offset + 16), u32::MAX);
}

#[test]
fn compressed_payload_and_declared_metadata_are_preserved_without_reencoding() {
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(b"payload payload payload").unwrap();
    let data = encoder.finish().unwrap();
    let input = PackageResource {
        key: key(7, 1), data: data.clone(), mem_size: 23, compressed: 0x5A42,
    };
    let mut bytes = write_to_buffer(&[input]);
    let (_, index) = DBPFReader::read_index(&mut bytes).unwrap();
    let entry = &index[0];
    assert_eq!(entry.mem_size, 23);
    assert_eq!(entry.compressed, 0x5A42);
    assert_eq!(&bytes.get_ref()[entry.location_offset as usize
        ..entry.location_offset as usize + entry.file_size as usize], &data);
}

#[test]
fn streaming_writer_keeps_callback_input_order_and_exact_metadata() {
    let entries = [
        IndexEntry {
            key: key(9, 1), location_offset: u32::MAX,
            file_size: 3, mem_size: 0, compressed: 0xFFFF,
        },
        IndexEntry {
            key: key(7, 2), location_offset: 0,
            file_size: 2, mem_size: 200, compressed: 0x5A42,
        },
    ];
    let payloads: [&[u8]; 2] = [b"abc", b"de"];
    let mut called = Vec::new();
    let mut bytes = Cursor::new(Vec::new());
    DBPFWriter::write_package_stream(&mut bytes, &entries, |index, writer| {
        called.push(index);
        writer.write_all(payloads[index])
    }).unwrap();
    assert_eq!(called, vec![0, 1]);
    let (_, index) = DBPFReader::read_index(&mut bytes).unwrap();
    for original in &entries {
        let written = index.iter().find(|entry| entry.key == original.key).unwrap();
        assert_eq!(written.mem_size, original.mem_size);
        assert_eq!(written.compressed, original.compressed);
        assert_eq!(written.file_size, original.file_size);
    }
    assert_eq!(&bytes.get_ref()[96..101], b"abcde");
}

#[test]
fn streaming_writer_rejects_short_long_and_unrepresentable_payloads() {
    let mut entry = IndexEntry {
        key: key(7, 1), location_offset: 0,
        file_size: 2, mem_size: 2, compressed: 0,
    };
    for payload in [b"a".as_slice(), b"abc".as_slice()] {
        let mut bytes = Cursor::new(Vec::new());
        assert!(DBPFWriter::write_package_stream(
            &mut bytes, &[entry.clone()], |_, writer| writer.write_all(payload),
        ).is_err());
    }
    entry.file_size = u32::MAX;
    let mut bytes = Cursor::new(Vec::new());
    assert!(DBPFWriter::write_package_stream(
        &mut bytes, &[entry], |_, _| panic!("invalid bounds must fail before callback"),
    ).is_err());
    assert!(bytes.into_inner().is_empty());
}

#[test]
fn size_flag_controls_compression_extension_without_guessing() {
    let mut bytes = shared_high_word_fixture();
    // A real unextended entry ends immediately after mem_size.
    put_word(&mut bytes, 44, 32);
    put_word(&mut bytes, 97 + 24, 1);
    bytes.truncate(bytes.len() - 4);
    let (_, index) = DBPFReader::read_index(&mut Cursor::new(bytes.clone())).unwrap();
    assert_eq!(index[0].compressed, 0);
    assert_eq!(index[0].file_size, 1);
    // Clearing the flag but retaining the extension is malformed, not a legacy mode.
    put_word(&mut bytes, 44, 36);
    bytes.extend_from_slice(&[0, 0, 1, 0]);
    assert!(DBPFReader::read_index(&mut Cursor::new(bytes)).is_err());
}
