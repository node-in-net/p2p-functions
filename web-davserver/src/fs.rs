use crate::{DavDrive, WebDavSession};
use bytes::{Buf, Bytes};
use dav_server::davpath::DavPath;
use dav_server::fs::{
    DavDirEntry, DavFile, DavFileSystem, DavMetaData, FsError, FsFuture, FsStream, OpenOptions,
    ReadDirMeta,
};
use futures_util::future::{self, FutureExt};
use std::fmt::Debug;
use std::io::{Cursor, Read, Seek, SeekFrom, Write};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

#[derive(Clone)]
pub struct NodeInNetDavFs {
    pub session: Arc<Mutex<WebDavSession>>,
    pub p2p_tx: tokio::sync::mpsc::Sender<nodeinnet_p2p::P2pMessage>,
}

impl NodeInNetDavFs {
    pub fn new(
        session: Arc<Mutex<WebDavSession>>,
        p2p_tx: tokio::sync::mpsc::Sender<nodeinnet_p2p::P2pMessage>,
    ) -> Self {
        Self { session, p2p_tx }
    }

    fn get_drive_and_path(&self, path: &DavPath) -> Option<(Arc<Mutex<DavDrive>>, String)> {
        let session = self.session.lock().unwrap();
        if let Some(drive) = session.drives.values().next() {
            let sub_path = path.as_url_string();
            let sub_path = if sub_path.is_empty() {
                "/".to_string()
            } else {
                sub_path
            };
            Some((drive.clone(), sub_path))
        } else {
            None
        }
    }

    fn is_ignored_junk(path_str: &str) -> bool {
        let name = path_str.split('/').next_back().unwrap_or("");
        name == ".DS_Store"
            || name.starts_with("._")
            || name == ".Spotlight-V100"
            || name == ".Trashes"
            || name == ".fseventsd"
            || name == "Thumbs.db"
            || name == "desktop.ini"
            || name == "autorun.inf"
    }
}

impl DavFileSystem for NodeInNetDavFs {
    fn open<'a>(
        &'a self,
        path: &'a DavPath,
        options: OpenOptions,
    ) -> FsFuture<'a, Box<dyn DavFile>> {
        let this = self.clone();
        let path = path.clone();
        async move {
            let path_str = path.as_url_string();
            if path_str.ends_with(".metadata_never_index") {
                return Ok(Box::new(BlackHoleDavFile {}) as Box<dyn DavFile>);
            }

            if let Some((drive, sub_path)) = this.get_drive_and_path(&path) {
                if Self::is_ignored_junk(&sub_path) {
                    if options.write {
                        return Ok(Box::new(BlackHoleDavFile {}) as Box<dyn DavFile>);
                    }
                    return Err(FsError::NotFound);
                }

                // Single expression: the MutexGuard is created and dropped immediately, never captured in the Future state
                let resource_id = drive.lock().unwrap().id.clone();

                if options.read {
                    let transfer_id = uuid::Uuid::new_v4();
                    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

                    {
                        let mut pending = crate::get_pending_requests().lock().unwrap();
                        pending.insert(transfer_id, tx);
                    }
                    let _pending_guard = crate::PendingGuard(transfer_id);

                    let msg = nodeinnet_p2p::P2pMessage::FileDownloadRequest {
                        resource_id,
                        file_path: sub_path,
                        transfer_id,
                    };

                    let _ = this.p2p_tx.send(msg).await;

                    // Wait until the file is fully downloaded into the temp dir.
                    //
                    // TWO replies arrive: `FileTransferResponse{Accepted}` when the peer has
                    // opened the file, and `FileTransferComplete` when the last chunk has
                    // landed. Only the second means the temp file is whole, so the first is
                    // skipped rather than mistaken for an answer.
                    loop {
                        match rx.recv().await {
                            Some(nodeinnet_p2p::P2pMessage::FileTransferResponse {
                                status: nodeinnet_p2p::FileTransferStatus::Accepted { .. },
                                ..
                            }) => continue,
                            Some(nodeinnet_p2p::P2pMessage::FileTransferComplete { .. }) => {
                                let temp_path = std::env::temp_dir().join(transfer_id.to_string());
                                if let Ok(data) = tokio::fs::read(&temp_path).await {
                                    let _ = tokio::fs::remove_file(&temp_path).await;
                                    let file = NodeInNetDavFile::new_read(data);
                                    return Ok(Box::new(file) as Box<dyn DavFile>);
                                }
                                return Err(FsError::NotFound);
                            }
                            Some(nodeinnet_p2p::P2pMessage::FileTransferResponse {
                                status: nodeinnet_p2p::FileTransferStatus::Rejected { .. },
                                ..
                            }) => {
                                return Err(FsError::Forbidden);
                            }
                            _ => return Err(FsError::NotFound),
                        }
                    }
                }

                // Write (create or overwrite)
                if options.write {
                    let name = path.file_name().unwrap_or("").as_bytes().to_vec();
                    let file = NodeInNetDavFile::new_write(
                        drive.clone(),
                        sub_path,
                        name,
                        this.p2p_tx.clone(),
                    );
                    return Ok(Box::new(file) as Box<dyn DavFile>);
                }

                Err(FsError::NotImplemented)
            } else {
                Err(FsError::NotFound)
            }
        }
        .boxed()
    }

    fn read_dir<'a>(
        &'a self,
        path: &'a DavPath,
        _meta: ReadDirMeta,
    ) -> FsFuture<'a, FsStream<Box<dyn DavDirEntry>>> {
        let this = self.clone();
        let path = path.clone();
        async move {
            if let Some((drive, sub_path)) = this.get_drive_and_path(&path) {
                if NodeInNetDavFs::is_ignored_junk(&sub_path) {
                    return Err(FsError::NotFound);
                }

                let resource_id = drive.lock().unwrap().id.clone();
                let cache_key = (resource_id.clone(), sub_path.clone());

                {
                    let mut cache = crate::get_dir_cache().lock().unwrap();
                    if let Some(entry) = cache.get(&cache_key) {
                        if entry.expires_at > std::time::Instant::now() {
                            let mut dir_entries = Vec::new();
                            for e in &entry.data {
                                dir_entries.push(Box::new(NodeInNetDirEntry {
                                    name: e.name.clone(),
                                    is_dir: e.entry_type == "Directory",
                                    size: e.size,
                                    modified: SystemTime::now(),
                                })
                                    as Box<dyn DavDirEntry>);
                            }
                            let stream = futures_util::stream::iter(dir_entries);
                            return Ok(Box::pin(stream) as FsStream<Box<dyn DavDirEntry>>);
                        } else {
                            cache.remove(&cache_key);
                        }
                    }
                }

                let request_id = uuid::Uuid::new_v4();
                let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

                {
                    let mut pending = crate::get_pending_requests().lock().unwrap();
                    pending.insert(request_id, tx);
                }
                let _pending_guard = crate::PendingGuard(request_id);

                let msg = nodeinnet_p2p::P2pMessage::RequestEntries {
                    request_id,
                    resource_id: resource_id.clone(),
                    path: sub_path.clone(),
                };

                let _ = this.p2p_tx.send(msg).await;

                match rx.recv().await {
                    Some(nodeinnet_p2p::P2pMessage::EntriesResponse {
                        directories,
                        mut files,
                        ..
                    }) => {
                        // Spotlight opt-out marker: keeps macOS from indexing the mount
                        if sub_path == "/" {
                            files.push((".metadata_never_index".to_string(), 0));
                        }

                        let mut entries = Vec::new();
                        for d in directories {
                            entries.push(nodeinnet_p2p::EntryInfo {
                                name: d,
                                entry_type: "Directory".to_string(),
                                size: 0,
                            });
                        }
                        for f in files {
                            entries.push(nodeinnet_p2p::EntryInfo {
                                name: f.0,
                                entry_type: "File".to_string(),
                                size: f.1,
                            });
                        }

                        {
                            let mut dir_cache = crate::get_dir_cache().lock().unwrap();
                            dir_cache.insert(
                                cache_key.clone(),
                                crate::CachedData {
                                    data: entries.clone(),
                                    expires_at: std::time::Instant::now()
                                        + std::time::Duration::from_secs(10),
                                },
                            );

                            // CACHE PRE-WARM: store metadata for every contained file right away
                            let mut meta_cache = crate::get_meta_cache().lock().unwrap();
                            for e in &entries {
                                let entry_path = if sub_path == "/" {
                                    format!("/{}", e.name)
                                } else {
                                    format!("{}/{}", sub_path, e.name)
                                };
                                meta_cache.insert(
                                    (resource_id.clone(), entry_path),
                                    crate::CachedData {
                                        data: e.clone(),
                                        expires_at: std::time::Instant::now()
                                            + std::time::Duration::from_secs(10),
                                    },
                                );
                            }
                        }

                        let mut dir_entries = Vec::new();
                        for entry in entries {
                            dir_entries.push(Box::new(NodeInNetDirEntry {
                                name: entry.name,
                                is_dir: entry.entry_type == "Directory",
                                size: entry.size,
                                modified: SystemTime::now(),
                            })
                                as Box<dyn DavDirEntry>);
                        }
                        let stream = futures_util::stream::iter(dir_entries);
                        Ok(Box::pin(stream) as FsStream<Box<dyn DavDirEntry>>)
                    }
                    _ => Err(FsError::NotFound),
                }
            } else {
                Err(FsError::NotFound)
            }
        }
        .boxed()
    }

    fn metadata<'a>(&'a self, path: &'a DavPath) -> FsFuture<'a, Box<dyn DavMetaData>> {
        let this = self.clone();
        let path = path.clone();
        async move {
            let path_str = path.as_url_string();
            if path_str == "/" || path_str.is_empty() {
                return Ok(Box::new(NodeInNetMetaData {
                    is_dir: true,
                    size: 0,
                    modified: SystemTime::now(),
                }) as Box<dyn DavMetaData>);
            }

            if path_str.ends_with(".metadata_never_index") {
                return Ok(Box::new(NodeInNetMetaData {
                    is_dir: false,
                    size: 0,
                    modified: SystemTime::now(),
                }) as Box<dyn DavMetaData>);
            }

            if NodeInNetDavFs::is_ignored_junk(&path_str) {
                return Err(FsError::NotFound);
            }

            if let Some((drive, sub_path)) = this.get_drive_and_path(&path) {
                let resource_id = drive.lock().unwrap().id.clone();
                let cache_key = (resource_id.clone(), sub_path.clone());

                {
                    let mut cache = crate::get_meta_cache().lock().unwrap();
                    if let Some(entry) = cache.get(&cache_key) {
                        if entry.expires_at > std::time::Instant::now() {
                            return Ok(Box::new(NodeInNetMetaData {
                                is_dir: entry.data.entry_type == "Directory",
                                size: entry.data.size,
                                modified: SystemTime::now(),
                            }) as Box<dyn DavMetaData>);
                        } else {
                            cache.remove(&cache_key);
                        }
                    }
                }

                let request_id = uuid::Uuid::new_v4();
                let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

                {
                    let mut pending = crate::get_pending_requests().lock().unwrap();
                    pending.insert(request_id, tx);
                }
                let _pending_guard = crate::PendingGuard(request_id);

                let msg = nodeinnet_p2p::P2pMessage::RequestMetadata {
                    request_id,
                    resource_id,
                    path: sub_path,
                };

                let _ = this.p2p_tx.send(msg).await;

                match rx.recv().await {
                    Some(nodeinnet_p2p::P2pMessage::MetadataResponse {
                        metadata: Some(meta),
                        ..
                    }) => {
                        {
                            let mut cache = crate::get_meta_cache().lock().unwrap();
                            cache.insert(
                                cache_key,
                                crate::CachedData {
                                    data: meta.clone(),
                                    expires_at: std::time::Instant::now()
                                        + std::time::Duration::from_secs(10),
                                },
                            );
                        }

                        Ok(Box::new(NodeInNetMetaData {
                            is_dir: meta.entry_type == "Directory",
                            size: meta.size,
                            modified: SystemTime::now(), // EntryInfo carries no mtime
                        }) as Box<dyn DavMetaData>)
                    }
                    _ => Err(FsError::NotFound),
                }
            } else {
                Err(FsError::NotFound)
            }
        }
        .boxed()
    }

    fn create_dir<'a>(&'a self, path: &'a DavPath) -> FsFuture<'a, ()> {
        let this = self.clone();
        let path = path.clone();
        async move {
            if let Some((drive, sub_path)) = this.get_drive_and_path(&path) {
                if Self::is_ignored_junk(&sub_path) || sub_path.ends_with(".metadata_never_index") {
                    return Ok(()); // Pretend the create succeeded without hitting the network
                }

                let resource_id = drive.lock().unwrap().id.clone();

                let (parent_path, dir_name) = match sub_path.rfind('/') {
                    Some(idx) => (sub_path[..idx].to_string(), sub_path[idx + 1..].to_string()),
                    None => ("/".to_string(), sub_path.clone()),
                };
                let parent_path = if parent_path.is_empty() {
                    "/".to_string()
                } else {
                    parent_path
                };

                let request_id = uuid::Uuid::new_v4();
                let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
                {
                    let mut pending = crate::get_pending_requests().lock().unwrap();
                    pending.insert(request_id, tx);
                }
                let _pending_guard = crate::PendingGuard(request_id);

                let msg = nodeinnet_p2p::P2pMessage::CreateDirectoryRequest {
                    request_id,
                    resource_id,
                    parent_path,
                    dir_name,
                    permissions: None,
                };
                let _ = this.p2p_tx.send(msg).await;

                match rx.recv().await {
                    Some(nodeinnet_p2p::P2pMessage::CreateDirectoryResponse {
                        result: Ok(_),
                        ..
                    }) => Ok(()),
                    _ => Err(FsError::Forbidden),
                }
            } else {
                Err(FsError::NotFound)
            }
        }
        .boxed()
    }

    fn remove_dir<'a>(&'a self, path: &'a DavPath) -> FsFuture<'a, ()> {
        self.remove_file(path)
    }

    fn remove_file<'a>(&'a self, path: &'a DavPath) -> FsFuture<'a, ()> {
        let this = self.clone();
        let path = path.clone();
        async move {
            if let Some((drive, sub_path)) = this.get_drive_and_path(&path) {
                if Self::is_ignored_junk(&sub_path) || sub_path.ends_with(".metadata_never_index") {
                    return Ok(());
                }

                let resource_id = drive.lock().unwrap().id.clone();

                let request_id = uuid::Uuid::new_v4();
                let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
                {
                    let mut pending = crate::get_pending_requests().lock().unwrap();
                    pending.insert(request_id, tx);
                }
                let _pending_guard = crate::PendingGuard(request_id);

                let msg = nodeinnet_p2p::P2pMessage::DeleteEntryRequest {
                    request_id,
                    resource_id,
                    path: sub_path,
                };
                let _ = this.p2p_tx.send(msg).await;

                match rx.recv().await {
                    Some(nodeinnet_p2p::P2pMessage::DeleteEntryResponse {
                        result: Ok(_), ..
                    }) => Ok(()),
                    _ => Err(FsError::Forbidden),
                }
            } else {
                Err(FsError::NotFound)
            }
        }
        .boxed()
    }

    fn rename<'a>(&'a self, from: &'a DavPath, to: &'a DavPath) -> FsFuture<'a, ()> {
        let this = self.clone();
        let from = from.clone();
        let to = to.clone();
        async move {
            let from_res = this.get_drive_and_path(&from);
            let to_res = this.get_drive_and_path(&to);

            if let (Some((drive_from, path_from)), Some((drive_to, path_to))) = (from_res, to_res) {
                if NodeInNetDavFs::is_ignored_junk(&path_from)
                    || path_from.ends_with(".metadata_never_index")
                {
                    return Ok(());
                }

                if !Arc::ptr_eq(&drive_from, &drive_to) {
                    return Err(FsError::Forbidden);
                }
                let resource_id = drive_from.lock().unwrap().id.clone();

                let request_id = uuid::Uuid::new_v4();
                let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
                {
                    let mut pending = crate::get_pending_requests().lock().unwrap();
                    pending.insert(request_id, tx);
                }
                let _pending_guard = crate::PendingGuard(request_id);

                let msg = nodeinnet_p2p::P2pMessage::RenameEntryRequest {
                    request_id,
                    resource_id,
                    path: path_from,
                    new_path: path_to,
                };
                let _ = this.p2p_tx.send(msg).await;

                match rx.recv().await {
                    Some(nodeinnet_p2p::P2pMessage::RenameEntryResponse {
                        result: Ok(_), ..
                    }) => Ok(()),
                    _ => Err(FsError::Forbidden),
                }
            } else {
                Err(FsError::NotFound)
            }
        }
        .boxed()
    }
}

#[derive(Clone, Debug)]
pub struct NodeInNetDirEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified: SystemTime,
}

impl DavDirEntry for NodeInNetDirEntry {
    fn name(&self) -> Vec<u8> {
        self.name.as_bytes().to_vec()
    }
    fn metadata(&self) -> FsFuture<'_, Box<dyn DavMetaData>> {
        let meta = NodeInNetMetaData {
            is_dir: self.is_dir,
            size: self.size,
            modified: self.modified,
        };
        future::ready(Ok(Box::new(meta) as Box<dyn DavMetaData>)).boxed()
    }
}

#[derive(Clone, Debug)]
pub struct NodeInNetMetaData {
    pub is_dir: bool,
    pub size: u64,
    pub modified: SystemTime,
}

impl DavMetaData for NodeInNetMetaData {
    fn len(&self) -> u64 {
        self.size
    }
    fn modified(&self) -> dav_server::fs::FsResult<SystemTime> {
        Ok(self.modified)
    }
    fn is_dir(&self) -> bool {
        self.is_dir
    }
}

pub struct NodeInNetDavFile {
    content: Cursor<Vec<u8>>,
    drive: Option<Arc<Mutex<DavDrive>>>,
    path: String,
    _name: Vec<u8>,
    is_write: bool,
    p2p_tx: Option<tokio::sync::mpsc::Sender<nodeinnet_p2p::P2pMessage>>,
}

impl Debug for NodeInNetDavFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NodeInNetDavFile")
            .field("path", &self.path)
            .field("is_write", &self.is_write)
            .finish()
    }
}

impl NodeInNetDavFile {
    pub fn new_read(data: Vec<u8>) -> Self {
        Self {
            content: Cursor::new(data),
            drive: None,
            path: String::new(),
            _name: Vec::new(),
            is_write: false,
            p2p_tx: None,
        }
    }
    pub fn new_write(
        drive: Arc<Mutex<DavDrive>>,
        path: String,
        name: Vec<u8>,
        p2p_tx: tokio::sync::mpsc::Sender<nodeinnet_p2p::P2pMessage>,
    ) -> Self {
        Self {
            content: Cursor::new(Vec::new()),
            drive: Some(drive),
            path,
            _name: name,
            is_write: true,
            p2p_tx: Some(p2p_tx),
        }
    }
}

impl DavFile for NodeInNetDavFile {
    fn metadata(&mut self) -> FsFuture<'_, Box<dyn DavMetaData>> {
        let len = self.content.get_ref().len() as u64;
        let meta = NodeInNetMetaData {
            is_dir: false,
            size: len,
            modified: SystemTime::now(),
        };
        future::ready(Ok(Box::new(meta) as Box<dyn DavMetaData>)).boxed()
    }

    fn write_buf(&mut self, mut buf: Box<dyn Buf + Send>) -> FsFuture<'_, ()> {
        if self.is_write {
            while buf.has_remaining() {
                let chunk = buf.chunk();
                let len = chunk.len();
                let _ = self.content.write_all(chunk);
                buf.advance(len);
            }
        }
        future::ready(Ok(())).boxed()
    }

    fn read_bytes(&mut self, count: usize) -> FsFuture<'_, Bytes> {
        let mut buf = vec![0u8; count];
        let n = self.content.read(&mut buf).unwrap_or(0);
        buf.truncate(n);
        future::ready(Ok(Bytes::from(buf))).boxed()
    }
    fn write_bytes(&mut self, buf: Bytes) -> FsFuture<'_, ()> {
        if self.is_write {
            let _ = self.content.write_all(&buf);
        }
        future::ready(Ok(())).boxed()
    }
    fn seek(&mut self, pos: SeekFrom) -> FsFuture<'_, u64> {
        future::ready(self.content.seek(pos).map_err(|_| FsError::GeneralFailure)).boxed()
    }
    fn flush(&mut self) -> FsFuture<'_, ()> {
        if self.is_write && self.drive.is_some() {
            let data = std::mem::take(self.content.get_mut());
            let drive = self.drive.take().unwrap();
            let resource_id = drive.lock().unwrap().id.clone();
            let p2p_tx = self.p2p_tx.take().unwrap();

            let path_clone = self.path.clone();

            async move {
                let (parent_path, file_name) = match path_clone.rfind('/') {
                    Some(idx) => (
                        path_clone[..idx].to_string(),
                        path_clone[idx + 1..].to_string(),
                    ),
                    None => ("/".to_string(), path_clone.clone()),
                };
                let parent_path = if parent_path.is_empty() {
                    "/".to_string()
                } else {
                    parent_path
                };
                let transfer_id = uuid::Uuid::new_v4();

                let req = nodeinnet_p2p::P2pMessage::FileUploadRequest {
                    resource_id,
                    target_path: parent_path,
                    file_name,
                    total_size: data.len() as u64,
                    transfer_id,
                    permissions: None,
                };
                // Wait to be told the transfer exists instead of guessing how
                // long that takes. The old fixed 200 ms was both a race — chunks
                // arriving before the peer registered `transfer_id` are dropped
                // without a word — and a toll paid on every single file.
                let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
                {
                    let mut pending = crate::get_pending_requests().lock().unwrap();
                    pending.insert(transfer_id, tx);
                }
                let _pending_guard = crate::PendingGuard(transfer_id);

                let _ = p2p_tx.send(req).await;

                match rx.recv().await {
                    Some(nodeinnet_p2p::P2pMessage::FileTransferResponse {
                        status: nodeinnet_p2p::FileTransferStatus::Accepted { .. },
                        ..
                    }) => {}
                    Some(nodeinnet_p2p::P2pMessage::FileTransferResponse {
                        status: nodeinnet_p2p::FileTransferStatus::Rejected { .. },
                        ..
                    }) => return Err(FsError::Forbidden),
                    _ => return Err(FsError::GeneralFailure),
                }

                if !data.is_empty() {
                    let chunk_size = 16 * 1024;
                    let mut offset = 0;
                    for chunk_data in data.chunks(chunk_size) {
                        let chunk = nodeinnet_p2p::P2pMessage::FileChunk {
                            transfer_id,
                            offset: offset as u64,
                            data: chunk_data.to_vec(),
                        };
                        // `send` on a bounded queue waits for room, and the
                        // transport below paces itself off the SCTP buffer. A
                        // fixed per-chunk delay on top of that only cost time:
                        // 2 ms per 16 KiB is about two minutes per gigabyte.
                        let _ = p2p_tx.send(chunk).await;
                        offset += chunk_data.len();
                    }
                }

                let complete = nodeinnet_p2p::P2pMessage::FileTransferComplete {
                    transfer_id,
                    checksum: None,
                };
                let _ = p2p_tx.send(complete).await;

                Ok(())
            }
            .boxed()
        } else {
            future::ready(Ok(())).boxed()
        }
    }
}

#[derive(Debug)]
pub struct BlackHoleDavFile {}

impl DavFile for BlackHoleDavFile {
    fn metadata(&mut self) -> FsFuture<'_, Box<dyn DavMetaData>> {
        future::ready(Ok(Box::new(NodeInNetMetaData {
            is_dir: false,
            size: 0,
            modified: SystemTime::now(),
        }) as Box<dyn DavMetaData>))
        .boxed()
    }

    fn write_buf(&mut self, _buf: Box<dyn Buf + Send>) -> FsFuture<'_, ()> {
        future::ready(Ok(())).boxed()
    }

    fn write_bytes(&mut self, _buf: Bytes) -> FsFuture<'_, ()> {
        future::ready(Ok(())).boxed()
    }

    fn read_bytes(&mut self, _count: usize) -> FsFuture<'_, Bytes> {
        future::ready(Ok(Bytes::new())).boxed()
    }

    fn seek(&mut self, _pos: SeekFrom) -> FsFuture<'_, u64> {
        future::ready(Ok(0)).boxed()
    }

    fn flush(&mut self) -> FsFuture<'_, ()> {
        future::ready(Ok(())).boxed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dav_server::fs::DavMetaData;

    // --- is_ignored_junk ---

    #[test]
    fn ds_store_is_junk() {
        assert!(NodeInNetDavFs::is_ignored_junk("/Volumes/share/.DS_Store"));
    }

    #[test]
    fn dot_underscore_prefix_is_junk() {
        assert!(NodeInNetDavFs::is_ignored_junk("/share/._thumbnails"));
    }

    #[test]
    fn thumbs_db_is_junk() {
        assert!(NodeInNetDavFs::is_ignored_junk("C:/share/Thumbs.db"));
    }

    #[test]
    fn desktop_ini_is_junk() {
        assert!(NodeInNetDavFs::is_ignored_junk("/share/desktop.ini"));
    }

    #[test]
    fn regular_file_is_not_junk() {
        assert!(!NodeInNetDavFs::is_ignored_junk("/share/document.pdf"));
    }

    #[test]
    fn regular_dir_is_not_junk() {
        assert!(!NodeInNetDavFs::is_ignored_junk("/share/projects"));
    }

    #[test]
    fn autorun_inf_is_junk() {
        assert!(NodeInNetDavFs::is_ignored_junk("/share/autorun.inf"));
    }

    // --- NodeInNetMetaData ---

    #[test]
    fn metadata_len_returns_size() {
        let meta = NodeInNetMetaData {
            is_dir: false,
            size: 1024,
            modified: SystemTime::now(),
        };
        assert_eq!(meta.len(), 1024);
    }

    #[test]
    fn metadata_is_dir_true() {
        let meta = NodeInNetMetaData {
            is_dir: true,
            size: 0,
            modified: SystemTime::now(),
        };
        assert!(meta.is_dir());
    }

    #[test]
    fn metadata_is_dir_false() {
        let meta = NodeInNetMetaData {
            is_dir: false,
            size: 42,
            modified: SystemTime::now(),
        };
        assert!(!meta.is_dir());
    }

    #[test]
    fn metadata_modified_returns_ok() {
        let now = SystemTime::now();
        let meta = NodeInNetMetaData {
            is_dir: false,
            size: 0,
            modified: now,
        };
        assert!(meta.modified().is_ok());
    }

    // --- NodeInNetDavFile ---

    #[test]
    fn new_read_sets_is_write_false() {
        let f = NodeInNetDavFile::new_read(b"hello".to_vec());
        assert!(!f.is_write);
    }

    #[test]
    fn new_read_stores_data_in_cursor() {
        let data = b"test content".to_vec();
        let f = NodeInNetDavFile::new_read(data.clone());
        assert_eq!(f.content.get_ref(), &data);
    }
}
