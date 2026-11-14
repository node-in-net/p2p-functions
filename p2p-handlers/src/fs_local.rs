use node_functions::fs_local;
use node_functions::names::{folded, Comparison};
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

fn is_plain_name(name: &str) -> bool {
    let mut components = Path::new(name).components();
    matches!(
        (components.next(), components.next()),
        (Some(std::path::Component::Normal(only)), None) if only == name
    ) && !(cfg!(windows) && node_functions::names::windows_refuses(name))
}

fn windows_refuses_within(share_base: &Path, local: &Path) -> bool {
    cfg!(windows)
        && local.strip_prefix(share_base).map_or(true, |rel| {
            rel.components()
                .any(|c| node_functions::names::windows_refuses(&c.as_os_str().to_string_lossy()))
        })
}

fn host_comparison() -> Comparison {
    if cfg!(windows) {
        Comparison::Windows
    } else if cfg!(any(target_os = "macos", target_os = "android")) {
        Comparison::MacOs
    } else {
        Comparison::Exact
    }
}

fn stored_as(share_base: &Path, local: &Path) -> Option<PathBuf> {
    let by = host_comparison();
    if by == Comparison::Exact {
        return None;
    }
    let mut stored = share_base.to_path_buf();
    for part in local.strip_prefix(share_base).ok()?.components() {
        let wanted = part.as_os_str();
        let wanted_name = wanted.to_string_lossy();
        let key = folded(&wanted_name, by);
        let names: Vec<_> = std::fs::read_dir(fs_local::sanitize_windows_path(&stored))
            .ok()?
            .flatten()
            .map(|entry| entry.file_name())
            .collect();
        let name = names
            .iter()
            .find(|name| name.as_os_str() == wanted)
            .or_else(|| {
                names
                    .iter()
                    .find(|name| folded(&name.to_string_lossy(), by) == key)
            })?;
        stored.push(name);
    }
    Some(stored)
}

type Reserved = Vec<(u64, PathBuf)>;

static RESERVED: std::sync::Mutex<Reserved> = std::sync::Mutex::new(Vec::new());

struct Reservation(u64);

impl Drop for Reservation {
    fn drop(&mut self) {
        reserved().retain(|(id, _)| *id != self.0);
    }
}

fn reserved() -> std::sync::MutexGuard<'static, Reserved> {
    RESERVED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn folded_parts(path: &Path, by: Comparison) -> Vec<String> {
    path.components()
        .map(|part| folded(&part.as_os_str().to_string_lossy(), by).into_owned())
        .collect()
}

fn taken_by<'a>(reserved: &'a Reserved, path: &Path, by: Comparison) -> Option<&'a PathBuf> {
    let wanted = folded_parts(path, by);
    reserved
        .iter()
        .map(|(_, held)| held)
        .find(|held| held.file_name() != path.file_name() && folded_parts(held, by) == wanted)
}

fn reserve(path: &Path, by: Comparison) -> Result<Reservation, PathBuf> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let mut reserved = reserved();
    if let Some(held) = taken_by(&reserved, path, by) {
        return Err(held.clone());
    }
    let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    reserved.push((id, path.to_path_buf()));
    Ok(Reservation(id))
}

fn already_there(share_base: &Path, wanted: &Path, stored: &Path) -> String {
    let shown = |path: &Path| {
        path.strip_prefix(share_base)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/")
    };
    format!(
        "\"{}\" already exists here as \"{}\"",
        shown(wanted),
        shown(stored)
    )
}

fn download_path(transfer_id: uuid::Uuid) -> PathBuf {
    std::env::temp_dir().join(transfer_id.to_string())
}

async fn write_download_chunk(
    ctx: &NodeContext,
    transfer_id: uuid::Uuid,
    path: &Path,
    total_size: u64,
    offset: u64,
    data: &[u8],
) -> Result<(), String> {
    let done = download_path(transfer_id);
    if path == done {
        return fs_local::write_local_file_chunk(path, offset, data).await;
    }
    let have = tokio::fs::metadata(path).await.map_or(0, |m| m.len());
    if offset > have {
        return Err(format!("bytes {have}..{offset} never arrived"));
    }
    fs_local::write_local_file_chunk(path, offset, data).await?;
    if have.max(offset + data.len() as u64) < total_size {
        return Ok(());
    }
    fs_local::rename_local_entry(path, &done).await?;
    if let Some(download) = ctx.active_downloads.lock().await.get_mut(&transfer_id) {
        download.path = done;
    }
    Ok(())
}

async fn write_upload_chunk(
    ctx: &NodeContext,
    transfer_id: uuid::Uuid,
    upload: &p2p_node::ActiveUpload,
    total_size: u64,
    offset: u64,
    data: &[u8],
) -> Result<u64, String> {
    let end = offset
        .checked_add(data.len() as u64)
        .filter(|end| *end <= total_size)
        .ok_or_else(|| format!("more than the {total_size} bytes announced"))?;
    if offset > upload.stored {
        return Err(format!("bytes {}..{offset} never arrived", upload.stored));
    }
    fs_local::write_file_chunk(&mut *upload.file.lock().await, offset, data).await?;
    if let Some(p2p_node::ActiveDownload {
        upload: Some(upload),
        ..
    }) = ctx.active_downloads.lock().await.get_mut(&transfer_id)
    {
        upload.stored = upload.stored.max(end);
    }
    Ok(end)
}

async fn discard_upload(upload: p2p_node::ActiveUpload) {
    let temp = upload.temp.clone();
    drop(upload);
    let _ = tokio::fs::remove_file(temp).await;
}

async fn store_upload(path: &Path, upload: p2p_node::ActiveUpload) -> Result<(), String> {
    let path = fs_local::sanitize_windows_path(path);
    let p2p_node::ActiveUpload {
        file,
        temp,
        permissions,
        reservation,
        ..
    } = upload;
    let synced = {
        let file = file.lock().await;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let replaced = tokio::fs::symlink_metadata(&path)
                .await
                .ok()
                .filter(|meta| meta.is_file())
                .map(|meta| meta.permissions().mode());
            if let Some(mode) = permissions.or(replaced) {
                let _ = file
                    .set_permissions(std::fs::Permissions::from_mode(mode & PEER_MODE_MASK))
                    .await;
            }
        }
        #[cfg(not(unix))]
        let _ = permissions;
        file.sync_all().await.map_err(|e| e.to_string())
    };
    drop(file);
    if let Err(e) = synced {
        let _ = tokio::fs::remove_file(&temp).await;
        return Err(e);
    }
    if let Err(e) = tokio::fs::rename(&temp, &path).await {
        let _ = tokio::fs::remove_file(&temp).await;
        return Err(e.to_string());
    }
    drop(reservation);
    #[cfg(unix)]
    if let Some(dir) = path.parent() {
        if let Ok(dir) = tokio::fs::File::open(dir).await {
            let _ = dir.sync_all().await;
        }
    }
    Ok(())
}

async fn answer_upload(
    ctx: &NodeContext,
    transfer_id: uuid::Uuid,
    path: &Path,
    result: Result<(), String>,
) {
    match &result {
        Ok(()) => ctx.log(format!("✅ Upload {} stored", transfer_id)),
        Err(e) => ctx.log(format!("❌ Upload {} failed: {}", transfer_id, e)),
    }
    ctx.send_msg(P2pMessage::FileTransferReceipt {
        transfer_id,
        result,
    })
    .await;
    let _ = ctx
        .local_event_tx
        .send(p2p_node::LocalP2pEvent::TransferComplete {
            transfer_id,
            file_name: path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string(),
            is_upload: false,
        })
        .await;
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
                    if dir_name.contains("..")
                        || dir_name.contains('/')
                        || dir_name.contains('\\')
                        || !is_plain_name(&dir_name)
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
                    if let Some(stored) = stored_as(&share_base, &p).filter(|s| *s != p) {
                        ctx.send_msg(P2pMessage::CreateDirectoryResponse {
                            request_id,
                            resource_id,
                            parent_path,
                            result: Err(already_there(&share_base, &p, &stored)),
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
            let (target_entry, target_base, parent_path) = match resolve(&ctx, &resource_id, &path)
            {
                Some(Resolved::At {
                    local,
                    share_base,
                    prefix,
                }) => {
                    let parent = virtual_parent(&local, &share_base, &prefix);
                    (local, share_base, parent)
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
            let (new_entry, new_base) = match resolve(&ctx, &resource_id, &new_path) {
                Some(Resolved::At {
                    local, share_base, ..
                }) if !windows_refuses_within(&share_base, &local) => (local, share_base),
                Some(Resolved::At { .. }) => {
                    ctx.send_msg(P2pMessage::RenameEntryResponse {
                        request_id,
                        resource_id,
                        parent_path,
                        result: Err("Invalid file name".to_string()),
                    })
                    .await;
                    return;
                }
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
            let _reservation = match reserve(&new_entry, host_comparison()) {
                Ok(reservation) => reservation,
                Err(held) => {
                    ctx.send_msg(P2pMessage::RenameEntryResponse {
                        request_id,
                        resource_id,
                        parent_path,
                        result: Err(already_there(&new_base, &new_entry, &held)),
                    })
                    .await;
                    return;
                }
            };
            let stored = stored_as(&new_base, &new_entry).filter(|stored| {
                *stored != new_entry
                    && Some(stored) != stored_as(&target_base, &target_entry).as_ref()
            });
            if let Some(stored) = stored {
                ctx.send_msg(P2pMessage::RenameEntryResponse {
                    request_id,
                    resource_id,
                    parent_path,
                    result: Err(already_there(&new_base, &new_entry, &stored)),
                })
                .await;
                return;
            }
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
            if file_name.contains("..")
                || file_name.contains('/')
                || file_name.contains('\\')
                || !is_plain_name(&file_name)
                || windows_refuses_within(&share_base, &final_dir)
            {
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
            let reservation = match reserve(&final_path, host_comparison()) {
                Ok(reservation) => reservation,
                Err(held) => {
                    ctx.send_msg(P2pMessage::FileTransferResponse {
                        transfer_id,
                        status: nodeinnet_p2p::FileTransferStatus::Rejected {
                            reason: already_there(&share_base, &final_path, &held),
                        },
                    })
                    .await;
                    return;
                }
            };
            if let Some(stored) = stored_as(&share_base, &final_path).filter(|s| *s != final_path) {
                ctx.send_msg(P2pMessage::FileTransferResponse {
                    transfer_id,
                    status: nodeinnet_p2p::FileTransferStatus::Rejected {
                        reason: already_there(&share_base, &final_path, &stored),
                    },
                })
                .await;
                return;
            }
            let _ = fs_local::create_local_directory(&final_dir).await;
            if tokio::fs::symlink_metadata(fs_local::sanitize_windows_path(&final_path))
                .await
                .is_ok_and(|meta| meta.is_dir())
            {
                ctx.send_msg(P2pMessage::FileTransferResponse {
                    transfer_id,
                    status: nodeinnet_p2p::FileTransferStatus::Rejected {
                        reason: "Cannot upload over a folder".to_string(),
                    },
                })
                .await;
                return;
            }

            let temp = final_dir.join(format!(".~{transfer_id}.part"));
            match fs_local::create_new_file_within(&share_base, &temp).await {
                Err(e) => {
                    ctx.log(format!("❌ Failed to create file for upload: {}", e));
                    ctx.send_msg(P2pMessage::FileTransferResponse {
                        transfer_id,
                        status: nodeinnet_p2p::FileTransferStatus::Rejected { reason: e },
                    })
                    .await;
                }
                Ok(file) => {
                    ctx.active_downloads.lock().await.insert(
                        transfer_id,
                        p2p_node::ActiveDownload {
                            path: final_path,
                            total_size,
                            upload: Some(p2p_node::ActiveUpload {
                                file: std::sync::Arc::new(tokio::sync::Mutex::new(file)),
                                temp: fs_local::sanitize_windows_path(&temp),
                                permissions,
                                stored: 0,
                                reservation: std::sync::Arc::new(reservation),
                            }),
                        },
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
                if ctx.requested_downloads.lock().await.remove(&transfer_id) {
                    let done = download_path(transfer_id);
                    let temp_path = if total_size == 0 {
                        let _ = fs_local::create_local_file(&done).await;
                        done
                    } else {
                        done.with_extension("part")
                    };
                    ctx.active_downloads.lock().await.insert(
                        transfer_id,
                        p2p_node::ActiveDownload {
                            path: temp_path,
                            total_size,
                            upload: None,
                        },
                    );
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
            let Some(p2p_node::ActiveDownload {
                path: target_path,
                total_size,
                upload,
            }) = stream_info
            else {
                return;
            };
            let current_total = match upload {
                Some(upload) => {
                    match write_upload_chunk(&ctx, transfer_id, &upload, total_size, offset, &data)
                        .await
                    {
                        Ok(end) => end,
                        Err(reason) => {
                            drop(upload);
                            let removed = ctx.active_downloads.lock().await.remove(&transfer_id);
                            if let Some(p2p_node::ActiveDownload {
                                upload: Some(upload),
                                ..
                            }) = removed
                            {
                                discard_upload(upload).await;
                                ctx.log(format!("❌ Upload {} failed: {}", transfer_id, reason));
                                ctx.send_msg(P2pMessage::FileTransferReceipt {
                                    transfer_id,
                                    result: Err(reason),
                                })
                                .await;
                            }
                            return;
                        }
                    }
                }
                None => {
                    let Some(end) = offset
                        .checked_add(data.len() as u64)
                        .filter(|end| *end <= total_size)
                    else {
                        return;
                    };
                    if write_download_chunk(
                        &ctx,
                        transfer_id,
                        &target_path,
                        total_size,
                        offset,
                        &data,
                    )
                    .await
                    .is_err()
                    {
                        return;
                    }
                    end
                }
            };
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
        P2pMessage::FileTransferComplete {
            transfer_id,
            checksum: _,
        } => {
            let finished = ctx.active_downloads.lock().await.remove(&transfer_id);
            let path = match finished {
                Some(p2p_node::ActiveDownload {
                    path,
                    total_size,
                    upload: Some(upload),
                }) => {
                    if upload.stored < total_size {
                        let stored = upload.stored;
                        discard_upload(upload).await;
                        let ended = Err(format!("ended after {stored} of {total_size} bytes"));
                        answer_upload(&ctx, transfer_id, &path, ended).await;
                    } else {
                        let ctx = ctx.clone();
                        tokio::spawn(async move {
                            let stored = store_upload(&path, upload).await;
                            answer_upload(&ctx, transfer_id, &path, stored).await;
                        });
                    }
                    return;
                }
                Some(download) => download.path,
                None => {
                    let unknown = Err(format!("no upload {transfer_id} is open here"));
                    answer_upload(&ctx, transfer_id, Path::new("unknown"), unknown).await;
                    return;
                }
            };
            if path != download_path(transfer_id) {
                let _ = fs_local::delete_local_entry(&path).await;
                ctx.log(format!(
                    "❌ Transfer {} ended before all its bytes arrived",
                    transfer_id
                ));
            } else {
                ctx.log(format!("✅ Transfer {} complete", transfer_id));
            }
            let _ = ctx
                .local_event_tx
                .send(p2p_node::LocalP2pEvent::TransferComplete {
                    transfer_id,
                    file_name: path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string(),
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

    #[cfg(any(unix, windows))]
    struct Scratch(PathBuf);

    #[cfg(any(unix, windows))]
    impl Scratch {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!("p2p-handlers-{}", uuid::Uuid::new_v4())))
        }
    }

    #[cfg(any(unix, windows))]
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[cfg(any(unix, windows))]
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

    #[cfg(any(unix, windows))]
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

    #[cfg(any(unix, windows))]
    async fn next_answer(
        out_rx: &mut tokio::sync::mpsc::Receiver<nodeinnet_p2p::OutboundP2pPayload>,
    ) -> P2pMessage {
        loop {
            let payload = tokio::time::timeout(std::time::Duration::from_secs(5), out_rx.recv())
                .await
                .expect("nothing was answered within 5 s")
                .expect("the session closed");
            if let nodeinnet_p2p::OutboundP2pPayload::Message(envelope) = payload {
                return envelope.message;
            }
        }
    }

    #[cfg(unix)]
    async fn receipt(
        out_rx: &mut tokio::sync::mpsc::Receiver<nodeinnet_p2p::OutboundP2pPayload>,
    ) -> Result<(), String> {
        loop {
            if let P2pMessage::FileTransferReceipt { result, .. } = next_answer(out_rx).await {
                return result;
            }
        }
    }

    #[cfg(any(unix, windows))]
    fn upload_of(
        name: &str,
        total_size: u64,
        chunks: &[(u64, &[u8])],
        permissions: Option<u32>,
    ) -> (uuid::Uuid, Vec<P2pMessage>) {
        let transfer_id = uuid::Uuid::new_v4();
        let mut messages = vec![P2pMessage::FileUploadRequest {
            resource_id: "fs-test".to_string(),
            target_path: "/".to_string(),
            file_name: name.to_string(),
            total_size,
            transfer_id,
            permissions,
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
        (transfer_id, messages)
    }

    #[cfg(any(unix, windows))]
    fn names_in(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        names.sort();
        names
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

        assert!(matches!(
            next_answer(&mut out_rx).await,
            P2pMessage::FileTransferResponse {
                status: nodeinnet_p2p::FileTransferStatus::Accepted { .. },
                ..
            }
        ));
        assert!(matches!(
            next_answer(&mut out_rx).await,
            P2pMessage::RenameEntryResponse { result: Ok(()), .. }
        ));
        assert_eq!(receipt(&mut out_rx).await, Ok(()));
        let victim = share.join("victim");
        assert!(std::fs::symlink_metadata(&victim).unwrap().is_file());
        assert_eq!(std::fs::read(&victim).unwrap(), b"hacked");
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
    #[tokio::test]
    async fn a_fifo_in_a_share_is_refused_and_the_session_goes_on() {
        let root = Scratch::new();
        std::fs::create_dir_all(&root.0).unwrap();
        let fifo = root.0.join("pipe");
        assert!(std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success());
        let (ctx, mut out_rx) = sharing(&root.0);

        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            handle_local_fs_message(
                P2pMessage::FileDownloadRequest {
                    resource_id: "fs-test".to_string(),
                    file_path: "/pipe".to_string(),
                    transfer_id: uuid::Uuid::new_v4(),
                    offset: 0,
                    length: None,
                },
                ctx.clone(),
            ),
        )
        .await
        .unwrap_or_else(|_| {
            let _ = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&fifo);
            panic!("the download of a FIFO held up the session");
        });
        let answer = tokio::time::timeout(std::time::Duration::from_secs(5), out_rx.recv())
            .await
            .expect("the download went unanswered");
        assert!(
            matches!(
                &answer,
                Some(nodeinnet_p2p::OutboundP2pPayload::Message(nodeinnet_p2p::SecuredP2pEnvelope {
                    message: P2pMessage::FileTransferResponse {
                        status: nodeinnet_p2p::FileTransferStatus::Rejected { reason },
                        ..
                    },
                    ..
                })) if reason == "Not a regular file"
            ),
            "{answer:?}"
        );

        handle_local_fs_message(
            P2pMessage::RequestEntries {
                request_id: uuid::Uuid::new_v4(),
                resource_id: "fs-test".to_string(),
                path: "/".to_string(),
            },
            ctx,
        )
        .await;
        assert!(matches!(
            messages(&mut out_rx)[..],
            [P2pMessage::EntriesResponse { .. }]
        ));
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
        let (ctx, mut out_rx) = sharing(&root.0);

        for msg in upload_of("tool", 10, &[(0, b"#!/bin/sh\n")], Some(0o6755)).1 {
            handle_local_fs_message(msg, ctx.clone()).await;
        }

        assert_eq!(receipt(&mut out_rx).await, Ok(()));
        assert_eq!(std::fs::read(root.0.join("tool")).unwrap(), b"#!/bin/sh\n");
        assert_eq!(mode_of(&root.0.join("tool")), 0o755);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_upload_over_a_file_keeps_its_mode_but_not_setuid() {
        use std::os::unix::fs::PermissionsExt;
        let root = Scratch::new();
        std::fs::create_dir_all(&root.0).unwrap();
        let modes = [
            ("deploy.sh", 0o755),
            ("shared.txt", 0o664),
            ("tool", 0o4755),
        ];
        for (name, mode) in modes {
            let path = root.0.join(name);
            std::fs::write(&path, b"old").unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        }
        let (ctx, mut out_rx) = sharing(&root.0);

        for (name, _) in modes {
            for msg in upload_of(name, 3, &[(0, b"new")], None).1 {
                handle_local_fs_message(msg, ctx.clone()).await;
            }
            assert_eq!(receipt(&mut out_rx).await, Ok(()), "{name}");
        }

        assert_eq!(std::fs::read(root.0.join("deploy.sh")).unwrap(), b"new");
        assert_eq!(mode_of(&root.0.join("deploy.sh")), 0o755);
        assert_eq!(mode_of(&root.0.join("shared.txt")), 0o664);
        assert_eq!(mode_of(&root.0.join("tool")), 0o755);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn a_peer_name_with_a_drive_is_refused() {
        let root = Scratch::new();
        std::fs::create_dir_all(&root.0).unwrap();
        let (ctx, mut out_rx) = sharing(&root.0);

        for name in ["C:x", "C:"] {
            for msg in [
                P2pMessage::CreateDirectoryRequest {
                    request_id: uuid::Uuid::new_v4(),
                    resource_id: "fs-test".to_string(),
                    parent_path: "/".to_string(),
                    dir_name: name.to_string(),
                    permissions: None,
                },
                P2pMessage::FileUploadRequest {
                    resource_id: "fs-test".to_string(),
                    target_path: "/".to_string(),
                    file_name: name.to_string(),
                    total_size: 1,
                    transfer_id: uuid::Uuid::new_v4(),
                    permissions: None,
                },
            ] {
                handle_local_fs_message(msg, ctx.clone()).await;
            }
            let answers: Vec<_> = std::iter::from_fn(|| out_rx.try_recv().ok())
                .filter_map(|payload| match payload {
                    nodeinnet_p2p::OutboundP2pPayload::Message(envelope) => Some(envelope.message),
                    _ => None,
                })
                .collect();
            assert!(
                matches!(
                    answers[..],
                    [
                        P2pMessage::CreateDirectoryResponse { result: Err(_), .. },
                        P2pMessage::FileTransferResponse {
                            status: nodeinnet_p2p::FileTransferStatus::Rejected { .. },
                            ..
                        },
                    ]
                ),
                "{name} was taken: {answers:?}"
            );
        }
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn a_peer_name_windows_would_rewrite_is_refused() {
        let root = Scratch::new();
        std::fs::create_dir_all(root.0.join("docs")).unwrap();
        std::fs::write(root.0.join("notes"), b"keep").unwrap();
        std::fs::write(root.0.join("12"), b"keep").unwrap();
        std::fs::write(root.0.join("docs").join("a.txt"), b"a").unwrap();
        let (ctx, mut out_rx) = sharing(&root.0);

        for name in [
            "12:30.log",
            "notes.",
            "notes ",
            "nul",
            "NUL .txt",
            "com1",
            "a?b",
        ] {
            for msg in [
                P2pMessage::CreateDirectoryRequest {
                    request_id: uuid::Uuid::new_v4(),
                    resource_id: "fs-test".to_string(),
                    parent_path: "/".to_string(),
                    dir_name: name.to_string(),
                    permissions: None,
                },
                P2pMessage::FileUploadRequest {
                    resource_id: "fs-test".to_string(),
                    target_path: "/".to_string(),
                    file_name: name.to_string(),
                    total_size: 1,
                    transfer_id: uuid::Uuid::new_v4(),
                    permissions: None,
                },
                P2pMessage::FileUploadRequest {
                    resource_id: "fs-test".to_string(),
                    target_path: format!("/{name}"),
                    file_name: "x.txt".to_string(),
                    total_size: 1,
                    transfer_id: uuid::Uuid::new_v4(),
                    permissions: None,
                },
                P2pMessage::RenameEntryRequest {
                    request_id: uuid::Uuid::new_v4(),
                    resource_id: "fs-test".to_string(),
                    path: "/docs/a.txt".to_string(),
                    new_path: format!("/{name}"),
                },
                P2pMessage::RenameEntryRequest {
                    request_id: uuid::Uuid::new_v4(),
                    resource_id: "fs-test".to_string(),
                    path: "/docs/a.txt".to_string(),
                    new_path: format!("/{name}/a.txt"),
                },
            ] {
                handle_local_fs_message(msg, ctx.clone()).await;
            }
            let answers = messages(&mut out_rx);
            assert!(
                matches!(
                    answers[..],
                    [
                        P2pMessage::CreateDirectoryResponse { result: Err(_), .. },
                        P2pMessage::FileTransferResponse {
                            status: nodeinnet_p2p::FileTransferStatus::Rejected { .. },
                            ..
                        },
                        P2pMessage::FileTransferResponse {
                            status: nodeinnet_p2p::FileTransferStatus::Rejected { .. },
                            ..
                        },
                        P2pMessage::RenameEntryResponse { result: Err(_), .. },
                        P2pMessage::RenameEntryResponse { result: Err(_), .. },
                    ]
                ),
                "{name} was taken: {answers:?}"
            );
        }
        assert_eq!(std::fs::read(root.0.join("notes")).unwrap(), b"keep");
        assert_eq!(std::fs::read(root.0.join("12")).unwrap(), b"keep");
        assert_eq!(std::fs::read(root.0.join("docs/a.txt")).unwrap(), b"a");
    }

    #[cfg(any(windows, target_os = "macos", target_os = "android"))]
    #[tokio::test]
    async fn a_name_this_host_takes_for_an_existing_one_is_refused() {
        let root = Scratch::new();
        std::fs::create_dir_all(root.0.join("docs")).unwrap();
        std::fs::write(root.0.join("notes.txt"), b"keep").unwrap();
        std::fs::write(root.0.join("docs").join("a.txt"), b"keep").unwrap();
        std::fs::write(root.0.join("b.txt"), b"b").unwrap();
        let (ctx, mut out_rx) = sharing(&root.0);

        for msg in [
            P2pMessage::FileUploadRequest {
                resource_id: "fs-test".to_string(),
                target_path: "/".to_string(),
                file_name: "Notes.txt".to_string(),
                total_size: 1,
                transfer_id: uuid::Uuid::new_v4(),
                permissions: None,
            },
            P2pMessage::FileUploadRequest {
                resource_id: "fs-test".to_string(),
                target_path: "/Docs".to_string(),
                file_name: "a.txt".to_string(),
                total_size: 1,
                transfer_id: uuid::Uuid::new_v4(),
                permissions: None,
            },
            P2pMessage::CreateDirectoryRequest {
                request_id: uuid::Uuid::new_v4(),
                resource_id: "fs-test".to_string(),
                parent_path: "/".to_string(),
                dir_name: "DOCS".to_string(),
                permissions: None,
            },
            P2pMessage::RenameEntryRequest {
                request_id: uuid::Uuid::new_v4(),
                resource_id: "fs-test".to_string(),
                path: "/b.txt".to_string(),
                new_path: "/NOTES.txt".to_string(),
            },
            P2pMessage::RenameEntryRequest {
                request_id: uuid::Uuid::new_v4(),
                resource_id: "fs-test".to_string(),
                path: "/b.txt".to_string(),
                new_path: "/Docs/A.txt".to_string(),
            },
        ] {
            handle_local_fs_message(msg, ctx.clone()).await;
        }

        let answers = messages(&mut out_rx);
        let refused = |answer: &P2pMessage| match answer {
            P2pMessage::FileTransferResponse {
                status: nodeinnet_p2p::FileTransferStatus::Rejected { reason },
                ..
            } => Some(reason.clone()),
            P2pMessage::CreateDirectoryResponse {
                result: Err(reason),
                ..
            }
            | P2pMessage::RenameEntryResponse {
                result: Err(reason),
                ..
            } => Some(reason.clone()),
            _ => None,
        };
        assert_eq!(
            answers.iter().map(refused).collect::<Vec<_>>(),
            [
                Some("\"Notes.txt\" already exists here as \"notes.txt\"".to_string()),
                Some("\"Docs/a.txt\" already exists here as \"docs/a.txt\"".to_string()),
                Some("\"DOCS\" already exists here as \"docs\"".to_string()),
                Some("\"NOTES.txt\" already exists here as \"notes.txt\"".to_string()),
                Some("\"Docs/A.txt\" already exists here as \"docs/a.txt\"".to_string()),
            ],
            "{answers:?}"
        );
        assert_eq!(std::fs::read(root.0.join("notes.txt")).unwrap(), b"keep");
        assert_eq!(std::fs::read(root.0.join("docs/a.txt")).unwrap(), b"keep");
        assert_eq!(std::fs::read(root.0.join("b.txt")).unwrap(), b"b");
    }

    #[cfg(any(windows, target_os = "macos", target_os = "android"))]
    #[tokio::test]
    async fn an_exact_name_a_new_name_and_a_change_of_case_go_through() {
        let root = Scratch::new();
        std::fs::create_dir_all(root.0.join("docs")).unwrap();
        std::fs::write(root.0.join("notes.txt"), b"keep").unwrap();
        std::fs::write(root.0.join("x.txt"), b"x").unwrap();
        let (ctx, mut out_rx) = sharing(&root.0);

        for msg in [
            P2pMessage::RenameEntryRequest {
                request_id: uuid::Uuid::new_v4(),
                resource_id: "fs-test".to_string(),
                path: "/notes.txt".to_string(),
                new_path: "/Notes.txt".to_string(),
            },
            P2pMessage::FileUploadRequest {
                resource_id: "fs-test".to_string(),
                target_path: "/".to_string(),
                file_name: "x.txt".to_string(),
                total_size: 1,
                transfer_id: uuid::Uuid::new_v4(),
                permissions: None,
            },
            P2pMessage::FileUploadRequest {
                resource_id: "fs-test".to_string(),
                target_path: "/Docs".to_string(),
                file_name: "new.txt".to_string(),
                total_size: 1,
                transfer_id: uuid::Uuid::new_v4(),
                permissions: None,
            },
            P2pMessage::CreateDirectoryRequest {
                request_id: uuid::Uuid::new_v4(),
                resource_id: "fs-test".to_string(),
                parent_path: "/docs".to_string(),
                dir_name: "fresh".to_string(),
                permissions: None,
            },
        ] {
            handle_local_fs_message(msg, ctx.clone()).await;
        }

        let answers = messages(&mut out_rx);
        assert!(
            matches!(
                answers[..],
                [
                    P2pMessage::RenameEntryResponse { result: Ok(()), .. },
                    P2pMessage::FileTransferResponse {
                        status: nodeinnet_p2p::FileTransferStatus::Accepted { .. },
                        ..
                    },
                    P2pMessage::FileTransferResponse {
                        status: nodeinnet_p2p::FileTransferStatus::Accepted { .. },
                        ..
                    },
                    P2pMessage::CreateDirectoryResponse { result: Ok(()), .. },
                ]
            ),
            "{answers:?}"
        );
        assert_eq!(std::fs::read(root.0.join("Notes.txt")).unwrap(), b"keep");
    }

    #[cfg(any(windows, target_os = "macos", target_os = "android"))]
    #[tokio::test]
    async fn a_name_that_lands_as_an_open_upload_is_refused() {
        let root = Scratch::new();
        std::fs::create_dir_all(&root.0).unwrap();
        std::fs::write(root.0.join("x.txt"), b"x").unwrap();
        let (ctx, mut out_rx) = sharing(&root.0);
        let (first, upload) = upload_of("Makefile", 5, &[(0, b"first")], None);
        let (second, later) = upload_of("makefile", 5, &[(0, b"later")], None);

        for msg in [
            upload[0].clone(),
            later[0].clone(),
            P2pMessage::RenameEntryRequest {
                request_id: uuid::Uuid::new_v4(),
                resource_id: "fs-test".to_string(),
                path: "/x.txt".to_string(),
                new_path: "/MAKEFILE".to_string(),
            },
            upload[1].clone(),
            later[1].clone(),
            later[2].clone(),
            upload[2].clone(),
        ] {
            handle_local_fs_message(msg, ctx.clone()).await;
        }

        assert!(matches!(
            next_answer(&mut out_rx).await,
            P2pMessage::FileTransferResponse {
                status: nodeinnet_p2p::FileTransferStatus::Accepted { .. },
                ..
            }
        ));
        let refused = next_answer(&mut out_rx).await;
        assert!(
            matches!(
                &refused,
                P2pMessage::FileTransferResponse {
                    status: nodeinnet_p2p::FileTransferStatus::Rejected { reason },
                    ..
                } if reason == "\"makefile\" already exists here as \"Makefile\""
            ),
            "{refused:?}"
        );
        let renamed = next_answer(&mut out_rx).await;
        assert!(
            matches!(
                &renamed,
                P2pMessage::RenameEntryResponse { result: Err(reason), .. }
                    if reason == "\"MAKEFILE\" already exists here as \"Makefile\""
            ),
            "{renamed:?}"
        );
        let mut receipts = Vec::new();
        while receipts.len() < 2 {
            if let P2pMessage::FileTransferReceipt {
                transfer_id,
                result,
            } = next_answer(&mut out_rx).await
            {
                receipts.push((transfer_id, result));
            }
        }
        receipts.sort();
        let mut expected = vec![
            (first, Ok(())),
            (second, Err(format!("no upload {second} is open here"))),
        ];
        expected.sort();
        assert_eq!(receipts, expected);
        assert_eq!(names_in(&root.0), ["Makefile", "x.txt"]);
        assert_eq!(std::fs::read(root.0.join("Makefile")).unwrap(), b"first");
        assert_eq!(std::fs::read(root.0.join("x.txt")).unwrap(), b"x");
    }

    #[test]
    fn a_reserved_name_turns_away_only_the_names_that_land_as_it() {
        let dir = std::env::temp_dir().join(format!("p2p-handlers-{}", uuid::Uuid::new_v4()));
        let held = reserve(&dir.join("Makefile"), Comparison::Windows).unwrap();
        assert_eq!(
            reserve(&dir.join("makefile"), Comparison::Windows).err(),
            Some(dir.join("Makefile"))
        );
        assert_eq!(
            reserve(&dir.join("MAKEFILE"), Comparison::MacOs).err(),
            Some(dir.join("Makefile"))
        );
        assert!(reserve(&dir.join("makefile"), Comparison::Exact).is_ok());
        assert!(reserve(&dir.join("sub").join("makefile"), Comparison::Windows).is_ok());
        let again = reserve(&dir.join("Makefile"), Comparison::Windows).unwrap();
        drop(held);
        assert!(reserve(&dir.join("makefile"), Comparison::Windows).is_err());
        drop(again);
        assert!(reserve(&dir.join("makefile"), Comparison::Windows).is_ok());

        let _composed = reserve(&dir.join("caf\u{e9}"), Comparison::MacOs).unwrap();
        assert!(reserve(&dir.join("cafe\u{301}"), Comparison::MacOs).is_err());
        assert!(reserve(&dir.join("cafe\u{301}"), Comparison::Windows).is_ok());
    }

    #[cfg(any(unix, windows))]
    #[tokio::test]
    async fn an_upload_holds_its_name_until_it_is_stored_or_dropped() {
        let root = Scratch::new();
        std::fs::create_dir_all(&root.0).unwrap();
        let (ctx, mut out_rx) = sharing(&root.0);
        let held =
            |name: &str| taken_by(&reserved(), &root.0.join(name), Comparison::Windows).cloned();
        let (stored, stored_upload) = upload_of("Stored", 2, &[(0, b"ok")], None);
        let (cut, cut_upload) = upload_of("Cut", 2, &[(0, b"o")], None);
        let (_, open_upload) = upload_of("Open", 2, &[], None);

        for msg in [&stored_upload[0], &cut_upload[0], &open_upload[0]] {
            handle_local_fs_message(msg.clone(), ctx.clone()).await;
        }
        assert_eq!(held("STORED"), Some(root.0.join("Stored")));
        assert_eq!(held("CUT"), Some(root.0.join("Cut")));
        assert_eq!(held("OPEN"), Some(root.0.join("Open")));

        for msg in stored_upload[1..].iter().chain(&cut_upload[1..]) {
            handle_local_fs_message(msg.clone(), ctx.clone()).await;
        }
        let mut receipts = Vec::new();
        while receipts.len() < 2 {
            if let P2pMessage::FileTransferReceipt {
                transfer_id,
                result,
            } = next_answer(&mut out_rx).await
            {
                receipts.push((transfer_id, result.is_ok()));
            }
        }
        receipts.sort();
        let mut expected = vec![(stored, true), (cut, false)];
        expected.sort();
        assert_eq!(receipts, expected);
        assert_eq!((held("STORED"), held("CUT")), (None, None));
        assert_eq!(held("OPEN"), Some(root.0.join("Open")));

        ctx.shutdown().await;
        assert_eq!(held("OPEN"), None);
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
            let _ = std::fs::remove_file(self.0.with_extension("part"));
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
    async fn an_empty_download_leaves_an_empty_file() {
        let root = Scratch::new();
        let (ctx, _out_rx) = sharing(&root.0);
        let transfer_id = uuid::Uuid::new_v4();
        let file = Downloaded::of(transfer_id);
        ctx.requested_downloads.lock().await.insert(transfer_id);

        deliver_download(&ctx, transfer_id, 0, &[]).await;

        assert_eq!(std::fs::read(&file.0).unwrap(), b"");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_download_that_got_none_of_its_bytes_leaves_no_file() {
        let root = Scratch::new();
        let (ctx, _out_rx) = sharing(&root.0);
        let transfer_id = uuid::Uuid::new_v4();
        let file = Downloaded::of(transfer_id);
        ctx.requested_downloads.lock().await.insert(transfer_id);

        deliver_download(&ctx, transfer_id, 6, &[]).await;

        assert!(
            !file.0.exists(),
            "a download that got none of its bytes passed for an empty file"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_download_cut_short_leaves_no_file() {
        let root = Scratch::new();
        let (ctx, _out_rx) = sharing(&root.0);
        let transfer_id = uuid::Uuid::new_v4();
        let file = Downloaded::of(transfer_id);
        ctx.requested_downloads.lock().await.insert(transfer_id);

        for msg in [
            P2pMessage::FileTransferResponse {
                transfer_id,
                status: nodeinnet_p2p::FileTransferStatus::Accepted { total_size: 6 },
            },
            P2pMessage::FileChunk {
                transfer_id,
                offset: 0,
                data: b"hi ".to_vec(),
            },
        ] {
            handle_local_fs_message(msg, ctx.clone()).await;
        }
        assert!(
            !file.0.exists(),
            "a reader woken by FileTransferComplete would take 3 of the 6 bytes"
        );
        handle_local_fs_message(
            P2pMessage::FileTransferComplete {
                transfer_id,
                checksum: None,
            },
            ctx.clone(),
        )
        .await;

        assert!(!file.0.exists());
        assert!(!file.0.with_extension("part").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn repeated_chunks_do_not_make_up_for_missing_bytes() {
        let root = Scratch::new();
        let (ctx, _out_rx) = sharing(&root.0);
        let short = uuid::Uuid::new_v4();
        let whole = uuid::Uuid::new_v4();
        let files = [short, whole].map(Downloaded::of);
        ctx.requested_downloads.lock().await.extend([short, whole]);

        deliver_download(
            &ctx,
            short,
            6,
            &[(0, b"hi "), (0, b"hi "), (1, b"i b"), (5, b"b")],
        )
        .await;
        deliver_download(&ctx, whole, 6, &[(0, b"hi "), (1, b"i "), (2, b" bob")]).await;

        assert!(!files[0].0.exists());
        assert!(!files[0].0.with_extension("part").exists());
        assert_eq!(std::fs::read(&files[1].0).unwrap(), b"hi bob");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn serving_an_empty_file_accepts_zero_bytes_and_completes() {
        let root = Scratch::new();
        std::fs::create_dir_all(&root.0).unwrap();
        std::fs::write(root.0.join("empty"), b"").unwrap();
        let (ctx, mut out_rx) = sharing(&root.0);

        handle_local_fs_message(
            P2pMessage::FileDownloadRequest {
                resource_id: "fs-test".to_string(),
                file_path: "/empty".to_string(),
                transfer_id: uuid::Uuid::new_v4(),
                offset: 0,
                length: None,
            },
            ctx,
        )
        .await;

        let mut accepted = None;
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
                    status: nodeinnet_p2p::FileTransferStatus::Accepted { total_size },
                    ..
                } => accepted = Some(total_size),
                P2pMessage::FileTransferComplete { .. } => break,
                other => panic!("unexpected answer to the download: {other:?}"),
            }
        }
        assert_eq!(accepted, Some(0));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_share_upload_cannot_grow_past_its_announced_size() {
        let root = Scratch::new();
        std::fs::create_dir_all(&root.0).unwrap();
        let (ctx, mut out_rx) = sharing(&root.0);
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

        assert_eq!(
            receipt(&mut out_rx).await,
            Err("more than the 6 bytes announced".to_string())
        );
        assert_eq!(std::fs::read_dir(&root.0).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_upload_is_confirmed_only_once_its_name_holds_every_byte() {
        let root = Scratch::new();
        std::fs::create_dir_all(&root.0).unwrap();
        let (ctx, mut out_rx) = sharing(&root.0);
        let (_, mut upload) = upload_of("up.txt", 6, &[(0, b"hi "), (3, b"bob")], None);
        let complete = upload.pop().unwrap();

        for msg in upload {
            handle_local_fs_message(msg, ctx.clone()).await;
        }
        assert!(matches!(
            next_answer(&mut out_rx).await,
            P2pMessage::FileTransferResponse {
                status: nodeinnet_p2p::FileTransferStatus::Accepted { .. },
                ..
            }
        ));
        tokio::task::yield_now().await;
        assert!(messages(&mut out_rx).is_empty());
        assert!(!root.0.join("up.txt").exists());

        handle_local_fs_message(complete, ctx).await;
        assert_eq!(receipt(&mut out_rx).await, Ok(()));
        assert_eq!(std::fs::read(root.0.join("up.txt")).unwrap(), b"hi bob");
        assert_eq!(names_in(&root.0), ["up.txt"]);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_empty_upload_is_confirmed() {
        let root = Scratch::new();
        std::fs::create_dir_all(&root.0).unwrap();
        let (ctx, mut out_rx) = sharing(&root.0);

        for msg in upload_of("empty.txt", 0, &[], None).1 {
            handle_local_fs_message(msg, ctx.clone()).await;
        }

        assert_eq!(receipt(&mut out_rx).await, Ok(()));
        assert_eq!(std::fs::read(root.0.join("empty.txt")).unwrap(), b"");
        assert_eq!(names_in(&root.0), ["empty.txt"]);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_upload_cut_short_is_refused_and_leaves_no_file() {
        let root = Scratch::new();
        std::fs::create_dir_all(&root.0).unwrap();
        std::fs::write(root.0.join("up.txt"), b"old").unwrap();
        let (ctx, mut out_rx) = sharing(&root.0);

        for msg in upload_of("up.txt", 6, &[(0, b"hi ")], None).1 {
            handle_local_fs_message(msg, ctx.clone()).await;
        }

        assert_eq!(
            receipt(&mut out_rx).await,
            Err("ended after 3 of 6 bytes".to_string())
        );
        assert_eq!(names_in(&root.0), ["up.txt"]);
        assert_eq!(std::fs::read(root.0.join("up.txt")).unwrap(), b"old");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_gap_in_an_upload_is_refused_at_once() {
        let root = Scratch::new();
        std::fs::create_dir_all(&root.0).unwrap();
        let (ctx, mut out_rx) = sharing(&root.0);
        let (transfer_id, mut upload) = upload_of("up.txt", 9, &[(0, b"abc"), (6, b"ghi")], None);
        let complete = upload.pop().unwrap();

        for msg in upload {
            handle_local_fs_message(msg, ctx.clone()).await;
        }
        assert_eq!(
            receipt(&mut out_rx).await,
            Err("bytes 3..6 never arrived".to_string())
        );
        assert!(names_in(&root.0).is_empty());

        handle_local_fs_message(complete, ctx).await;
        assert_eq!(
            receipt(&mut out_rx).await,
            Err(format!("no upload {transfer_id} is open here"))
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_end_of_an_unknown_upload_is_refused() {
        let root = Scratch::new();
        std::fs::create_dir_all(&root.0).unwrap();
        let (ctx, mut out_rx) = sharing(&root.0);
        let transfer_id = uuid::Uuid::new_v4();

        handle_local_fs_message(
            P2pMessage::FileTransferComplete {
                transfer_id,
                checksum: None,
            },
            ctx,
        )
        .await;

        assert_eq!(
            receipt(&mut out_rx).await,
            Err(format!("no upload {transfer_id} is open here"))
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_end_of_a_refused_upload_is_refused_too() {
        let root = Scratch::new();
        std::fs::create_dir_all(root.0.join("docs")).unwrap();
        let (ctx, mut out_rx) = sharing(&root.0);

        for name in ["../x", "docs"] {
            let (transfer_id, upload) = upload_of(name, 1, &[(0, b"x")], None);
            for msg in upload {
                handle_local_fs_message(msg, ctx.clone()).await;
            }
            assert!(
                matches!(
                    next_answer(&mut out_rx).await,
                    P2pMessage::FileTransferResponse {
                        status: nodeinnet_p2p::FileTransferStatus::Rejected { .. },
                        ..
                    }
                ),
                "{name}"
            );
            assert_eq!(
                receipt(&mut out_rx).await,
                Err(format!("no upload {transfer_id} is open here"))
            );
        }
        assert_eq!(names_in(&root.0), ["docs"]);
        assert_eq!(std::fs::read_dir(root.0.join("docs")).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_end_of_a_session_deletes_what_an_open_upload_wrote() {
        let root = Scratch::new();
        std::fs::create_dir_all(&root.0).unwrap();
        let (ctx, mut out_rx) = sharing(&root.0);
        let (transfer_id, mut upload) = upload_of("up.txt", 6, &[(0, b"hi ")], None);
        let complete = upload.pop().unwrap();

        for msg in upload {
            handle_local_fs_message(msg, ctx.clone()).await;
        }
        assert_eq!(names_in(&root.0), [format!(".~{transfer_id}.part")]);
        ctx.shutdown().await;
        assert!(names_in(&root.0).is_empty());

        handle_local_fs_message(complete, ctx).await;
        assert_eq!(
            receipt(&mut out_rx).await,
            Err(format!("no upload {transfer_id} is open here"))
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_end_of_a_session_keeps_an_upload_already_being_stored() {
        let root = Scratch::new();
        std::fs::create_dir_all(&root.0).unwrap();
        let (ctx, mut out_rx) = sharing(&root.0);

        for msg in upload_of("up.txt", 6, &[(0, b"hi bob")], None).1 {
            handle_local_fs_message(msg, ctx.clone()).await;
        }
        ctx.shutdown().await;

        assert_eq!(receipt(&mut out_rx).await, Ok(()));
        assert_eq!(std::fs::read(root.0.join("up.txt")).unwrap(), b"hi bob");
        assert_eq!(names_in(&root.0), ["up.txt"]);
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_write_is_answered_with_the_systems_reason() {
        let Some(dir) = std::env::var_os("P2P_HANDLERS_FSIZE") else {
            let root = Scratch::new();
            std::fs::create_dir_all(&root.0).unwrap();
            let run = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "fs_local::tests::a_failed_write_is_answered_with_the_systems_reason",
                ])
                .env("P2P_HANDLERS_FSIZE", &root.0)
                .output()
                .unwrap();
            assert!(
                run.status.success(),
                "{}{}",
                String::from_utf8_lossy(&run.stdout),
                String::from_utf8_lossy(&run.stderr)
            );
            return;
        };
        let root = PathBuf::from(dir);
        unsafe {
            let mut limit = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            assert_eq!(libc::getrlimit(libc::RLIMIT_FSIZE, &mut limit), 0);
            limit.rlim_cur = 4096;
            assert_eq!(libc::setrlimit(libc::RLIMIT_FSIZE, &limit), 0);
            libc::signal(libc::SIGXFSZ, libc::SIG_IGN);
        }
        let data = vec![7u8; 16 * 1024];
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let (ctx, mut out_rx) = sharing(&root);
                for msg in upload_of("big.bin", data.len() as u64, &[(0, &data)], None).1 {
                    handle_local_fs_message(msg, ctx.clone()).await;
                }
                let reason = receipt(&mut out_rx).await.unwrap_err();
                assert!(reason.contains("File too large"), "{reason}");
                assert!(names_in(&root).is_empty());
            });
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
