use nodeinnet_p2p::p2p::{FileDifference, SyncDiffItem, SyncFileInfo};
use nodeinnet_p2p::{ResourceType, SharedResource};
use std::collections::BTreeMap;

#[cfg(test)]
fn make_file(path: &str, hash: &str, size: u64) -> SyncFileInfo {
    SyncFileInfo {
        relative_path: path.to_string(),
        md5_hash: hash.to_string(),
        size,
        last_modified_ms: 0,
    }
}

#[cfg(test)]
fn make_resource(name: &str, path: &str, ignores: &[&str]) -> SharedResource {
    let ignores_json: Vec<String> = ignores.iter().map(|s| format!("\"{}\"", s)).collect();
    let config = format!(
        r#"{{"path": "{}", "ignores": [{}]}}"#,
        path,
        ignores_json.join(", ")
    );
    SharedResource {
        id: name.to_string(),
        name: name.to_string(),
        resource_type: ResourceType::SyncFolder,
        config: Some(config),
        is_active: true,
        session_token: None,
    }
}

pub fn compute_sync_diff(
    local_cache: &BTreeMap<String, Vec<SyncFileInfo>>,
    states: &BTreeMap<String, Vec<SyncFileInfo>>,
) -> Vec<SyncDiffItem> {
    let mut diffs = Vec::new();
    for (folder_name, local_files) in local_cache {
        if let Some(remote_files) = states.get(folder_name) {
            for rf in remote_files {
                if let Some(lf) = local_files
                    .iter()
                    .find(|f| f.relative_path == rf.relative_path)
                {
                    if lf.md5_hash != rf.md5_hash || lf.size != rf.size {
                        diffs.push(SyncDiffItem {
                            folder_name: folder_name.clone(),
                            relative_path: rf.relative_path.clone(),
                            local_state: Some(lf.clone()),
                            remote_state: Some(rf.clone()),
                            difference: FileDifference::Modified,
                        });
                    } else {
                        diffs.push(SyncDiffItem {
                            folder_name: folder_name.clone(),
                            relative_path: rf.relative_path.clone(),
                            local_state: Some(lf.clone()),
                            remote_state: Some(rf.clone()),
                            difference: FileDifference::Identical,
                        });
                    }
                } else {
                    diffs.push(SyncDiffItem {
                        folder_name: folder_name.clone(),
                        relative_path: rf.relative_path.clone(),
                        local_state: None,
                        remote_state: Some(rf.clone()),
                        difference: FileDifference::RemoteOnly,
                    });
                }
            }
            for lf in local_files {
                if !remote_files
                    .iter()
                    .any(|f| f.relative_path == lf.relative_path)
                {
                    diffs.push(SyncDiffItem {
                        folder_name: folder_name.clone(),
                        relative_path: lf.relative_path.clone(),
                        local_state: Some(lf.clone()),
                        remote_state: None,
                        difference: FileDifference::LocalOnly,
                    });
                }
            }
        } else {
            for lf in local_files {
                diffs.push(SyncDiffItem {
                    folder_name: folder_name.clone(),
                    relative_path: lf.relative_path.clone(),
                    local_state: Some(lf.clone()),
                    remote_state: None,
                    difference: FileDifference::LocalOnly,
                });
            }
        }
    }
    diffs
}

pub fn scan_local_resource_folders(
    resources: &[SharedResource],
) -> BTreeMap<String, Vec<SyncFileInfo>> {
    let mut states = BTreeMap::new();
    for res in resources {
        if res.resource_type == ResourceType::SyncFolder {
            let mut files_info = Vec::new();
            if let Some(cfg_str) = &res.config {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(cfg_str) {
                    if let Some(path_str) = json.get("path").and_then(|v| v.as_str()) {
                        let ignores = json
                            .get("ignores")
                            .and_then(|v| v.as_array())
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                                    .collect::<Vec<_>>()
                            })
                            .unwrap_or_default();

                        let root_path = std::path::Path::new(path_str);
                        if root_path.exists() && root_path.is_dir() {
                            let walker = walkdir::WalkDir::new(root_path).into_iter().filter_entry(
                                move |e| {
                                    if let Ok(rel_path) = e.path().strip_prefix(root_path) {
                                        let rel_str = rel_path.to_string_lossy().replace("\\", "/");
                                        if rel_str.is_empty() {
                                            return true;
                                        }
                                        let parts: Vec<&str> = rel_str.split('/').collect();
                                        for ignore in &ignores {
                                            let ignore = ignore.trim();
                                            if ignore.is_empty() {
                                                continue;
                                            }
                                            if parts.contains(&ignore) {
                                                return false;
                                            }
                                            if let Some(prefix) = ignore.strip_suffix('*') {
                                                if rel_str.starts_with(prefix) {
                                                    return false;
                                                }
                                            }
                                            if let Some(suffix) = ignore.strip_prefix('*') {
                                                if rel_str.ends_with(suffix) {
                                                    return false;
                                                }
                                            }
                                            if rel_str == ignore {
                                                return false;
                                            }
                                        }
                                    }
                                    true
                                },
                            );

                            for entry in walker.filter_map(|e| e.ok()) {
                                let path = entry.path();
                                if path.is_file() {
                                    if let Ok(metadata) = std::fs::metadata(path) {
                                        if let Ok(rel_path) = path.strip_prefix(root_path) {
                                            let rel_str =
                                                rel_path.to_string_lossy().replace("\\", "/");
                                            let size = metadata.len();
                                            let last_modified_ms = metadata
                                                .modified()
                                                .ok()
                                                .and_then(|m| {
                                                    m.duration_since(std::time::UNIX_EPOCH).ok()
                                                })
                                                .map(|d| d.as_millis() as u64)
                                                .unwrap_or(0);
                                            let md5_hash =
                                                if let Ok(mut file) = std::fs::File::open(path) {
                                                    use std::io::Read;
                                                    let mut context = md5::Context::new();
                                                    let mut buffer = [0; 8192];
                                                    loop {
                                                        match file.read(&mut buffer) {
                                                            Ok(0) => break, // EOF
                                                            Ok(n) => context.consume(&buffer[..n]),
                                                            Err(_) => break,
                                                        }
                                                    }
                                                    format!("{:x}", context.finalize())
                                                } else {
                                                    String::new()
                                                };

                                            files_info.push(SyncFileInfo {
                                                relative_path: rel_str,
                                                md5_hash,
                                                size,
                                                last_modified_ms,
                                            });
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            states.insert(res.name.clone(), files_info);
        }
    }
    states
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;


    #[test]
    fn empty_inputs_produce_no_diffs() {
        let result = compute_sync_diff(&BTreeMap::new(), &BTreeMap::new());
        assert!(result.is_empty());
    }

    #[test]
    fn identical_files_marked_identical() {
        let f = make_file("a.txt", "abc123", 10);
        let local = BTreeMap::from([("docs".into(), vec![f.clone()])]);
        let remote = BTreeMap::from([("docs".into(), vec![f.clone()])]);

        let diffs = compute_sync_diff(&local, &remote);
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].difference, FileDifference::Identical);
        assert_eq!(diffs[0].relative_path, "a.txt");
    }

    #[test]
    fn different_hash_marked_modified() {
        let local_f = make_file("a.txt", "old_hash", 10);
        let remote_f = make_file("a.txt", "new_hash", 10);
        let local = BTreeMap::from([("docs".into(), vec![local_f])]);
        let remote = BTreeMap::from([("docs".into(), vec![remote_f])]);

        let diffs = compute_sync_diff(&local, &remote);
        assert_eq!(diffs[0].difference, FileDifference::Modified);
    }

    #[test]
    fn different_size_same_hash_marked_modified() {
        let local_f = make_file("a.txt", "same_hash", 10);
        let remote_f = make_file("a.txt", "same_hash", 20);
        let local = BTreeMap::from([("docs".into(), vec![local_f])]);
        let remote = BTreeMap::from([("docs".into(), vec![remote_f])]);

        let diffs = compute_sync_diff(&local, &remote);
        assert_eq!(diffs[0].difference, FileDifference::Modified);
    }

    #[test]
    fn file_only_in_remote_is_remote_only() {
        let remote_f = make_file("new.txt", "hash", 5);
        let local = BTreeMap::from([("docs".into(), vec![])]);
        let remote = BTreeMap::from([("docs".into(), vec![remote_f])]);

        let diffs = compute_sync_diff(&local, &remote);
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].difference, FileDifference::RemoteOnly);
        assert!(diffs[0].local_state.is_none());
        assert!(diffs[0].remote_state.is_some());
    }

    #[test]
    fn file_only_in_local_is_local_only() {
        let local_f = make_file("old.txt", "hash", 5);
        let local = BTreeMap::from([("docs".into(), vec![local_f])]);
        let remote = BTreeMap::from([("docs".into(), vec![])]);

        let diffs = compute_sync_diff(&local, &remote);
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].difference, FileDifference::LocalOnly);
        assert!(diffs[0].local_state.is_some());
        assert!(diffs[0].remote_state.is_none());
    }

    #[test]
    fn folder_missing_from_remote_all_local_only() {
        let files = vec![make_file("a.txt", "h1", 1), make_file("b.txt", "h2", 2)];
        let local = BTreeMap::from([("docs".into(), files)]);
        let remote = BTreeMap::new(); // folder not present at all

        let diffs = compute_sync_diff(&local, &remote);
        assert_eq!(diffs.len(), 2);
        assert!(diffs
            .iter()
            .all(|d| d.difference == FileDifference::LocalOnly));
    }

    #[test]
    fn mixed_diff_in_one_folder() {
        let local = BTreeMap::from([(
            "docs".into(),
            vec![
                make_file("same.txt", "hash", 10),
                make_file("changed.txt", "old", 10),
                make_file("local_only.txt", "hx", 5),
            ],
        )]);
        let remote = BTreeMap::from([(
            "docs".into(),
            vec![
                make_file("same.txt", "hash", 10),
                make_file("changed.txt", "new", 10),
                make_file("remote_only.txt", "hy", 5),
            ],
        )]);

        let diffs = compute_sync_diff(&local, &remote);
        assert_eq!(diffs.len(), 4);

        let by_path = |path: &str| {
            diffs
                .iter()
                .find(|d| d.relative_path == path)
                .unwrap()
                .difference
        };
        assert_eq!(by_path("same.txt"), FileDifference::Identical);
        assert_eq!(by_path("changed.txt"), FileDifference::Modified);
        assert_eq!(by_path("remote_only.txt"), FileDifference::RemoteOnly);
        assert_eq!(by_path("local_only.txt"), FileDifference::LocalOnly);
    }

    #[test]
    fn multiple_folders_diffs_carry_correct_folder_name() {
        let local = BTreeMap::from([
            ("music".into(), vec![make_file("song.mp3", "h1", 100)]),
            ("docs".into(), vec![make_file("readme.txt", "h2", 20)]),
        ]);
        let remote = BTreeMap::from([
            ("music".into(), vec![make_file("song.mp3", "h1", 100)]),
            ("docs".into(), vec![make_file("readme.txt", "CHANGED", 20)]),
        ]);

        let diffs = compute_sync_diff(&local, &remote);
        assert_eq!(diffs.len(), 2);
        let music = diffs.iter().find(|d| d.folder_name == "music").unwrap();
        let docs = diffs.iter().find(|d| d.folder_name == "docs").unwrap();
        assert_eq!(music.difference, FileDifference::Identical);
        assert_eq!(docs.difference, FileDifference::Modified);
    }


    #[test]
    fn non_sync_folder_resource_is_skipped() {
        let res = SharedResource {
            id: "x".into(),
            name: "x".into(),
            resource_type: ResourceType::Filesystem,
            config: None,
            is_active: true,
            session_token: None,
        };
        let result = scan_local_resource_folders(&[res]);
        assert!(result.is_empty());
    }

    #[test]
    fn scans_files_with_correct_name_and_size() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("file.txt"), b"hello world").unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("sub/nested.txt"), b"data").unwrap();

        let res = make_resource("myfolder", &dir.path().to_string_lossy(), &[]);
        let result = scan_local_resource_folders(&[res]);

        let files = result.get("myfolder").unwrap();
        assert_eq!(files.len(), 2);

        let find = |name: &str| files.iter().find(|f| f.relative_path.contains(name));
        assert_eq!(find("file.txt").unwrap().size, 11);
        assert_eq!(find("nested.txt").unwrap().size, 4);
    }

    #[test]
    fn scan_produces_non_empty_md5_hash() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"content").unwrap();

        let res = make_resource("f", &dir.path().to_string_lossy(), &[]);
        let result = scan_local_resource_folders(&[res]);

        let file = &result["f"][0];
        assert!(!file.md5_hash.is_empty());
        assert_eq!(file.md5_hash, "9a0364b9e99bb480dd25e1f0284c8555");
    }

    #[test]
    fn ignore_by_exact_dir_name() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("keep.txt"), b"x").unwrap();
        fs::create_dir(dir.path().join(".git")).unwrap();
        fs::write(dir.path().join(".git/HEAD"), b"ref").unwrap();

        let res = make_resource("f", &dir.path().to_string_lossy(), &[".git"]);
        let result = scan_local_resource_folders(&[res]);

        let files = &result["f"];
        assert_eq!(files.len(), 1);
        assert!(files[0].relative_path.contains("keep.txt"));
    }

    #[test]
    fn ignore_by_suffix_wildcard() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("main.rs"), b"fn main() {}").unwrap();
        fs::write(dir.path().join("notes.log"), b"log data").unwrap();

        let res = make_resource("f", &dir.path().to_string_lossy(), &["*.log"]);
        let result = scan_local_resource_folders(&[res]);

        let files = &result["f"];
        assert_eq!(files.len(), 1);
        assert!(files[0].relative_path.ends_with(".rs"));
    }

    #[test]
    fn nonexistent_path_returns_empty_entry() {
        let res = make_resource("f", "/this/path/does/not/exist/xyz", &[]);
        let result = scan_local_resource_folders(&[res]);
        let files = result.get("f").unwrap();
        assert!(files.is_empty());
    }

    #[test]
    fn scan_multiple_resources() {
        let dir1 = tempdir().unwrap();
        let dir2 = tempdir().unwrap();
        fs::write(dir1.path().join("a.txt"), b"1").unwrap();
        fs::write(dir2.path().join("b.txt"), b"2").unwrap();

        let result = scan_local_resource_folders(&[
            make_resource("folder1", &dir1.path().to_string_lossy(), &[]),
            make_resource("folder2", &dir2.path().to_string_lossy(), &[]),
        ]);

        assert_eq!(result["folder1"].len(), 1);
        assert_eq!(result["folder2"].len(), 1);
        assert!(result["folder1"][0].relative_path.contains("a.txt"));
        assert!(result["folder2"][0].relative_path.contains("b.txt"));
    }
}
