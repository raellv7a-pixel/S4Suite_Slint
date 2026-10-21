use crate::engine::dbpf::{DBPFReader, PackageResource, ResourceKey};
use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use flate2::{Decompress, FlushDecompress, Status};
use flate2::write::ZlibEncoder;
use flate2::Compression;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::path::Path;

/// Resource Type exato utilizado pelo Sims 4 Studio para o manifesto de pacotes mesclados:
/// FNV-32("S4SMergedPackageManifest") = 0x7FB6AD8A
pub const S4S_MANIFEST_TYPE: u32 = 0x7FB6AD8A;
pub const S4S_MANIFEST_GROUP: u32 = 0x00000000;
pub const S4S_MANIFEST_INSTANCE_EX: u32 = 0x00000000;
pub const S4S_MANIFEST_INSTANCE_LOW: u32 = 0x00000000;

pub const S4S_MANIFEST_KEY: ResourceKey = ResourceKey {
    type_id: S4S_MANIFEST_TYPE,
    group_id: S4S_MANIFEST_GROUP,
    instance_ex: S4S_MANIFEST_INSTANCE_EX,
    instance_low: S4S_MANIFEST_INSTANCE_LOW,
};

/// Versão padrão do manifesto do Sims 4 Studio.
pub const S4S_MANIFEST_VERSION_1: u32 = 1;

// Policy limits, not format limits: a manifest is metadata, never mesh/texture data.
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_MANIFEST_STRING_BYTES: usize = 64 * 1024;
pub const MAX_MANIFEST_DEPTH: usize = 64;
pub const MAX_MANIFEST_FOLDERS: usize = 65_536;
pub const MAX_MANIFEST_PACKAGES: usize = 262_144;
pub const MAX_MANIFEST_RESOURCES: usize = 2_000_000;

#[derive(Default)]
struct ManifestBudget {
    folders: usize,
    packages: usize,
    resources: usize,
    expanded_paths: usize,
}

fn invalid_manifest(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn charge(total: &mut usize, count: usize, limit: usize) -> io::Result<()> {
    *total = total.checked_add(count)
        .filter(|n| *n <= limit)
        .ok_or_else(|| invalid_manifest("Manifest count exceeds safety limit"))?;
    Ok(())
}

fn folder_path_len(parent: usize, name: &str) -> usize {
    parent + name.len() + usize::from(parent != 0 && !name.is_empty())
}

fn charge_paths(budget: &mut ManifestBudget, length: usize, copies: usize) -> io::Result<()> {
    let bytes = length.checked_mul(copies)
        .ok_or_else(|| invalid_manifest("Manifest paths exceed safety limit"))?;
    charge(&mut budget.expanded_paths, bytes, MAX_MANIFEST_BYTES)
}

/// Estrutura de manifesto embutida em packages mesclados pelo Sims 4 Studio e S4Suite.
/// Reconstrói nomes e recursos ainda presentes; não restaura bytes originais do arquivo
/// nem recursos sobrescritos por colisões durante o merge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergedPackageManifest {
    pub version: u32,
    pub root: MergedFolder,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MergedFolder {
    pub name: String,
    pub folders: Vec<MergedFolder>,
    pub packages: Vec<MergedPackage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergedPackage {
    /// Nome do package original (sem a extensão `.package`).
    pub name: String,
    /// Chaves de recursos (TGIs) pertencentes a este package.
    pub resources: Vec<ResourceKey>,
}

impl MergedPackageManifest {
    pub fn new() -> Self {
        Self {
            version: S4S_MANIFEST_VERSION_1,
            root: MergedFolder::default(),
        }
    }

    pub fn with_packages(packages: Vec<MergedPackage>) -> Self {
        Self {
            version: S4S_MANIFEST_VERSION_1,
            root: MergedFolder {
                name: String::new(),
                folders: Vec::new(),
                packages,
            },
        }
    }

    /// Serializa o manifesto no formato binário oficial do Sims 4 Studio:
    /// - Version: u32 (little endian)
    /// - MergedFolder (Root):
    ///   - Name: u32 len + UTF-8 bytes
    ///   - Folders Count: u32
    ///     - Para cada subpasta: MergedFolder recursivo
    ///   - Packages Count: u32
    ///     - Para cada package:
    ///       - Name: u32 len + UTF-8 bytes
    ///       - Resources Count: u32
    ///       - Para cada recurso:
    ///         - Instance (u64 LE): high word then low word in the DBPF key.
    ///         - Type (u32 LE)
    ///         - Group (u32 LE)
    pub fn write<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        self.validate()?;
        writer.write_u32::<LittleEndian>(self.version)?;
        Self::write_folder(&self.root, writer)?;
        Ok(())
    }

    fn write_folder<W: Write>(folder: &MergedFolder, writer: &mut W) -> io::Result<()> {
        Self::write_string(&folder.name, writer)?;

        writer.write_u32::<LittleEndian>(folder.folders.len() as u32)?;
        for sub in &folder.folders {
            Self::write_folder(sub, writer)?;
        }

        writer.write_u32::<LittleEndian>(folder.packages.len() as u32)?;
        for pkg in &folder.packages {
            Self::write_string(&pkg.name, writer)?;
            writer.write_u32::<LittleEndian>(pkg.resources.len() as u32)?;
            for res in &pkg.resources {
                // Historical API names are reversed: instance_low is the high word.
                let inst = ((res.instance_low as u64) << 32) | (res.instance_ex as u64);
                writer.write_u64::<LittleEndian>(inst)?;
                writer.write_u32::<LittleEndian>(res.type_id)?;
                writer.write_u32::<LittleEndian>(res.group_id)?;
            }
        }

        Ok(())
    }

    fn write_string<W: Write>(s: &str, writer: &mut W) -> io::Result<()> {
        let bytes = s.as_bytes();
        writer.write_u32::<LittleEndian>(bytes.len() as u32)?;
        writer.write_all(bytes)?;
        Ok(())
    }

    /// Reads one complete manifest; extra bytes and unsupported versions are errors.
    pub fn read<R: Read>(reader: &mut R) -> io::Result<Self> {
        let mut bytes = Vec::new();
        reader.take(MAX_MANIFEST_BYTES as u64 + 1).read_to_end(&mut bytes)?;
        Self::from_bytes(&bytes)
    }

    fn read_count(
        reader: &mut io::Cursor<&[u8]>,
        minimum_record_bytes: usize,
    ) -> io::Result<usize> {
        let count = reader.read_u32::<LittleEndian>()? as usize;
        let remaining = reader.get_ref().len() - reader.position() as usize;
        if count > remaining / minimum_record_bytes {
            return Err(invalid_manifest("Manifest count exceeds remaining bytes"));
        }
        Ok(count)
    }

    fn read_folder(
        reader: &mut io::Cursor<&[u8]>,
        depth: usize,
        parent_path_len: usize,
        budget: &mut ManifestBudget,
    ) -> io::Result<MergedFolder> {
        if depth > MAX_MANIFEST_DEPTH {
            return Err(invalid_manifest("Manifest folder nesting exceeds safety limit"));
        }
        charge(&mut budget.folders, 1, MAX_MANIFEST_FOLDERS)?;
        let name = Self::read_string(reader)?;
        let path_len = folder_path_len(parent_path_len, &name);
        charge_paths(budget, path_len, 1)?;
        let folder_count = Self::read_count(reader, 12)?;
        if folder_count > MAX_MANIFEST_FOLDERS - budget.folders {
            return Err(invalid_manifest("Manifest folder count exceeds safety limit"));
        }
        let mut folders = Vec::new();
        for _ in 0..folder_count {
            folders.push(Self::read_folder(reader, depth + 1, path_len, budget)?);
        }
        let package_count = Self::read_count(reader, 8)?;
        charge(&mut budget.packages, package_count, MAX_MANIFEST_PACKAGES)?;
        charge_paths(budget, path_len, package_count)?;
        let mut packages = Vec::new();
        for _ in 0..package_count {
            let name = Self::read_string(reader)?;
            let count = Self::read_count(reader, 16)?;
            charge(&mut budget.resources, count, MAX_MANIFEST_RESOURCES)?;
            let mut resources = Vec::with_capacity(count);
            for _ in 0..count {
                let instance = reader.read_u64::<LittleEndian>()?;
                resources.push(ResourceKey {
                    type_id: reader.read_u32::<LittleEndian>()?,
                    group_id: reader.read_u32::<LittleEndian>()?,
                    // Keep the historical DBPF API's reversed word names.
                    instance_low: (instance >> 32) as u32,
                    instance_ex: instance as u32,
                });
            }
            packages.push(MergedPackage { name, resources });
        }
        Ok(MergedFolder { name, folders, packages })
    }

    fn read_string(reader: &mut io::Cursor<&[u8]>) -> io::Result<String> {
        let len = reader.read_u32::<LittleEndian>()? as usize;
        let start = reader.position() as usize;
        if len > MAX_MANIFEST_STRING_BYTES || len > reader.get_ref().len() - start {
            return Err(invalid_manifest("Manifest string exceeds safety limit or remaining bytes"));
        }
        let value = std::str::from_utf8(&reader.get_ref()[start..start + len])
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
            .to_owned();
        reader.set_position((start + len) as u64);
        Ok(value)
    }

    fn validate(&self) -> io::Result<()> {
        if self.version != S4S_MANIFEST_VERSION_1 {
            return Err(invalid_manifest("Unsupported merged manifest version"));
        }
        fn string_size(value: &str) -> io::Result<usize> {
            if value.len() > MAX_MANIFEST_STRING_BYTES {
                return Err(invalid_manifest("Manifest string exceeds safety limit"));
            }
            Ok(4 + value.len())
        }
        fn folder_size(
            folder: &MergedFolder,
            depth: usize,
            parent_path_len: usize,
            budget: &mut ManifestBudget,
        ) -> io::Result<usize> {
            if depth > MAX_MANIFEST_DEPTH {
                return Err(invalid_manifest("Manifest folder nesting exceeds safety limit"));
            }
            charge(&mut budget.folders, 1, MAX_MANIFEST_FOLDERS)?;
            charge(&mut budget.packages, folder.packages.len(), MAX_MANIFEST_PACKAGES)?;
            let mut size = string_size(&folder.name)? + 8;
            let path_len = folder_path_len(parent_path_len, &folder.name);
            charge_paths(budget, path_len, folder.packages.len() + 1)?;
            for sub in &folder.folders {
                size += folder_size(sub, depth + 1, path_len, budget)?;
                if size > MAX_MANIFEST_BYTES {
                    return Err(invalid_manifest("Manifest exceeds safety limit"));
                }
            }
            for package in &folder.packages {
                charge(&mut budget.resources, package.resources.len(), MAX_MANIFEST_RESOURCES)?;
                size += string_size(&package.name)? + 4 + package.resources.len() * 16;
                if size > MAX_MANIFEST_BYTES {
                    return Err(invalid_manifest("Manifest exceeds safety limit"));
                }
            }
            Ok(size)
        }
        if folder_size(&self.root, 0, 0, &mut ManifestBudget::default())? + 4 > MAX_MANIFEST_BYTES {
            return Err(invalid_manifest("Manifest exceeds safety limit"));
        }
        Ok(())
    }

    pub fn to_bytes(&self) -> io::Result<Vec<u8>> {
        let mut buf = Vec::new();
        self.write(&mut buf)?;
        Ok(buf)
    }

    pub fn from_bytes(bytes: &[u8]) -> io::Result<Self> {
        if bytes.len() > MAX_MANIFEST_BYTES {
            return Err(invalid_manifest("Manifest exceeds safety limit"));
        }
        let mut cursor = io::Cursor::new(bytes);
        let version = cursor.read_u32::<LittleEndian>()?;
        if version != S4S_MANIFEST_VERSION_1 {
            return Err(invalid_manifest("Unsupported merged manifest version"));
        }
        let root = Self::read_folder(&mut cursor, 0, 0, &mut ManifestBudget::default())?;
        if cursor.position() != bytes.len() as u64 {
            return Err(invalid_manifest("Trailing bytes after merged manifest"));
        }
        Ok(Self { version, root })
    }

    /// Codifica e comprime o manifesto em um `PackageResource` pronto para ser gravado
    /// no DBPF com compressão zlib (0x5A42).
    pub fn to_compressed_resource(&self) -> io::Result<PackageResource> {
        let uncompressed = self.to_bytes()?;
        let mem_size = uncompressed.len() as u32;

        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&uncompressed)?;
        let compressed_data = encoder.finish()?;

        Ok(PackageResource {
            key: S4S_MANIFEST_KEY,
            data: compressed_data,
            mem_size,
            compressed: 0x5A42,
        })
    }

    /// Descomprime e decodifica o manifesto a partir dos dados do recurso DBPF.
    pub fn from_resource_data(
        data: &[u8],
        compressed: u16,
        mem_size: u32,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        if data.len() > MAX_MANIFEST_BYTES || mem_size as usize > MAX_MANIFEST_BYTES {
            return Err(invalid_manifest("Manifest resource exceeds safety limit").into());
        }
        match compressed {
            0 => {
                if data.len() != mem_size as usize {
                    return Err(invalid_manifest("Manifest declared size mismatch").into());
                }
                Ok(Self::from_bytes(data)?)
            }
            0x5A42 => {
                // One extra output byte detects a dishonest size without inflating a bomb.
                let mut bytes = vec![0u8; mem_size as usize + 1];
                let mut decoder = Decompress::new(true);
                let status = decoder.decompress(data, &mut bytes, FlushDecompress::Finish)?;
                if status != Status::StreamEnd || decoder.total_out() != mem_size as u64 {
                    return Err(invalid_manifest("Manifest declared size mismatch or incomplete zlib stream").into());
                }
                if decoder.total_in() != data.len() as u64 {
                    return Err(invalid_manifest("Trailing compressed manifest bytes").into());
                }
                bytes.truncate(mem_size as usize);
                Ok(Self::from_bytes(&bytes)?)
            }
            _ => Err(invalid_manifest("Unsupported manifest compression type").into()),
        }
    }

    /// Retorna todos os packages do manifesto de forma plana, preservando os caminhos relativos
    /// das pastas onde se encontravam.
    pub fn flatten_packages(&self) -> Vec<(String, &MergedPackage)> {
        let mut result = Vec::new();
        Self::collect_packages(&self.root, "", &mut result);
        result
    }

    fn collect_packages<'a>(
        folder: &'a MergedFolder,
        parent_path: &str,
        acc: &mut Vec<(String, &'a MergedPackage)>,
    ) {
        let current_path = if folder.name.is_empty() {
            parent_path.to_string()
        } else if parent_path.is_empty() {
            folder.name.clone()
        } else {
            format!("{}/{}", parent_path, folder.name)
        };

        for pkg in &folder.packages {
            acc.push((current_path.clone(), pkg));
        }

        for sub in &folder.folders {
            Self::collect_packages(sub, &current_path, acc);
        }
    }

    pub fn total_package_count(&self) -> usize {
        self.flatten_packages().len()
    }

    pub fn total_resource_count(&self) -> usize {
        self.flatten_packages()
            .iter()
            .map(|(_, p)| p.resources.len())
            .sum()
    }

    /// Lê e decodifica o manifesto do Sims 4 Studio de um leitor DBPF (Read + Seek).
    /// Retorna `Ok(None)` se o pacote for um DBPF comum sem manifesto S4S.
    pub fn read_from_reader<R: Read + Seek>(
        reader: &mut R,
    ) -> Result<Option<Self>, Box<dyn std::error::Error + Send + Sync>> {
        let (_header, index) = DBPFReader::read_index(reader)?;
        let Some(entry) = index.iter().find(|e| {
            e.key.type_id == S4S_MANIFEST_TYPE
                && e.key.group_id == S4S_MANIFEST_GROUP
                && e.key.instance_ex == S4S_MANIFEST_INSTANCE_EX
                && e.key.instance_low == S4S_MANIFEST_INSTANCE_LOW
        }) else {
            return Ok(None);
        };

        if entry.file_size as usize > MAX_MANIFEST_BYTES
            || entry.mem_size as usize > MAX_MANIFEST_BYTES
        {
            return Err(invalid_manifest("Manifest resource exceeds safety limit").into());
        }
        reader.seek(SeekFrom::Start(entry.location_offset as u64))?;
        let mut raw = vec![0u8; entry.file_size as usize];
        reader.read_exact(&mut raw)?;

        let manifest = Self::from_resource_data(&raw, entry.compressed, entry.mem_size)?;
        Ok(Some(manifest))
    }

    /// Abre um arquivo `.package`, verifica se possui o manifesto do Sims 4 Studio e o decodifica.
    pub fn read_from_package_file(
        path: &Path,
    ) -> Result<Option<Self>, Box<dyn std::error::Error + Send + Sync>> {
        let file = File::open(path)?;
        let mut reader = BufReader::with_capacity(128 * 1024, file);
        Self::read_from_reader(&mut reader)
    }

    /// Verifica se um arquivo `.package` possui o manifesto do Sims 4 Studio
    /// sem precisar descomprimir o payload inteiro do manifesto.
    pub fn is_s4s_merged_package(path: &Path) -> bool {
        let Ok(file) = File::open(path) else {
            return false;
        };
        let mut reader = BufReader::with_capacity(64 * 1024, file);
        let Ok((_header, index)) = DBPFReader::read_index(&mut reader) else {
            return false;
        };
        index.iter().any(|e| {
            e.key.type_id == S4S_MANIFEST_TYPE
                && e.key.group_id == S4S_MANIFEST_GROUP
                && e.key.instance_ex == S4S_MANIFEST_INSTANCE_EX
                && e.key.instance_low == S4S_MANIFEST_INSTANCE_LOW
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_manifest_roundtrip() {
        let manifest = MergedPackageManifest::with_packages(vec![
            MergedPackage {
                name: "Dress_Blue".to_string(),
                resources: vec![
                    ResourceKey {
                        type_id: 0x0333406C,
                        group_id: 0x80000000,
                        instance_ex: 0x12345678,
                        instance_low: 0x9ABCDEF0,
                    },
                    ResourceKey {
                        type_id: 0x319E4F1D,
                        group_id: 0x00000000,
                        instance_ex: 0x00000000,
                        instance_low: 0x11223344,
                    },
                ],
            },
            MergedPackage {
                name: "Shoes_Heels".to_string(),
                resources: vec![ResourceKey {
                    type_id: 0x0333406C,
                    group_id: 0x00000000,
                    instance_ex: 0xAABBCCDD,
                    instance_low: 0xEEFF0011,
                }],
            },
        ]);

        let res = manifest.to_compressed_resource().unwrap();
        assert_eq!(res.key, S4S_MANIFEST_KEY);
        assert_eq!(res.compressed, 0x5A42);

        let decoded =
            MergedPackageManifest::from_resource_data(&res.data, res.compressed, res.mem_size)
                .unwrap();
        assert_eq!(decoded, manifest);
        assert_eq!(decoded.total_package_count(), 2);
        assert_eq!(decoded.total_resource_count(), 3);
    }

    fn words(values: &[u32]) -> Vec<u8> {
        values.iter().flat_map(|value| value.to_le_bytes()).collect()
    }

    #[test]
    fn manifest_matches_utf8_and_instance_wire_order() {
        // CIL: MergedPackageItem.Read/Write select Encoding.UTF8, Pascal32;
        // BinaryStreamWrapper.WritePascalString prefixes encoded byte count.
        let key = ResourceKey {
            type_id: 0x11223344,
            group_id: 0x55667788,
            instance_low: 0x01234567,
            instance_ex: 0x89ABCDEF,
        };
        let manifest = MergedPackageManifest::with_packages(vec![MergedPackage {
            name: "é🌙".into(),
            resources: vec![key],
        }]);
        let mut expected = words(&[1, 0, 0, 1, 6]);
        expected.extend_from_slice("é🌙".as_bytes());
        expected.extend_from_slice(&1u32.to_le_bytes());
        expected.extend_from_slice(&0x0123456789ABCDEFu64.to_le_bytes());
        expected.extend_from_slice(&words(&[0x11223344, 0x55667788]));
        assert_eq!(manifest.to_bytes().unwrap(), expected);
        assert_eq!(MergedPackageManifest::from_bytes(&expected).unwrap(), manifest);
    }

    #[test]
    fn malformed_small_manifests_reject_counts_versions_and_strings() {
        for bytes in [
            words(&[2, 0, 0, 0]),
            words(&[1, u32::MAX]),
            words(&[1, 0, u32::MAX]),
            words(&[1, 0, 0, u32::MAX]),
            words(&[1, 0, 0, 1, 0, u32::MAX]),
            words(&[1, 0, 0, 0, 42]),
            vec![1, 0, 0, 0, 1, 0, 0, 0, 0xFF, 0, 0, 0, 0, 0, 0, 0, 0],
        ] {
            assert!(MergedPackageManifest::from_bytes(&bytes).is_err());
        }
        let valid = MergedPackageManifest::new().to_bytes().unwrap();
        for end in 0..valid.len() {
            assert!(MergedPackageManifest::from_bytes(&valid[..end]).is_err());
        }
    }

    #[test]
    fn excessive_folder_nesting_is_rejected_on_read_and_write() {
        let mut bytes = words(&[1]);
        for _ in 0..MAX_MANIFEST_DEPTH + 2 {
            bytes.extend_from_slice(&words(&[0, 1]));
        }
        bytes.extend_from_slice(&words(&[0, 0, 0]));
        for _ in 0..MAX_MANIFEST_DEPTH + 2 {
            bytes.extend_from_slice(&0u32.to_le_bytes());
        }
        assert!(MergedPackageManifest::from_bytes(&bytes).is_err());
        let mut folder = MergedFolder::default();
        for _ in 0..MAX_MANIFEST_DEPTH + 1 {
            folder = MergedFolder {
                folders: vec![folder],
                ..MergedFolder::default()
            };
        }
        let mut manifest = MergedPackageManifest::new();
        manifest.root = folder;
        assert!(manifest.to_bytes().is_err());
        manifest.root = MergedFolder::default();
        manifest.version = 2;
        assert!(manifest.to_bytes().is_err());
    }

    #[test]
    fn manifest_compression_requires_exact_complete_supported_resource() {
        let manifest = MergedPackageManifest::new();
        let raw = manifest.to_bytes().unwrap();
        let resource = manifest.to_compressed_resource().unwrap();
        assert_eq!(MergedPackageManifest::from_resource_data(
            &raw, 0, raw.len() as u32,
        ).unwrap(), manifest);
        for compression in [1, 0xFFFF, 0xFFFE] {
            assert!(MergedPackageManifest::from_resource_data(
                &resource.data, compression, resource.mem_size,
            ).is_err());
        }
        for size in [0, resource.mem_size - 1, resource.mem_size + 1, u32::MAX] {
            assert!(MergedPackageManifest::from_resource_data(
                &resource.data, resource.compressed, size,
            ).is_err());
            assert!(MergedPackageManifest::from_resource_data(&raw, 0, size).is_err());
        }
        for end in 0..resource.data.len() {
            assert!(MergedPackageManifest::from_resource_data(
                &resource.data[..end], resource.compressed, resource.mem_size,
            ).is_err());
        }
        let mut trailing = resource.data.clone();
        trailing.push(0);
        assert!(MergedPackageManifest::from_resource_data(
            &trailing, resource.compressed, resource.mem_size,
        ).is_err());
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&vec![0u8; 1024 * 1024]).unwrap();
        let bomb = encoder.finish().unwrap();
        assert!(MergedPackageManifest::from_resource_data(&bomb, 0x5A42, 16).is_err());
    }

    #[test]
    fn compact_manifest_cannot_amplify_flattened_paths_without_bound() {
        let name = "a".repeat(MAX_MANIFEST_STRING_BYTES);
        let count = MAX_MANIFEST_BYTES / name.len();
        let mut bytes = words(&[1, name.len() as u32]);
        bytes.extend_from_slice(name.as_bytes());
        bytes.extend_from_slice(&words(&[0, count as u32]));
        for _ in 0..count {
            bytes.extend_from_slice(&words(&[0, 0]));
        }
        assert!(MergedPackageManifest::from_bytes(&bytes).is_err());
        let mut manifest = MergedPackageManifest::new();
        manifest.root.name = name;
        manifest.root.packages = (0..count).map(|_| MergedPackage {
            name: String::new(), resources: Vec::new(),
        }).collect();
        assert!(manifest.to_bytes().is_err());
    }

}
