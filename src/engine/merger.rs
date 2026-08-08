use crate::engine::dbpf::{DBPFReader, DBPFWriter, PackageResource, ResourceKey};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergeReport {
    pub success: bool,
    pub partial: bool,
    pub total_files_found: usize,
    pub success_count: usize,
    pub failed_files: Vec<(String, String)>,
    pub output_packages: Vec<String>,
}

pub struct MergeTask {
    pub input_files: Vec<PathBuf>,
    pub output_dir: PathBuf,
    pub max_size_bytes: u64,
}

pub fn merge_sims4_packages<F>(
    task: &MergeTask,
    progress_cb: F,
) -> Result<MergeReport, Box<dyn std::error::Error + Send + Sync>>
where
    F: Fn(usize, usize, &str) + Send + Sync,
{
    fs::create_dir_all(&task.output_dir)?;

    let total_files = task.input_files.len();
    if total_files == 0 {
        return Ok(MergeReport {
            success: false,
            partial: false,
            total_files_found: 0,
            success_count: 0,
            failed_files: Vec::new(),
            output_packages: Vec::new(),
        });
    }

    let timestamp = chrono_like_timestamp();
    let mut global_resources: IndexMap<ResourceKey, (PathBuf, u32, u32, u32, u16)> = IndexMap::new();
    let mut failed_files = Vec::new();
    let mut success_count = 0;

    for (idx, fpath) in task.input_files.iter().enumerate() {
        let fname = fpath.file_name().unwrap_or_default().to_string_lossy();
        progress_cb(idx + 1, total_files, &format!("Lendo Metadados: {}/{} ({})", idx + 1, total_files, fname));

        match File::open(fpath) {
            Ok(file) => {
                let mut reader = BufReader::with_capacity(512 * 1024, file);
                match DBPFReader::read_index(&mut reader) {
                    Ok((_header, index)) => {
                        for entry in index {
                            // Later files in task.input_files overwrite earlier ones
                            global_resources.insert(
                                entry.key,
                                (
                                    fpath.clone(),
                                    entry.location_offset,
                                    entry.file_size,
                                    entry.mem_size,
                                    entry.compressed,
                                ),
                            );
                        }
                        success_count += 1;
                    }
                    Err(e) => {
                        failed_files.push((fpath.display().to_string(), e.to_string()));
                    }
                }
            }
            Err(e) => {
                failed_files.push((fpath.display().to_string(), e.to_string()));
            }
        }
    }

    if global_resources.is_empty() {
        return Ok(MergeReport {
            success: false,
            partial: false,
            total_files_found: total_files,
            success_count: 0,
            failed_files,
            output_packages: Vec::new(),
        });
    }

    // Sort resources by TGI for engine load times
    let mut sorted_keys: Vec<ResourceKey> = global_resources.keys().copied().collect();
    sorted_keys.sort();

    let mut chunk_index = 1;
    let mut current_chunk_resources: Vec<PackageResource> = Vec::new();
    let mut current_chunk_bytes = 0u64;
    let mut output_packages = Vec::new();

    let safe_limit = ((task.max_size_bytes as f64) * 0.90) as u64;

    // Cache opened files to avoid reopening repeatedly
    let mut file_cache: std::collections::HashMap<PathBuf, BufReader<File>> = std::collections::HashMap::new();

    for (res_idx, key) in sorted_keys.iter().enumerate() {
        if res_idx % 100 == 0 || res_idx == sorted_keys.len() - 1 {
            progress_cb(res_idx + 1, sorted_keys.len(), &format!("Processando Recursos: {}/{}", res_idx + 1, sorted_keys.len()));
        }

        let (src_path, offset, size, mem_size, compressed) = &global_resources[key];

        if !file_cache.contains_key(src_path) {
            if let Ok(f) = File::open(src_path) {
                file_cache.insert(src_path.clone(), BufReader::with_capacity(4 * 1024 * 1024, f));
            }
        }

        let mut data = vec![0u8; *size as usize];
        if let Some(reader) = file_cache.get_mut(src_path) {
            if reader.seek(SeekFrom::Start(*offset as u64)).is_ok() {
                let _ = reader.read_exact(&mut data);
            }
        }

        let res_len = data.len() as u64;
        if current_chunk_bytes + res_len > safe_limit && !current_chunk_resources.is_empty() {
            let part_name = format!("Merged_Content_{}_Part{:03}.package", timestamp, chunk_index);
            let part_path = task.output_dir.join(&part_name);
            write_package_chunk(&part_path, &current_chunk_resources)?;
            output_packages.push(part_path.display().to_string());

            chunk_index += 1;
            current_chunk_resources.clear();
            current_chunk_bytes = 0;
        }

        current_chunk_resources.push(PackageResource {
            key: *key,
            data,
            mem_size: *mem_size,
            compressed: *compressed,
        });
        current_chunk_bytes += res_len;
    }

    if !current_chunk_resources.is_empty() {
        let part_name = format!("Merged_Content_{}_Part{:03}.package", timestamp, chunk_index);
        let part_path = task.output_dir.join(&part_name);
        write_package_chunk(&part_path, &current_chunk_resources)?;
        output_packages.push(part_path.display().to_string());
    }

    let report = MergeReport {
        success: failed_files.is_empty(),
        partial: !failed_files.is_empty() && success_count > 0,
        total_files_found: total_files,
        success_count,
        failed_files,
        output_packages,
    };

    let report_path = task.output_dir.join("merge_report.json");
    if let Ok(json) = serde_json::to_string_pretty(&report) {
        let _ = fs::write(report_path, json);
    }

    Ok(report)
}

fn write_package_chunk(part_path: &Path, resources: &[PackageResource]) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let file = File::create(part_path)?;
    let mut writer = BufWriter::with_capacity(4 * 1024 * 1024, file);
    DBPFWriter::write_package(&mut writer, resources)?;
    let inner = writer.into_inner()?;
    inner.sync_all()?;
    Ok(())
}

fn chrono_like_timestamp() -> String {
    let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default();
    format!("{}", now.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::dbpf::ResourceKey;
    use tempfile::tempdir;

    #[test]
    fn test_merger_tgi_overwrite() {
        let dir = tempdir().unwrap();
        let pkg1_path = dir.path().join("mod1.package");
        let pkg2_path = dir.path().join("mod2.package");
        let out_dir = dir.path().join("output");

        let key_shared = ResourceKey {
            type_id: 0x11111111,
            group_id: 0x0,
            instance_ex: 0x0,
            instance_low: 0x1,
        };

        let res1 = PackageResource {
            key: key_shared,
            data: b"Old Version Content from Mod1".to_vec(),
            mem_size: 29,
            compressed: 0,
        };

        let res2 = PackageResource {
            key: key_shared,
            data: b"NEW UPDATED VERSION Content from Mod2".to_vec(),
            mem_size: 37,
            compressed: 0,
        };

        let mut f1 = File::create(&pkg1_path).unwrap();
        DBPFWriter::write_package(&mut f1, &[res1]).unwrap();

        let mut f2 = File::create(&pkg2_path).unwrap();
        DBPFWriter::write_package(&mut f2, &[res2]).unwrap();

        let task = MergeTask {
            input_files: vec![pkg1_path, pkg2_path],
            output_dir: out_dir.clone(),
            max_size_bytes: 1_000_000_000,
        };

        let report = merge_sims4_packages(&task, |_, _, _| {}).unwrap();
        assert!(report.success);
        assert_eq!(report.output_packages.len(), 1);

        let merged_file_path = PathBuf::from(&report.output_packages[0]);
        let mut f_merged = File::open(&merged_file_path).unwrap();
        let (_hdr, index) = DBPFReader::read_index(&mut f_merged).unwrap();

        assert_eq!(index.len(), 1);
        assert_eq!(index[0].key, key_shared);
        assert_eq!(index[0].file_size, 37); // Length of NEW UPDATED VERSION from Mod2
    }
}
