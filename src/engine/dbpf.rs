use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use std::fmt;
use std::io::{self, Read, Seek, SeekFrom, Write};

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
    #[error("Erro de I/O ao ler/escrever DBPF: {0}")]
    Io(#[from] io::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ResourceKey {
    pub type_id: u32,
    pub group_id: u32,
    pub instance_ex: u32,
    pub instance_low: u32,
}

impl fmt::Display for ResourceKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:08X}:{:08X}:{:08X}{:08X}",
            self.type_id, self.group_id, self.instance_ex, self.instance_low
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

        let mut cursor = io::Cursor::new(&buf[4..]);
        let major = cursor.read_u32::<LittleEndian>()?;
        let minor = cursor.read_u32::<LittleEndian>()?;

        reader.seek(SeekFrom::Start(36))?;
        let index_count = reader.read_u32::<LittleEndian>()?;

        reader.seek(SeekFrom::Start(64))?;
        let index_offset = reader.read_u32::<LittleEndian>()?;
        let index_size = reader.read_u32::<LittleEndian>()?;

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
        if header.index_count == 0 {
            return Ok((header, Vec::new()));
        }

        reader.seek(SeekFrom::Start(header.index_offset as u64))?;
        let idx_flags = reader.read_u32::<LittleEndian>()?;

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

        let const_instance_ex = if (idx_flags & 4) != 0 {
            Some(reader.read_u32::<LittleEndian>()?)
        } else {
            None
        };

        let mut entries = Vec::with_capacity(header.index_count as usize);

        for _ in 0..header.index_count {
            let type_id = match const_type {
                Some(val) => val,
                None => reader.read_u32::<LittleEndian>()?,
            };
            let group_id = match const_group {
                Some(val) => val,
                None => reader.read_u32::<LittleEndian>()?,
            };

            let (instance_low, instance_ex) = if let Some(ex) = const_instance_ex {
                (reader.read_u32::<LittleEndian>()?, ex)
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
            let compressed = reader.read_u16::<LittleEndian>()?;
            let _reserved = reader.read_u16::<LittleEndian>()?;

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

        Ok((header, entries))
    }
}

pub struct DBPFWriter;

impl DBPFWriter {
    pub fn write_package<W: Write + Seek>(writer: &mut W, resources: &[PackageResource]) -> Result<(), DBPFError> {
        // Write 96-byte header placeholder
        writer.seek(SeekFrom::Start(0))?;
        let header_buf = [0u8; 96];
        writer.write_all(&header_buf)?;

        let mut entries = Vec::with_capacity(resources.len());

        for res in resources {
            let offset = writer.stream_position()? as u32;
            writer.write_all(&res.data)?;

            entries.push(IndexEntry {
                key: res.key,
                location_offset: offset,
                file_size: res.data.len() as u32,
                mem_size: if res.mem_size > 0 { res.mem_size } else { res.data.len() as u32 },
                compressed: res.compressed,
            });
        }

        // TGI Sort the index
        entries.sort_by(|a, b| a.key.cmp(&b.key));

        let index_offset = writer.stream_position()? as u32;

        // Write index flags (no constants = 0)
        writer.write_u32::<LittleEndian>(0)?;

        for entry in &entries {
            writer.write_u32::<LittleEndian>(entry.key.type_id)?;
            writer.write_u32::<LittleEndian>(entry.key.group_id)?;
            writer.write_u32::<LittleEndian>(entry.key.instance_low)?;
            writer.write_u32::<LittleEndian>(entry.key.instance_ex)?;
            writer.write_u32::<LittleEndian>(entry.location_offset)?;
            writer.write_u32::<LittleEndian>(entry.file_size)?;
            writer.write_u32::<LittleEndian>(entry.mem_size)?;
            writer.write_u16::<LittleEndian>(entry.compressed)?;
            writer.write_u16::<LittleEndian>(0x4200)?; // DBPF reserved
        }

        let index_end = writer.stream_position()? as u32;
        let index_size = index_end - index_offset;

        // Rewind and write real DBPF header
        writer.seek(SeekFrom::Start(0))?;
        writer.write_all(b"DBPF")?;
        writer.write_u32::<LittleEndian>(2)?; // Major = 2
        writer.write_u32::<LittleEndian>(0)?; // Minor = 0

        // Index Count at offset 36
        writer.seek(SeekFrom::Start(36))?;
        writer.write_u32::<LittleEndian>(entries.len() as u32)?;

        // Index Offset and Index Size at offset 64
        writer.seek(SeekFrom::Start(64))?;
        writer.write_u32::<LittleEndian>(index_offset)?;
        writer.write_u32::<LittleEndian>(index_size)?;

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
        assert!(index[0].key <= index[1].key);
    }
}
