use serde::{Deserialize, Serialize};
use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use std::fmt;
use std::io::{self, Read, Seek, SeekFrom, Write};

// Policy cap: even a two-million-entry index already needs tens of MiB of RAM.
pub const MAX_DBPF_INDEX_ENTRIES: u32 = 2_000_000;

#[derive(Debug, thiserror::Error)]
pub enum DBPFError {
    #[error("Assinatura DBPF inválida. Esperado 'DBPF'")]
    InvalidMagic,
    #[error("Versão DBPF não suportada")]
    UnsupportedVersion,
    #[error("Cabeçalho DBPF truncado ou inválido")]
    HeaderTruncated,
    #[error("Índice DBPF fora dos limites do arquivo (offset: {0})")]
    IndexOutOfBounds(u64),
    #[error("Estrutura DBPF inválida: {0}")]
    InvalidStructure(&'static str),
    #[error("Erro de I/O ao ler/escrever DBPF: {0}")]
    Io(#[from] io::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ResourceKey {
    pub type_id: u32,
    pub group_id: u32,
    /// Historical name: this is the low 32-bit word of the instance.
    pub instance_ex: u32,
    /// Historical name: this is the high 32-bit word of the instance.
    pub instance_low: u32,
}

impl fmt::Display for ResourceKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:08X}:{:08X}:{:08X}{:08X}",
            self.type_id, self.group_id, self.instance_low, self.instance_ex
        )
    }
}

#[derive(Debug, Clone)]
pub struct IndexEntry {
    pub key: ResourceKey,
    pub location_offset: u32,
    pub file_size: u32,
    pub mem_size: u32,
    pub compressed: u16,
}

#[derive(Debug, Clone)]
pub struct PackageResource {
    pub key: ResourceKey,
    pub data: Vec<u8>,
    pub mem_size: u32,
    pub compressed: u16,
}

#[derive(Debug, Clone)]
pub struct DBPFHeader {
    pub major: u32,
    pub minor: u32,
    pub index_count: u32,
    pub index_offset: u32,
    pub index_size: u32,
}

pub struct DBPFReader;

impl DBPFReader {
    pub fn read_header<R: Read + Seek>(reader: &mut R) -> Result<DBPFHeader, DBPFError> {
        let mut buf = [0u8; 96];
        reader.seek(SeekFrom::Start(0))?;
        reader.read_exact(&mut buf).map_err(|_| DBPFError::HeaderTruncated)?;

        if &buf[0..4] != b"DBPF" {
            return Err(DBPFError::InvalidMagic);
        }

        let word = |offset: usize| {
            u32::from_le_bytes(buf[offset..offset + 4].try_into().unwrap())
        };
        let major = word(4);
        let minor = word(8);
        // Accept DBPF 2.0 as well as the 2.1 layout used by Sims 4 Studio.
        if major != 2 || minor > 1 || word(60) != 3 {
            return Err(DBPFError::UnsupportedVersion);
        }
        let index_count = word(36);
        let index_offset = if word(64) != 0 { word(64) } else { word(40) };
        let index_size = word(44);

        Ok(DBPFHeader {
            major,
            minor,
            index_count,
            index_offset,
            index_size,
        })
    }

    pub fn read_index<R: Read + Seek>(reader: &mut R) -> Result<(DBPFHeader, Vec<IndexEntry>), DBPFError> {
        let header = Self::read_header(reader)?;
        if header.index_count > MAX_DBPF_INDEX_ENTRIES {
            return Err(DBPFError::InvalidStructure("index exceeds safety entry limit"));
        }
        let file_len = reader.seek(SeekFrom::End(0))?;
        let index_start = header.index_offset as u64;
        let index_end = index_start + header.index_size as u64;
        if header.index_count == 0 && header.index_size == 0 {
            return Ok((header, Vec::new()));
        }
        if index_start < 96 || index_end > file_len || header.index_size < 4 {
            return Err(DBPFError::IndexOutOfBounds(index_start));
        }
        reader.seek(SeekFrom::Start(index_start))?;
        let idx_flags = reader.read_u32::<LittleEndian>()?;
        if idx_flags & !7 != 0 {
            return Err(DBPFError::InvalidStructure("unsupported index flags"));
        }
        let constants = idx_flags.count_ones() as u64;
        let entry_size = 28 - constants * 4;
        let minimum_size = 4 + constants * 4 + header.index_count as u64 * entry_size;
        let maximum_size = minimum_size + header.index_count as u64 * 4;
        if (header.index_size as u64) < minimum_size
            || header.index_size as u64 > maximum_size
        {
            return Err(DBPFError::InvalidStructure("index count/size mismatch"));
        }

        let const_type = if (idx_flags & 1) != 0 {
            Some(reader.read_u32::<LittleEndian>()?)
        } else {
            None
        };

        let const_group = if (idx_flags & 2) != 0 {
            Some(reader.read_u32::<LittleEndian>()?)
        } else {
            None
        };

        let const_instance_high = if (idx_flags & 4) != 0 {
            Some(reader.read_u32::<LittleEndian>()?)
        } else {
            None
        };

        let mut entries = Vec::with_capacity(header.index_count as usize);

        for _ in 0..header.index_count {
            if reader.stream_position()? + entry_size > index_end {
                return Err(DBPFError::InvalidStructure("truncated index entry"));
            }
            let type_id = match const_type {
                Some(val) => val,
                None => reader.read_u32::<LittleEndian>()?,
            };
            let group_id = match const_group {
                Some(val) => val,
                None => reader.read_u32::<LittleEndian>()?,
            };

            let (instance_low, instance_ex) = if let Some(high) = const_instance_high {
                (high, reader.read_u32::<LittleEndian>()?)
            } else {
                (
                    reader.read_u32::<LittleEndian>()?,
                    reader.read_u32::<LittleEndian>()?,
                )
            };

            let location_offset = reader.read_u32::<LittleEndian>()?;
            let file_size_raw = reader.read_u32::<LittleEndian>()?;
            let file_size = file_size_raw & 0x7FFFFFFF;
            let mem_size = reader.read_u32::<LittleEndian>()?;
            // S4S ReadIndexTable (0x06008599) gates this extension on bit 31.
            let compressed = if file_size_raw & 0x8000_0000 != 0 {
                if reader.stream_position()? + 4 > index_end {
                    return Err(DBPFError::InvalidStructure("truncated compression extension"));
                }
                let compression = reader.read_u16::<LittleEndian>()?;
                let _committed = reader.read_u16::<LittleEndian>()?;
                compression
            } else {
                0
            };
            let resource_start = location_offset as u64;
            let resource_end = resource_start + file_size as u64;
            if resource_start < 96 || resource_end > file_len
                || (resource_start < index_end && resource_end > index_start)
            {
                return Err(DBPFError::InvalidStructure("resource range outside payload area"));
            }

            entries.push(IndexEntry {
                key: ResourceKey {
                    type_id,
                    group_id,
                    instance_ex,
                    instance_low,
                },
                location_offset,
                file_size,
                mem_size,
                compressed,
            });
        }
        if reader.stream_position()? != index_end {
            return Err(DBPFError::InvalidStructure("trailing bytes in index"));
        }

        Ok((header, entries))
    }
}

/// Entrega o conteúdo de cada recurso do package, já descomprimido.
///
/// Um `.package` guarda seus recursos comprimidos em zlib, então procurar texto
/// nos bytes crus do arquivo não acha nada: era por isso que a detecção de
/// dependências nunca acusava Lot51 num `Mods` real, embora funcionasse nos
/// testes, cujos fixtures são texto puro.
///
/// `limite_bytes` corta os recursos grandes — malha e textura não carregam
/// metadado de dependência, e descomprimi-los custaria segundos por mod. Um
/// recurso que não descomprime (RefPack, ou dado corrompido) é pulado sem
/// derrubar a leitura dos demais.
pub fn for_each_payload<F>(
    path: &std::path::Path,
    limite_bytes: u32,
    mut visitar: F,
) -> Result<(), DBPFError>
where
    F: FnMut(&[u8]),
{
    let file = std::fs::File::open(path)?;
    let mut reader = io::BufReader::with_capacity(256 * 1024, file);
    let (_header, index) = DBPFReader::read_index(&mut reader)?;

    for entry in &index {
        if entry.file_size == 0 || entry.file_size > limite_bytes {
            continue;
        }
        let mut bruto = vec![0u8; entry.file_size as usize];
        if reader.seek(SeekFrom::Start(entry.location_offset as u64)).is_err() {
            continue;
        }
        if reader.read_exact(&mut bruto).is_err() {
            continue;
        }

        if entry.compressed == 0 {
            visitar(&bruto);
            continue;
        }

        if entry.compressed != 0x5A42 || entry.mem_size > limite_bytes {
            continue;
        }
        let mut inflado = Vec::new();
        if flate2::read::ZlibDecoder::new(&bruto[..])
            .take(entry.mem_size as u64 + 1)
            .read_to_end(&mut inflado)
            .is_ok() && inflado.len() == entry.mem_size as usize
        {
            visitar(&inflado);
        }
    }

    Ok(())
}

pub struct DBPFWriter;

impl DBPFWriter {
    pub fn write_package<W: Write + Seek>(writer: &mut W, resources: &[PackageResource]) -> Result<(), DBPFError> {
        let entries: Vec<_> = resources.iter().map(|resource| {
            let file_size = u32::try_from(resource.data.len())
                .map_err(|_| DBPFError::InvalidStructure("resource exceeds DBPF size field"))?;
            Ok(IndexEntry {
                key: resource.key,
                location_offset: 0,
                file_size,
                mem_size: resource.mem_size,
                compressed: resource.compressed,
            })
        }).collect::<Result<_, DBPFError>>()?;
        Self::write_package_stream(writer, &entries, |index, writer| {
            writer.write_all(&resources[index].data)
        })
    }

    /// Streams payloads unchanged; callback indices refer to the input entries.
    /// Offsets are recomputed, while file size, memory size and compression are preserved.
    pub fn write_package_stream<W: Write + Seek, F>(
        writer: &mut W,
        entries: &[IndexEntry],
        mut copy_payload: F,
    ) -> Result<(), DBPFError>
    where
        F: FnMut(usize, &mut W) -> io::Result<()>,
    {
        let count = u32::try_from(entries.len())
            .map_err(|_| DBPFError::InvalidStructure("too many resources"))?;
        if count > MAX_DBPF_INDEX_ENTRIES {
            return Err(DBPFError::InvalidStructure("index exceeds safety entry limit"));
        }
        let index_size = 4u64 + entries.len() as u64 * 32;
        let mut payload_end = 96u64;
        for entry in entries {
            if entry.file_size > 0x7FFF_FFFF {
                return Err(DBPFError::InvalidStructure("resource exceeds 31-bit stored size"));
            }
            payload_end = payload_end.checked_add(entry.file_size as u64)
                .ok_or(DBPFError::InvalidStructure("package size overflow"))?;
        }
        if payload_end + index_size > u32::MAX as u64 {
            return Err(DBPFError::InvalidStructure("package exceeds 32-bit file offsets"));
        }
        writer.seek(SeekFrom::Start(0))?;
        writer.write_all(&[0u8; 96])?;
        let mut output_entries = Vec::with_capacity(entries.len());
        for (index, entry) in entries.iter().enumerate() {
            let start = writer.stream_position()?;
            copy_payload(index, writer)?;
            if writer.stream_position()? != start + entry.file_size as u64 {
                return Err(DBPFError::InvalidStructure("streamed payload size mismatch"));
            }
            let mut output_entry = entry.clone();
            output_entry.location_offset = start as u32;
            output_entries.push(output_entry);
        }
        // Actual TGI order, accounting for the historical reversed word names.
        output_entries.sort_by_key(|entry| (
            entry.key.type_id, entry.key.group_id,
            entry.key.instance_low, entry.key.instance_ex,
        ));
        let index_offset = writer.stream_position()? as u32;
        writer.write_u32::<LittleEndian>(0)?;
        for entry in &output_entries {
            writer.write_u32::<LittleEndian>(entry.key.type_id)?;
            writer.write_u32::<LittleEndian>(entry.key.group_id)?;
            writer.write_u32::<LittleEndian>(entry.key.instance_low)?;
            writer.write_u32::<LittleEndian>(entry.key.instance_ex)?;
            writer.write_u32::<LittleEndian>(entry.location_offset)?;
            writer.write_u32::<LittleEndian>(entry.file_size | 0x8000_0000)?;
            writer.write_u32::<LittleEndian>(entry.mem_size)?;
            writer.write_u16::<LittleEndian>(entry.compressed)?;
            writer.write_u16::<LittleEndian>(1)?; // committed resource
        }
        writer.seek(SeekFrom::Start(0))?;
        writer.write_all(b"DBPF")?;
        writer.write_u32::<LittleEndian>(2)?;
        writer.write_u32::<LittleEndian>(1)?;
        writer.seek(SeekFrom::Start(36))?;
        writer.write_u32::<LittleEndian>(count)?;
        writer.seek(SeekFrom::Start(44))?;
        writer.write_u32::<LittleEndian>(index_size as u32)?;
        writer.seek(SeekFrom::Start(60))?;
        writer.write_u32::<LittleEndian>(3)?;
        writer.write_u32::<LittleEndian>(index_offset)?;
        writer.flush()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dbpf_read_write() {
        let mut cursor = io::Cursor::new(Vec::new());

        let res1 = PackageResource {
            key: ResourceKey {
                type_id: 0x54524159,
                group_id: 0x00000000,
                instance_ex: 0x11223344,
                instance_low: 0x55667788,
            },
            data: b"Sims 4 Test Package Resource Payload 1".to_vec(),
            mem_size: 38,
            compressed: 0,
        };

        let res2 = PackageResource {
            key: ResourceKey {
                type_id: 0x34567890,
                group_id: 0x00000000,
                instance_ex: 0x00000001,
                instance_low: 0x00000002,
            },
            data: b"Payload 2 Content".to_vec(),
            mem_size: 17,
            compressed: 0,
        };

        DBPFWriter::write_package(&mut cursor, &[res1.clone(), res2.clone()]).expect("Write failed");

        cursor.seek(SeekFrom::Start(0)).unwrap();
        let (header, index) = DBPFReader::read_index(&mut cursor).expect("Read failed");

        assert_eq!(header.major, 2);
        assert_eq!(header.index_count, 2);
        assert_eq!(index.len(), 2);
        // TGI sorted order
        assert!(index[0].key.type_id <= index[1].key.type_id);
    }
}
