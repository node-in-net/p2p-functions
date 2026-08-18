use node_functions::fs_local;
use nodeinnet_p2p::P2pMessage;
use p2p_node::NodeContext;
use std::path::{Path, PathBuf};
use tokio::sync::mpsc as tokio_mpsc;

/// One named share: a human `name` (what a peer sees as a folder in the virtual
/// root) → a `path` that stays LOCAL (never leaves this node). Peers address
/// shares by name; we resolve the name to the local path here, on the server.
#[derive(serde::Deserialize, Clone)]
pub struct Share {
    pub name: String,
    pub path: String,
}

/// A Filesystem resource's LOCAL config: its list of named shares.
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

/// Where a peer's VIRTUAL path lands inside our named-share namespace.
#[derive(Debug, PartialEq)]
enum Resolved {
    /// The virtual root (≥2 shares, path == "/"): callers list these share
    /// NAMES as folders; writes/metadata here are rejected (no backing dir).
    Root(Vec<String>),
    /// A concrete local path, plus what response paths need to stay virtual:
    /// the share's local base and its `/<name>` prefix ("" for a lone share
    /// whose content IS the root).
    At {
        local: PathBuf,
        share_base: PathBuf,
        prefix: String,
    },
}

/// Pure routing (unit-tested): map a peer VIRTUAL path onto the share list.
/// 1 share ⇒ its content is the root; ≥2 ⇒ "/" lists names and "/<name>/rest"
/// dispatches into that share (each sandboxed under its OWN base). Returns
/// `None` (fail CLOSED) on an unknown share or a path-traversal attempt.
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
    resolve_in(&shares_of(ctx, resource_id), vpath)
}

/// The virtual parent-directory path of a resolved local target — what goes in
/// `*Response.parent_path` so the consumer stays in the virtual namespace.
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
                    // Virtual root: list the shares themselves, as folders.
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
            // No mode bits off unix, so the peer gets no permission vectors at all.
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
                    // Virtual root or unknown share/traversal: no file metadata.
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
                Some(Resolved::At { local: mut p, .. }) => {
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
                    p
                }
                _ => {
                    // Virtual root (no backing dir) or unknown share / traversal.
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
                        std::fs::Permissions::from_mode(mode),
                    );
                }
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
                        std::fs::Permissions::from_mode(permissions),
                    )
                    .map_err(|e| e.to_string())
                }
                #[cfg(not(unix))]
                {
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
            let final_dir = match resolve(&ctx, &resource_id, &target_path) {
                Some(Resolved::At { local, .. }) => local,
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
            ctx.active_downloads
                .lock()
                .await
                .insert(transfer_id, (final_path.clone(), total_size, permissions));
            let _ = fs_local::create_local_directory(&final_dir).await;

            // Just test if we can create it, then accept. The stream processing will happen in FileChunk.
            if let Err(e) = fs_local::create_local_file(&final_path).await {
                ctx.log(format!("❌ Failed to create file for upload: {}", e));
                ctx.send_msg(P2pMessage::FileTransferResponse {
                    transfer_id,
                    status: nodeinnet_p2p::FileTransferStatus::Rejected {
                        reason: e.to_string(),
                    },
                })
                .await;
            } else {
                ctx.send_msg(P2pMessage::FileTransferResponse {
                    transfer_id,
                    status: nodeinnet_p2p::FileTransferStatus::Accepted { total_size },
                })
                .await;
                ctx.log(format!("📤 Accepted upload request: {}", file_name));
            }
        }
        P2pMessage::FileDownloadRequest {
            resource_id,
            file_path,
            transfer_id,
        } => {
            let ctx_clone = ctx.clone();
            tokio::spawn(async move {
                let target_file = match resolve(&ctx_clone, &resource_id, &file_path) {
                    Some(Resolved::At { local, .. }) => local,
                    _ => {
                        ctx_clone
                            .send_msg(P2pMessage::FileTransferResponse {
                                transfer_id,
                                status: nodeinnet_p2p::FileTransferStatus::Rejected {
                                    reason: "Cannot download (unknown share or virtual root)"
                                        .to_string(),
                                },
                            })
                            .await;
                        return;
                    }
                };

                let (tx, mut rx) = tokio_mpsc::channel(1024);
                match fs_local::start_local_download(target_file, tx).await {
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
                        let (tx, mut rx) = tokio_mpsc::channel(1024);
                        if fs_local::start_local_download(local_path.clone(), tx)
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
                } else {
                    let temp_path = std::env::temp_dir().join(transfer_id.to_string());
                    ctx.active_downloads
                        .lock()
                        .await
                        .insert(transfer_id, (temp_path, total_size, None));
                }
            }
            nodeinnet_p2p::FileTransferStatus::Rejected { reason } => {
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
            if let Some((target_path, total_size, _)) = stream_info {
                if fs_local::write_local_file_chunk(&target_path, offset, &data)
                    .await
                    .is_ok()
                {
                    let current_total = offset + data.len() as u64;
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
                if let Some((path, _, permissions)) = map.remove(&transfer_id) {
                    #[cfg(unix)]
                    if let Some(mode) = permissions {
                        use std::os::unix::fs::PermissionsExt;
                        let _ =
                            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode));
                    }
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

    #[test]
    fn virtual_parent_stays_in_the_virtual_namespace() {
        // single share: no prefix
        assert_eq!(
            virtual_parent(Path::new("/srv/home/a/b.txt"), Path::new("/srv/home"), ""),
            "/a"
        );
        assert_eq!(
            virtual_parent(Path::new("/srv/home/a"), Path::new("/srv/home"), ""),
            "/"
        );
        // multi share: "/<Name>" prefix
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
