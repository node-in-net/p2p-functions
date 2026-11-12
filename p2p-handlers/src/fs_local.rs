use node_functions::fs_local;
use nodeinnet_p2p::P2pMessage;
use p2p_node::NodeContext;
use std::path::{Path, PathBuf};
use tokio::sync::mpsc as tokio_mpsc;

#[cfg(unix)]
const PEER_MODE_MASK: u32 = 0o1777;

#[derive(serde::Deserialize, Clone)]
pub struct Share {
    pub name: String,
    pub path: String,
}

#[derive(serde::Deserialize, Clone, Default)]
pub struct FsConfig {
    #[serde(default)]
    pub shares: Vec<Share>,
}

fn shares_of(ctx: &NodeContext, resource_id: &str) -> Vec<Share> {
    ctx.my_info
        .resources
        .iter()
        .find(|r| r.id == resource_id)
        .and_then(|r| r.config.as_ref())
        .and_then(|c| serde_json::from_str::<FsConfig>(c).ok())
        .map(|c| c.shares)
        .unwrap_or_default()
}

#[derive(Debug, PartialEq)]
enum Resolved {
    Root(Vec<String>),
    At {
        local: PathBuf,
        share_base: PathBuf,
        prefix: String,
    },
}

fn resolve_in(shares: &[Share], vpath: &str) -> Option<Resolved> {
    if shares.is_empty() {
        return None;
    }
    if shares.len() == 1 {
        let share_base = PathBuf::from(&shares[0].path);
        let local = fs_local::resolve_and_sandbox_path(&share_base, vpath)?;
        return Some(Resolved::At {
            local,
            share_base,
            prefix: String::new(),
        });
    }
    let seg = vpath.trim_start_matches('/');
    if seg.is_empty() {
        return Some(Resolved::Root(
            shares.iter().map(|s| s.name.clone()).collect(),
        ));
    }
    let (name, rest) = seg.split_once('/').unwrap_or((seg, ""));
    let share = shares.iter().find(|s| s.name == name)?;
    let share_base = PathBuf::from(&share.path);
    let local = fs_local::resolve_and_sandbox_path(&share_base, rest)?;
    Some(Resolved::At {
        local,
        share_base,
        prefix: format!("/{name}"),
    })
}

fn resolve(ctx: &NodeContext, resource_id: &str, vpath: &str) -> Option<Resolved> {
    match resolve_in(&shares_of(ctx, resource_id), vpath)? {
        Resolved::At {
            local,
            share_base,
            prefix,
        } => fs_local::stays_within(&share_base, &local).then_some(Resolved::At {
            local,
            share_base,
            prefix,
        }),
        root => Some(root),
    }
}

fn virtual_parent(local: &Path, share_base: &Path, prefix: &str) -> String {
    let parent = local.parent().unwrap_or(share_base);
    let rel = parent
        .strip_prefix(share_base)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default();
    match (prefix.is_empty(), rel.is_empty()) {
        (true, true) => "/".to_string(),
        (true, false) => format!("/{rel}"),
        (false, true) => prefix.to_string(),
        (false, false) => format!("{prefix}/{rel}"),
    }
}

pub async fn handle_local_fs_message(p2p_msg: P2pMessage, ctx: NodeContext) {
    match p2p_msg {
        P2pMessage::RequestEntries {
            request_id,
            resource_id,
            path,
        } => {
            ctx.log(format!("📂 Peer requested directory listing for: {}", path));
            let target_dir = match resolve(&ctx, &resource_id, &path) {
                Some(Resolved::At { local, .. }) => local,
                Some(Resolved::Root(names)) => {
                    ctx.send_msg(P2pMessage::EntriesResponse {
                        request_id,
                        resource_id,
                        path,
                        directories: names,
                        files: vec![],
                        directories_with_dates: None,
                        files_with_dates: None,
                        directories_permissions: None,
                        files_permissions: None,
                    })
                    .await;
                    return;
                }
                None => {
                    ctx.log(format!(
                        "⚠️ Rejecting directory listing (unknown share or path traversal): {}",
                        path
                    ));
                    ctx.send_msg(P2pMessage::EntriesResponse {
                        request_id,
                        resource_id,
                        path,
                        directories: vec![],
                        files: vec![],
                        directories_with_dates: None,
                        files_with_dates: None,
                        directories_permissions: None,
                        files_permissions: None,
                    })
                    .await;
                    return;
                }
            };
            let (directories_with_dates, files_with_dates) =
                fs_local::list_local_directory(&target_dir);
            let directories = directories_with_dates.iter().map(|d| d.0.clone()).collect();
            let files = files_with_dates
                .iter()
                .map(|f| (f.0.clone(), f.1))
                .collect();

            #[cfg(unix)]
            let (directories_permissions, files_permissions) = {
                use std::os::unix::fs::MetadataExt;
                let mut dir_perms = Vec::new();
                for d in &directories_with_dates {
                    let mut perm = None;
                    let full_path = target_dir.join(&d.0);
                    if let Ok(metadata) = std::fs::metadata(&full_path) {
                        perm = Some(metadata.mode());
                    }
                    dir_perms.push(perm);
                }

                let mut file_perms = Vec::new();
                for f in &files_with_dates {
                    let mut perm = None;
                    let full_path = target_dir.join(&f.0);
                    if let Ok(metadata) = std::fs::metadata(&full_path) {
                        perm = Some(metadata.mode());
                    }
                    file_perms.push(perm);
                }
                (Some(dir_perms), Some(file_perms))
            };
            #[cfg(not(unix))]
            let (directories_permissions, files_permissions): (
                Option<Vec<Option<u32>>>,
                Option<Vec<Option<u32>>>,
            ) = (None, None);

            ctx.send_msg(P2pMessage::EntriesResponse {
                request_id,
                resource_id,
                path,
                directories,
                files,
                directories_with_dates: Some(directories_with_dates),
                files_with_dates: Some(files_with_dates),
                directories_permissions,
                files_permissions,
            })
            .await;
        }
        P2pMessage::RequestMetadata {
            request_id,
            resource_id,
            path,
        } => {
            let target_entry = match resolve(&ctx, &resource_id, &path) {
                Some(Resolved::At { local, .. }) => local,
                _ => {
                    ctx.send_msg(P2pMessage::MetadataResponse {
                        request_id,
                        resource_id,
                        path,
                        metadata: None,
                    })
                    .await;
                    return;
                }
            };
            let metadata = fs_local::get_local_metadata(&target_entry);
            ctx.send_msg(P2pMessage::MetadataResponse {
                request_id,
                resource_id,
                path,
                metadata,
            })
            .await;
        }
        P2pMessage::CreateDirectoryRequest {
            request_id,
            resource_id,
            parent_path,
            dir_name,
            permissions,
        } => {
            ctx.log(format!(
                "📂 Peer requested to create directory '{}' in '{}'",
                dir_name, parent_path
            ));
            let target_dir = match resolve(&ctx, &resource_id, &parent_path) {
                Some(Resolved::At {
                    local: mut p,
                    share_base,
                    ..
                }) => {
                    if dir_name.contains("..") || dir_name.contains('/') || dir_name.contains('\\')
                    {
                        ctx.send_msg(P2pMessage::CreateDirectoryResponse {
                            request_id,
                            resource_id,
                            parent_path,
                            result: Err("Invalid directory name".to_string()),
                        })
                        .await;
                        return;
                    }
                    p.push(dir_name);
                    if !fs_local::stays_within(&share_base, &p) {
                        ctx.send_msg(P2pMessage::CreateDirectoryResponse {
                            request_id,
                            resource_id,
                            parent_path,
                            result: Err("Cannot create a directory here".to_string()),
                        })
                        .await;
                        return;
                    }
                    p
                }
                _ => {
                    ctx.send_msg(P2pMessage::CreateDirectoryResponse {
                        request_id,
                        resource_id,
                        parent_path,
                        result: Err("Cannot create a directory here".to_string()),
                    })
                    .await;
                    return;
                }
            };
            let result = fs_local::create_local_directory(&target_dir).await;
            if result.is_ok() {
                #[cfg(unix)]
                if let Some(mode) = permissions {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(
                        &target_dir,
                        std::fs::Permissions::from_mode(mode & PEER_MODE_MASK),
                    );
                }
                #[cfg(not(unix))]
                let _ = permissions;
            }
            ctx.send_msg(P2pMessage::CreateDirectoryResponse {
                request_id,
                resource_id,
                parent_path,
                result,
            })
            .await;
        }
        P2pMessage::DeleteEntryRequest {
            request_id,
            resource_id,
            path,
        } => {
            ctx.log(format!("🗑️ Peer requested to delete entry: '{}'", path));
            let (target_entry, parent_path) = match resolve(&ctx, &resource_id, &path) {
                Some(Resolved::At {
                    local,
                    share_base,
                    prefix,
                }) => {
                    let parent = virtual_parent(&local, &share_base, &prefix);
                    (local, parent)
                }
                _ => {
                    ctx.send_msg(P2pMessage::DeleteEntryResponse {
                        request_id,
                        resource_id,
                        parent_path: "".to_string(),
                        result: Err("Unknown share or path traversal".to_string()),
                    })
                    .await;
                    return;
                }
            };
            let result = fs_local::delete_local_entry(&target_entry).await;
            ctx.send_msg(P2pMessage::DeleteEntryResponse {
                request_id,
                resource_id,
                parent_path,
                result,
            })
            .await;
        }
        P2pMessage::RenameEntryRequest {
            request_id,
            resource_id,
            path,
            new_path,
        } => {
            ctx.log(format!(
                "📝 Peer requested to rename/move entry: '{}' to '{}'",
                path, new_path
            ));
            let (target_entry, parent_path) = match resolve(&ctx, &resource_id, &path) {
                Some(Resolved::At {
                    local,
                    share_base,
                    prefix,
                }) => {
                    let parent = virtual_parent(&local, &share_base, &prefix);
                    (local, parent)
                }
                _ => {
                    ctx.send_msg(P2pMessage::RenameEntryResponse {
                        request_id,
                        resource_id,
                        parent_path: "".to_string(),
                        result: Err("Unknown share or path traversal on source".to_string()),
                    })
                    .await;
                    return;
                }
            };
            let new_entry = match resolve(&ctx, &resource_id, &new_path) {
                Some(Resolved::At { local, .. }) => local,
                _ => {
                    ctx.send_msg(P2pMessage::RenameEntryResponse {
                        request_id,
                        resource_id,
                        parent_path,
                        result: Err("Unknown share or path traversal on destination".to_string()),
                    })
                    .await;
                    return;
                }
            };
            let result = fs_local::rename_local_entry(&target_entry, &new_entry).await;
            ctx.send_msg(P2pMessage::RenameEntryResponse {
                request_id,
                resource_id,
                parent_path,
                result,
            })
            .await;
        }
        P2pMessage::SetPermissionsRequest {
            request_id,
            resource_id,
            path,
            permissions,
        } => {
            ctx.log(format!(
                "🔑 Peer requested chmod to {:o} for path: '{}'",
                permissions, path
            ));
            let (target_entry, parent_path) = match resolve(&ctx, &resource_id, &path) {
                Some(Resolved::At {
                    local,
                    share_base,
                    prefix,
                }) => {
                    let parent = virtual_parent(&local, &share_base, &prefix);
                    (local, parent)
                }
                _ => {
                    ctx.send_msg(P2pMessage::SetPermissionsResponse {
                        request_id,
                        resource_id,
                        parent_path: "".to_string(),
                        result: Err("Unknown share or path traversal".to_string()),
                    })
                    .await;
                    return;
                }
            };

            let result = {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(
                        &target_entry,
                        std::fs::Permissions::from_mode(permissions & PEER_MODE_MASK),
                    )
                    .map_err(|e| e.to_string())
                }
                #[cfg(not(unix))]
                {
                    let _ = target_entry;
                    Err("Chmod not supported on this platform".to_string())
                }
            };

            ctx.send_msg(P2pMessage::SetPermissionsResponse {
                request_id,
                resource_id,
                parent_path,
                result,
            })
            .await;
        }
        P2pMessage::FileUploadRequest {
            resource_id,
            target_path,
            file_name,
            total_size,
            transfer_id,
            permissions,
        } => {
            ctx.log(format!(
                "📥 Peer wants to upload file: {} ({} bytes)",
                file_name, total_size
            ));
            let (final_dir, share_base) = match resolve(&ctx, &resource_id, &target_path) {
                Some(Resolved::At {
                    local, share_base, ..
                }) => (local, share_base),
                _ => {
                    ctx.send_msg(P2pMessage::FileTransferResponse {
                        transfer_id,
                        status: nodeinnet_p2p::FileTransferStatus::Rejected {
                            reason: "Cannot upload here (unknown share or virtual root)"
                                .to_string(),
                        },
                    })
                    .await;
                    return;
                }
            };
            if file_name.contains("..") || file_name.contains('/') || file_name.contains('\\') {
                ctx.send_msg(P2pMessage::FileTransferResponse {
                    transfer_id,
                    status: nodeinnet_p2p::FileTransferStatus::Rejected {
                        reason: "Invalid file name".to_string(),
                    },
                })
                .await;
                return;
            }
            let final_path = final_dir.join(&file_name);
            if !fs_local::stays_within(&share_base, &final_path) {
                ctx.send_msg(P2pMessage::FileTransferResponse {
                    transfer_id,
                    status: nodeinnet_p2p::FileTransferStatus::Rejected {
                        reason: "Cannot upload here (outside the share)".to_string(),
                    },
                })
                .await;
                return;
            }
            let _ = fs_local::create_local_directory(&final_dir).await;

            match fs_local::create_local_file_within(&share_base, &final_path).await {
                Err(e) => {
                    ctx.log(format!("❌ Failed to create file for upload: {}", e));
                    ctx.send_msg(P2pMessage::FileTransferResponse {
                        transfer_id,
                        status: nodeinnet_p2p::FileTransferStatus::Rejected { reason: e },
                    })
                    .await;
                }
                Ok(file) => {
                    let file = std::sync::Arc::new(tokio::sync::Mutex::new(file));
                    ctx.active_downloads.lock().await.insert(
                        transfer_id,
                        (final_path, total_size, permissions, Some(file)),
                    );
                    ctx.send_msg(P2pMessage::FileTransferResponse {
                        transfer_id,
                        status: nodeinnet_p2p::FileTransferStatus::Accepted { total_size },
                    })
                    .await;
                    ctx.log(format!("📤 Accepted upload request: {}", file_name));
                }
            }
        }
        P2pMessage::FileDownloadRequest {
            resource_id,
            file_path,
            transfer_id,
            offset,
            length,
        } => {
            let (tx, mut rx) =
                tokio_mpsc::channel(client_core::limits::outbound_queue_slots(16 * 1024));
            let started = match resolve(&ctx, &resource_id, &file_path) {
                Some(Resolved::At {
                    local, share_base, ..
                }) => match fs_local::open_local_file_within(&share_base, &local).await {
                    Ok(file) => fs_local::stream_local_file(file, tx, offset, length).await,
                    Err(e) => Err(e),
                },
                _ => Err("Cannot download (unknown share or virtual root)".to_string()),
            };
            let ctx_clone = ctx.clone();
            tokio::spawn(async move {
                match started {
                    Ok(total_size) => {
                        ctx_clone
                            .send_msg(P2pMessage::FileTransferResponse {
                                transfer_id,
                                status: nodeinnet_p2p::FileTransferStatus::Accepted { total_size },
                            })
                            .await;
                        let mut offset = 0u64;
                        let file_name = file_path
                            .split('/')
                            .next_back()
                            .unwrap_or("file")
                            .to_string();

                        while let Some(res) = rx.recv().await {
                            match res {
                                Ok(data) => {
                                    if data.is_empty() {
                                        break;
                                    }
                                    ctx_clone
                                        .send_msg(P2pMessage::FileChunk {
                                            transfer_id,
                                            offset,
                                            data: data.clone(),
                                        })
                                        .await;
                                    offset += data.len() as u64;
                                    let _ = ctx_clone
                                        .local_event_tx
                                        .send(p2p_node::LocalP2pEvent::TransferProgress {
                                            transfer_id,
                                            file_name: file_name.clone(),
                                            bytes_read: offset,
                                            total_bytes: total_size,
                                        })
                                        .await;
                                }
                                Err(e) => {
                                    ctx_clone.log(format!("❌ Error reading file stream: {}", e));
                                    break;
                                }
                            }
                        }
                        ctx_clone
                            .send_msg(P2pMessage::FileTransferComplete {
                                transfer_id,
                                checksum: None,
                            })
                            .await;
                        let _ = ctx_clone
                            .local_event_tx
                            .send(p2p_node::LocalP2pEvent::TransferComplete {
                                transfer_id,
                                file_name,
                                is_upload: true,
                            })
                            .await;
                    }
                    Err(e) => {
                        ctx_clone
                            .send_msg(P2pMessage::FileTransferResponse {
                                transfer_id,
                                status: nodeinnet_p2p::FileTransferStatus::Rejected { reason: e },
                            })
                            .await;
                    }
                }
            });
        }
        P2pMessage::FileTransferResponse {
            transfer_id,
            status,
        } => match status {
            nodeinnet_p2p::FileTransferStatus::Accepted { total_size } => {
                let is_upload = { ctx.active_uploads.lock().await.remove(&transfer_id) };
                if let Some(local_path) = is_upload {
                    ctx.log(format!(
                        "⬆️ Upload accepted! Starting transfer {}...",
                        transfer_id
                    ));
                    let ctx_clone = ctx.clone();
                    tokio::spawn(async move {
                        let (tx, mut rx) = tokio_mpsc::channel(
                            client_core::limits::outbound_queue_slots(16 * 1024),
                        );
                        if fs_local::start_local_download(local_path.clone(), tx, 0, None)
                            .await
                            .is_ok()
                        {
                            let mut offset = 0u64;
                            let file_name = local_path
                                .file_name()
                                .unwrap_or_default()
                                .to_string_lossy()
                                .to_string();
                            while let Some(res) = rx.recv().await {
                                match res {
                                    Ok(data) => {
                                        if data.is_empty() {
                                            break;
                                        }
                                        ctx_clone
                                            .send_msg(P2pMessage::FileChunk {
                                                transfer_id,
                                                offset,
                                                data: data.clone(),
                                            })
                                            .await;
                                        offset += data.len() as u64;
                                        let _ = ctx_clone
                                            .local_event_tx
                                            .send(p2p_node::LocalP2pEvent::TransferProgress {
                                                transfer_id,
                                                file_name: file_name.clone(),
                                                bytes_read: offset,
                                                total_bytes: total_size,
                                            })
                                            .await;
                                    }
                                    Err(_) => break,
                                }
                            }
                            ctx_clone
                                .send_msg(P2pMessage::FileTransferComplete {
                                    transfer_id,
                                    checksum: None,
                                })
                                .await;
                            let _ = ctx_clone
                                .local_event_tx
                                .send(p2p_node::LocalP2pEvent::TransferComplete {
                                    transfer_id,
                                    file_name,
                                    is_upload: true,
                                })
                                .await;
                        }
                    });
                } else if ctx.requested_downloads.lock().await.remove(&transfer_id) {
                    let temp_path = std::env::temp_dir().join(transfer_id.to_string());
                    ctx.active_downloads
                        .lock()
                        .await
                        .insert(transfer_id, (temp_path, total_size, None, None));
                } else {
                    ctx.log(format!(
                        "⏭️ No download registered for accepted transfer {}",
                        transfer_id
                    ));
                }
            }
            nodeinnet_p2p::FileTransferStatus::Rejected { reason } => {
                ctx.requested_downloads.lock().await.remove(&transfer_id);
                ctx.log(format!(
                    "❌ Upload transfer {} rejected: {}",
                    transfer_id, reason
                ));
            }
        },
        P2pMessage::FileChunk {
            transfer_id,
            offset,
            data,
        } => {
            let stream_info = { ctx.active_downloads.lock().await.get(&transfer_id).cloned() };
            if let Some((target_path, total_size, _, file)) = stream_info {
                let Some(current_total) = offset
                    .checked_add(data.len() as u64)
                    .filter(|end| *end <= total_size)
                else {
                    return;
                };
                let written = match file {
                    Some(file) => {
                        fs_local::write_file_chunk(&mut *file.lock().await, offset, &data).await
                    }
                    None => fs_local::write_local_file_chunk(&target_path, offset, &data).await,
                };
                if written.is_ok() {
                    let _ = ctx
                        .local_event_tx
                        .send(p2p_node::LocalP2pEvent::TransferProgress {
                            transfer_id,
                            file_name: target_path
                                .file_name()
                                .unwrap_or_default()
                                .to_string_lossy()
                                .to_string(),
                            bytes_read: current_total,
                            total_bytes: total_size,
                        })
                        .await;
                }
            }
        }
        P2pMessage::FileTransferComplete {
            transfer_id,
            checksum: _,
        } => {
            let (file_name, _permissions) = {
                let mut map = ctx.active_downloads.lock().await;
                if let Some((path, _, permissions, file)) = map.remove(&transfer_id) {
                    #[cfg(unix)]
                    if let (Some(mode), Some(file)) = (permissions, &file) {
                        use std::os::unix::fs::PermissionsExt;
                        let _ = file
                            .lock()
                            .await
                            .set_permissions(std::fs::Permissions::from_mode(mode & PEER_MODE_MASK))
                            .await;
                    }
                    #[cfg(not(unix))]
                    let _ = file;
                    (
                        path.file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_string(),
                        permissions,
                    )
                } else {
                    ("unknown".to_string(), None)
                }
            };
            ctx.log(format!("✅ Transfer {} complete", transfer_id));
            let _ = ctx
                .local_event_tx
                .send(p2p_node::LocalP2pEvent::TransferComplete {
                    transfer_id,
                    file_name,
                    is_upload: false,
                })
                .await;
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shares(pairs: &[(&str, &str)]) -> Vec<Share> {
        pairs
            .iter()
            .map(|(n, p)| Share {
                name: n.to_string(),
                path: p.to_string(),
            })
            .collect()
    }

    fn at(local: &str, base: &str, prefix: &str) -> Option<Resolved> {
        Some(Resolved::At {
            local: PathBuf::from(local),
            share_base: PathBuf::from(base),
            prefix: prefix.to_string(),
        })
    }

    #[test]
    fn fs_config_parses_the_shares_list() {
        let cfg: FsConfig = serde_json::from_str(
            r#"{"shares":[{"name":"Downloads","path":"/home/u/dl"},{"name":"Docs","path":"/home/u/docs"}]}"#,
        )
        .unwrap();
        assert_eq!(cfg.shares.len(), 2);
        assert_eq!(cfg.shares[0].name, "Downloads");
        assert_eq!(cfg.shares[1].path, "/home/u/docs");
    }

    #[test]
    fn single_share_content_is_the_root() {
        let s = shares(&[("Home", "/srv/home")]);
        assert_eq!(resolve_in(&s, "/"), at("/srv/home", "/srv/home", ""));
        assert_eq!(
            resolve_in(&s, "/docs/a.txt"),
            at("/srv/home/docs/a.txt", "/srv/home", "")
        );
    }

    #[test]
    fn multi_share_root_lists_the_names() {
        let s = shares(&[("Downloads", "/srv/dl"), ("Docs", "/srv/docs")]);
        assert_eq!(
            resolve_in(&s, "/"),
            Some(Resolved::Root(vec!["Downloads".into(), "Docs".into()]))
        );
    }

    #[test]
    fn multi_share_dispatches_into_the_named_share() {
        let s = shares(&[("Downloads", "/srv/dl"), ("Docs", "/srv/docs")]);
        assert_eq!(
            resolve_in(&s, "/Docs"),
            at("/srv/docs", "/srv/docs", "/Docs")
        );
        assert_eq!(
            resolve_in(&s, "/Downloads/sub/f.txt"),
            at("/srv/dl/sub/f.txt", "/srv/dl", "/Downloads")
        );
    }

    #[test]
    fn unknown_share_fails_closed() {
        let s = shares(&[("A", "/srv/a"), ("B", "/srv/b")]);
        assert_eq!(resolve_in(&s, "/Nope"), None);
        assert_eq!(resolve_in(&s, "/Nope/x"), None);
    }

    #[test]
    fn no_shares_resolves_to_nothing() {
        assert_eq!(resolve_in(&[], "/"), None);
        assert_eq!(resolve_in(&[], "/anything"), None);
    }

    #[test]
    fn path_traversal_is_rejected() {
        let one = shares(&[("Home", "/srv/home")]);
        assert_eq!(resolve_in(&one, "/../etc/passwd"), None);
        let many = shares(&[("A", "/srv/a"), ("B", "/srv/b")]);
        assert_eq!(resolve_in(&many, "/A/../../etc"), None);
    }

    #[cfg(unix)]
    struct Scratch(PathBuf);

    #[cfg(unix)]
    impl Scratch {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!("p2p-handlers-{}", uuid::Uuid::new_v4())))
        }
    }

    #[cfg(unix)]
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[cfg(unix)]
    fn sharing(
        share: &Path,
    ) -> (
        NodeContext,
        tokio::sync::mpsc::Receiver<nodeinnet_p2p::OutboundP2pPayload>,
    ) {
        let config =
            serde_json::json!({ "shares": [{ "name": "Share", "path": share }] }).to_string();
        let (out_tx, out_rx) = tokio::sync::mpsc::channel(8);
        let (log_tx, _) = tokio::sync::mpsc::channel(8);
        let (event_tx, _) = tokio::sync::mpsc::channel(8);
        let me = nodeinnet_p2p::NodeInfo {
            id: "me".to_string(),
            name: "me".to_string(),
            os: "linux".to_string(),
            version: "0.0.0".to_string(),
            app_type: "test".to_string(),
            build_type: "debug".to_string(),
            public_key: String::new(),
            resources: vec![nodeinnet_p2p::SharedResource {
                id: "fs-test".to_string(),
                name: "Files".to_string(),
                resource_type: nodeinnet_p2p::ResourceType::Filesystem,
                config: Some(config),
                is_active: true,
                session_token: None,
            }],
            is_online: true,
            last_used: 0,
            is_temporary: false,
        };
        let ctx = NodeContext::new(
            out_tx,
            log_tx,
            event_tx,
            me,
            "peer".to_string(),
            std::sync::Arc::new(nodeinnet_p2p::MemoryPeerStore::default()),
        );
        (ctx, out_rx)
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_out_of_a_share_resolves_to_none() {
        let root = Scratch::new();
        let share = root.0.join("share");
        let outside = root.0.join("outside");
        std::fs::create_dir_all(share.join("docs")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), b"secret").unwrap();
        std::os::unix::fs::symlink(&outside, share.join("out")).unwrap();
        let (ctx, _out_rx) = sharing(&share);

        assert!(resolve(&ctx, "fs-test", "/docs").is_some());
        assert!(resolve(&ctx, "fs-test", "/docs/new/file.txt").is_some());
        assert_eq!(resolve(&ctx, "fs-test", "/out"), None);
        assert_eq!(resolve(&ctx, "fs-test", "/out/secret.txt"), None);
        assert_eq!(resolve(&ctx, "fs-test", "/out/new.txt"), None);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_upload_onto_a_link_out_of_the_share_is_refused() {
        let root = Scratch::new();
        let share = root.0.join("share");
        let outside = root.0.join("outside");
        std::fs::create_dir_all(share.join("docs")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(outside.join("planted.txt"), share.join("docs/evil")).unwrap();
        let (ctx, mut out_rx) = sharing(&share);

        handle_local_fs_message(
            P2pMessage::FileUploadRequest {
                resource_id: "fs-test".to_string(),
                target_path: "/docs".to_string(),
                file_name: "evil".to_string(),
                total_size: 6,
                transfer_id: uuid::Uuid::new_v4(),
                permissions: None,
            },
            ctx,
        )
        .await;

        let answer = out_rx.try_recv().expect("an answer to the upload");
        assert!(matches!(
            answer,
            nodeinnet_p2p::OutboundP2pPayload::Message(nodeinnet_p2p::SecuredP2pEnvelope {
                message: P2pMessage::FileTransferResponse {
                    status: nodeinnet_p2p::FileTransferStatus::Rejected { .. },
                    ..
                },
                ..
            })
        ));
        assert!(!outside.join("planted.txt").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_delete_through_a_link_out_of_the_share_is_refused() {
        let root = Scratch::new();
        let share = root.0.join("share");
        let outside = root.0.join("outside");
        std::fs::create_dir_all(&share).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), b"secret").unwrap();
        std::os::unix::fs::symlink(&outside, share.join("out")).unwrap();
        let (ctx, mut out_rx) = sharing(&share);

        for path in ["/out", "/out/", "/out/."] {
            handle_local_fs_message(
                P2pMessage::DeleteEntryRequest {
                    request_id: uuid::Uuid::new_v4(),
                    resource_id: "fs-test".to_string(),
                    path: path.to_string(),
                },
                ctx.clone(),
            )
            .await;
            let answer = out_rx.try_recv().expect("an answer to the delete");
            assert!(
                matches!(
                    answer,
                    nodeinnet_p2p::OutboundP2pPayload::Message(nodeinnet_p2p::SecuredP2pEnvelope {
                        message: P2pMessage::DeleteEntryResponse { result: Err(_), .. },
                        ..
                    })
                ),
                "deleting {path} was not refused"
            );
        }
        assert!(outside.join("secret.txt").exists());
        assert!(share.join("out").symlink_metadata().is_ok());
    }

    #[cfg(unix)]
    fn messages(
        out_rx: &mut tokio::sync::mpsc::Receiver<nodeinnet_p2p::OutboundP2pPayload>,
    ) -> Vec<P2pMessage> {
        std::iter::from_fn(|| out_rx.try_recv().ok())
            .filter_map(|payload| match payload {
                nodeinnet_p2p::OutboundP2pPayload::Message(envelope) => Some(envelope.message),
                _ => None,
            })
            .collect()
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_link_renamed_over_an_accepted_upload_gets_none_of_its_bytes() {
        use std::os::unix::fs::PermissionsExt;
        let root = Scratch::new();
        let share = root.0.join("share");
        let outside = root.0.join("x");
        std::fs::create_dir_all(share.join("a")).unwrap();
        std::fs::write(share.join("x"), b"inside").unwrap();
        std::fs::write(&outside, b"outside").unwrap();
        std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::os::unix::fs::symlink("../x", share.join("a/l")).unwrap();
        let (ctx, mut out_rx) = sharing(&share);
        let transfer_id = uuid::Uuid::new_v4();

        for msg in [
            P2pMessage::FileUploadRequest {
                resource_id: "fs-test".to_string(),
                target_path: "/".to_string(),
                file_name: "victim".to_string(),
                total_size: 6,
                transfer_id,
                permissions: Some(0o777),
            },
            P2pMessage::RenameEntryRequest {
                request_id: uuid::Uuid::new_v4(),
                resource_id: "fs-test".to_string(),
                path: "/a/l".to_string(),
                new_path: "/victim".to_string(),
            },
            P2pMessage::FileChunk {
                transfer_id,
                offset: 0,
                data: b"hacked".to_vec(),
            },
            P2pMessage::FileTransferComplete {
                transfer_id,
                checksum: None,
            },
        ] {
            handle_local_fs_message(msg, ctx.clone()).await;
        }

        let answers = messages(&mut out_rx);
        assert!(matches!(
            answers[0],
            P2pMessage::FileTransferResponse {
                status: nodeinnet_p2p::FileTransferStatus::Accepted { .. },
                ..
            }
        ));
        assert!(matches!(
            answers[1],
            P2pMessage::RenameEntryResponse { result: Ok(()), .. }
        ));
        assert_eq!(std::fs::read(&outside).unwrap(), b"outside");
        assert_eq!(
            std::fs::metadata(&outside).unwrap().permissions().mode() & 0o7777,
            0o600
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_download_is_opened_before_the_next_request_runs() {
        let root = Scratch::new();
        let share = root.0.join("share");
        std::fs::create_dir_all(share.join("a")).unwrap();
        std::fs::write(share.join("x"), b"inside").unwrap();
        std::fs::write(share.join("victim"), b"victim").unwrap();
        std::fs::write(root.0.join("x"), b"outside").unwrap();
        std::os::unix::fs::symlink("../x", share.join("a/l")).unwrap();
        let (ctx, mut out_rx) = sharing(&share);

        handle_local_fs_message(
            P2pMessage::FileDownloadRequest {
                resource_id: "fs-test".to_string(),
                file_path: "/victim".to_string(),
                transfer_id: uuid::Uuid::new_v4(),
                offset: 0,
                length: None,
            },
            ctx,
        )
        .await;
        std::fs::rename(share.join("a/l"), share.join("victim")).unwrap();

        let mut sent = Vec::new();
        loop {
            let payload = tokio::time::timeout(std::time::Duration::from_secs(5), out_rx.recv())
                .await
                .expect("the download went quiet")
                .expect("the download ended without FileTransferComplete");
            let nodeinnet_p2p::OutboundP2pPayload::Message(envelope) = payload else {
                continue;
            };
            match envelope.message {
                P2pMessage::FileTransferResponse {
                    status: nodeinnet_p2p::FileTransferStatus::Accepted { .. },
                    ..
                } => {}
                P2pMessage::FileChunk { data, .. } => sent.extend(data),
                P2pMessage::FileTransferComplete { .. } => break,
                other => panic!("unexpected answer to the download: {other:?}"),
            }
        }
        assert_eq!(sent, b"victim");
    }

    #[cfg(unix)]
    fn mode_of(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o7777
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_peer_chmod_keeps_the_sticky_bit_but_not_setuid() {
        let root = Scratch::new();
        std::fs::create_dir_all(root.0.join("dir")).unwrap();
        std::fs::write(root.0.join("tool"), b"#!/bin/sh\n").unwrap();
        let (ctx, _out_rx) = sharing(&root.0);

        for (path, permissions) in [("/tool", 0o4755), ("/dir", 0o1777)] {
            handle_local_fs_message(
                P2pMessage::SetPermissionsRequest {
                    request_id: uuid::Uuid::new_v4(),
                    resource_id: "fs-test".to_string(),
                    path: path.to_string(),
                    permissions,
                },
                ctx.clone(),
            )
            .await;
        }

        assert_eq!(mode_of(&root.0.join("tool")), 0o755);
        assert_eq!(mode_of(&root.0.join("dir")), 0o1777);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_peer_mkdir_cannot_set_setgid() {
        let root = Scratch::new();
        std::fs::create_dir_all(&root.0).unwrap();
        let (ctx, _out_rx) = sharing(&root.0);

        handle_local_fs_message(
            P2pMessage::CreateDirectoryRequest {
                request_id: uuid::Uuid::new_v4(),
                resource_id: "fs-test".to_string(),
                parent_path: "/".to_string(),
                dir_name: "made".to_string(),
                permissions: Some(0o2775),
            },
            ctx,
        )
        .await;

        assert_eq!(mode_of(&root.0.join("made")), 0o775);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_peer_upload_cannot_set_setuid_or_setgid() {
        let root = Scratch::new();
        std::fs::create_dir_all(&root.0).unwrap();
        let (ctx, _out_rx) = sharing(&root.0);
        let transfer_id = uuid::Uuid::new_v4();

        for msg in [
            P2pMessage::FileUploadRequest {
                resource_id: "fs-test".to_string(),
                target_path: "/".to_string(),
                file_name: "tool".to_string(),
                total_size: 10,
                transfer_id,
                permissions: Some(0o6755),
            },
            P2pMessage::FileChunk {
                transfer_id,
                offset: 0,
                data: b"#!/bin/sh\n".to_vec(),
            },
            P2pMessage::FileTransferComplete {
                transfer_id,
                checksum: None,
            },
        ] {
            handle_local_fs_message(msg, ctx.clone()).await;
        }

        assert_eq!(std::fs::read(root.0.join("tool")).unwrap(), b"#!/bin/sh\n");
        assert_eq!(mode_of(&root.0.join("tool")), 0o755);
    }

    #[cfg(unix)]
    struct Downloaded(PathBuf);

    #[cfg(unix)]
    impl Downloaded {
        fn of(transfer_id: uuid::Uuid) -> Self {
            Self(std::env::temp_dir().join(transfer_id.to_string()))
        }
    }

    #[cfg(unix)]
    impl Drop for Downloaded {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[cfg(unix)]
    async fn deliver_download(
        ctx: &NodeContext,
        transfer_id: uuid::Uuid,
        total_size: u64,
        chunks: &[(u64, &[u8])],
    ) {
        let mut messages = vec![P2pMessage::FileTransferResponse {
            transfer_id,
            status: nodeinnet_p2p::FileTransferStatus::Accepted { total_size },
        }];
        messages.extend(chunks.iter().map(|(offset, data)| P2pMessage::FileChunk {
            transfer_id,
            offset: *offset,
            data: data.to_vec(),
        }));
        messages.push(P2pMessage::FileTransferComplete {
            transfer_id,
            checksum: None,
        });
        for msg in messages {
            handle_local_fs_message(msg, ctx.clone()).await;
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn only_a_download_this_node_asked_for_reaches_the_temp_dir() {
        let root = Scratch::new();
        let (ctx, _out_rx) = sharing(&root.0);
        let unasked = uuid::Uuid::new_v4();
        let rejected = uuid::Uuid::new_v4();
        let asked = uuid::Uuid::new_v4();
        let files = [unasked, rejected, asked].map(Downloaded::of);
        ctx.requested_downloads
            .lock()
            .await
            .extend([rejected, asked]);

        handle_local_fs_message(
            P2pMessage::FileTransferResponse {
                transfer_id: rejected,
                status: nodeinnet_p2p::FileTransferStatus::Rejected {
                    reason: "no".to_string(),
                },
            },
            ctx.clone(),
        )
        .await;
        for transfer_id in [unasked, rejected, asked] {
            deliver_download(&ctx, transfer_id, 6, &[(0, b"hi bob")]).await;
        }

        assert!(!files[0].0.exists(), "an unrequested transfer was written");
        assert!(
            !files[1].0.exists(),
            "a rejected request still opened a download"
        );
        assert_eq!(std::fs::read(&files[2].0).unwrap(), b"hi bob");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn chunks_past_the_announced_size_are_dropped() {
        let root = Scratch::new();
        let (ctx, _out_rx) = sharing(&root.0);
        let transfer_id = uuid::Uuid::new_v4();
        let file = Downloaded::of(transfer_id);
        ctx.requested_downloads.lock().await.insert(transfer_id);

        deliver_download(
            &ctx,
            transfer_id,
            6,
            &[
                (u64::MAX - 1, b"hi bob"),
                (3, b"bob!"),
                (6, b"!"),
                (0, b"hi bob"),
            ],
        )
        .await;

        assert_eq!(std::fs::read(&file.0).unwrap(), b"hi bob");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_share_upload_cannot_grow_past_its_announced_size() {
        let root = Scratch::new();
        std::fs::create_dir_all(&root.0).unwrap();
        let (ctx, _out_rx) = sharing(&root.0);
        let transfer_id = uuid::Uuid::new_v4();

        for msg in [
            P2pMessage::FileUploadRequest {
                resource_id: "fs-test".to_string(),
                target_path: "/".to_string(),
                file_name: "up.txt".to_string(),
                total_size: 6,
                transfer_id,
                permissions: None,
            },
            P2pMessage::FileChunk {
                transfer_id,
                offset: 1 << 40,
                data: b"far".to_vec(),
            },
            P2pMessage::FileChunk {
                transfer_id,
                offset: 0,
                data: b"hi bob".to_vec(),
            },
            P2pMessage::FileTransferComplete {
                transfer_id,
                checksum: None,
            },
        ] {
            handle_local_fs_message(msg, ctx.clone()).await;
        }

        let uploaded = root.0.join("up.txt");
        assert_eq!(std::fs::metadata(&uploaded).unwrap().len(), 6);
        assert_eq!(std::fs::read(&uploaded).unwrap(), b"hi bob");
    }

    #[test]
    fn virtual_parent_stays_in_the_virtual_namespace() {
        assert_eq!(
            virtual_parent(Path::new("/srv/home/a/b.txt"), Path::new("/srv/home"), ""),
            "/a"
        );
        assert_eq!(
            virtual_parent(Path::new("/srv/home/a"), Path::new("/srv/home"), ""),
            "/"
        );
        assert_eq!(
            virtual_parent(
                Path::new("/srv/dl/a/b.txt"),
                Path::new("/srv/dl"),
                "/Downloads"
            ),
            "/Downloads/a"
        );
        assert_eq!(
            virtual_parent(Path::new("/srv/dl/a"), Path::new("/srv/dl"), "/Downloads"),
            "/Downloads"
        );
    }
}
