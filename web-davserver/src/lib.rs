pub mod auth;
pub mod fs;

use crate::fs::NodeInNetDavFs;
use actix_web::{web, App, Error, HttpRequest, HttpResponse, HttpServer};
use actix_web_httpauth::middleware::HttpAuthentication;
use dav_server::memls::MemLs;
use dav_server::{body::Body, DavHandler};
use futures_util::StreamExt;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;
use tokio::sync::mpsc;
use uuid::Uuid;

/// Links a request to the reply (or replies) the peer sends back for it.  An UNBOUNDED.
pub fn get_pending_requests(
) -> &'static Mutex<HashMap<Uuid, mpsc::UnboundedSender<nodeinnet_p2p::P2pMessage>>> {
    static PENDING: OnceLock<
        Mutex<HashMap<Uuid, mpsc::UnboundedSender<nodeinnet_p2p::P2pMessage>>>,
    > = OnceLock::new();
    PENDING.get_or_init(|| Mutex::new(HashMap::new()))
}

pub struct PendingGuard(pub Uuid);

impl Drop for PendingGuard {
    fn drop(&mut self) {
        if let Ok(mut pending) = get_pending_requests().lock() {
            pending.remove(&self.0);
        }
    }
}

pub struct CachedData<T> {
    pub data: T,
    pub expires_at: Instant,
}

pub type DirCache = Mutex<HashMap<(String, String), CachedData<Vec<nodeinnet_p2p::EntryInfo>>>>;
pub type MetaCache = Mutex<HashMap<(String, String), CachedData<nodeinnet_p2p::EntryInfo>>>;

pub fn get_dir_cache() -> &'static DirCache {
    static DIR_CACHE: OnceLock<DirCache> = OnceLock::new();
    DIR_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn get_meta_cache() -> &'static MetaCache {
    static META_CACHE: OnceLock<MetaCache> = OnceLock::new();
    META_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn clear_p2p_caches() {
    get_dir_cache().lock().unwrap().clear();
    get_meta_cache().lock().unwrap().clear();
}

#[derive(Clone)]
pub enum FsNode {
    File(Vec<u8>),
    Directory,
}

pub struct DavDrive {
    pub nodes: HashMap<String, FsNode>,
    pub id: String,
}

impl DavDrive {
    pub fn new(id: String) -> Self {
        let mut nodes = HashMap::new();
        nodes.insert("/".to_string(), FsNode::Directory);
        Self { nodes, id }
    }
}

pub struct WebDavSession {
    pub username: String,
    pub last_access: Instant,
    pub drives: HashMap<String, Arc<Mutex<DavDrive>>>,
}

async fn handle_dav_request(
    req: HttpRequest,
    body: web::Payload,
    handler: DavHandler,
) -> HttpResponse {
    let mut builder = http::Request::builder()
        .method(req.method())
        .uri(req.path())
        .version(req.version());

    for (name, value) in req.headers() {
        builder = builder.header(name, value);
    }

    let bytes = body
        .fold(bytes::BytesMut::new(), |mut acc, chunk| async move {
            if let Ok(chunk) = chunk {
                acc.extend_from_slice(&chunk);
            }
            acc
        })
        .await;
    let dav_body = Body::from(bytes.freeze());
    let dav_req = builder.body(dav_body).unwrap();

    let dav_resp = handler.handle(dav_req).await;

    let mut resp = HttpResponse::build(dav_resp.status());
    for (name, value) in dav_resp.headers() {
        resp.append_header((name, value));
    }

    let stream = dav_resp.into_body().map(|b| b.map_err(Error::from));
    resp.streaming(stream)
}

#[derive(Clone, Default)]
pub struct MountPrefix(pub String);

pub async fn local_handler(
    req: HttpRequest,
    session: web::Data<Arc<Mutex<WebDavSession>>>,
    p2p_tx: web::Data<tokio::sync::mpsc::Sender<nodeinnet_p2p::P2pMessage>>,
    prefix: web::Data<MountPrefix>,
    body: web::Payload,
) -> HttpResponse {
    let prefix = &prefix.get_ref().0;
    if !prefix.is_empty() && !path_is_under(req.path(), prefix) {
        return HttpResponse::NotFound().finish();
    }

    let fs = NodeInNetDavFs::new(session.get_ref().clone(), p2p_tx.get_ref().clone());
    let ls = MemLs::new();

    let handler = DavHandler::builder()
        .filesystem(Box::new(fs))
        .locksystem(ls)
        .strip_prefix(if prefix.is_empty() { "/" } else { prefix })
        .build_handler();

    handle_dav_request(req, body, handler).await
}

fn path_is_under(path: &str, prefix: &str) -> bool {
    path == prefix || path.starts_with(&format!("{prefix}/"))
}


struct MountedServer {
    handle: actix_web::dev::ServerHandle,
    port: u16,
}

fn mounts() -> &'static Mutex<HashMap<String, MountedServer>> {
    static MOUNTS: OnceLock<Mutex<HashMap<String, MountedServer>>> = OnceLock::new();
    MOUNTS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn free_port() -> Option<u16> {
    (8001..9000).find(|p| std::net::TcpListener::bind(("127.0.0.1", *p)).is_ok())
}

/// What this process mounted, keyed by port: a drive letter on Windows, a `/Volumes`.
fn mapped_drives() -> &'static Mutex<HashMap<u16, String>> {
    static DRIVES: OnceLock<Mutex<HashMap<u16, String>>> = OnceLock::new();
    DRIVES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// A free drive letter, from Z: down. Ones we already picked are skipped: `net use` has.
fn free_drive_letter(taken: &HashMap<u16, String>) -> Option<String> {
    ('D'..='Z').rev().map(|c| format!("{c}:")).find(|d| {
        !taken.values().any(|t| t == d) && !std::path::Path::new(&format!("{d}\\")).exists()
    })
}

pub fn feed_response(msg: nodeinnet_p2p::P2pMessage) -> Option<nodeinnet_p2p::P2pMessage> {
    use nodeinnet_p2p::P2pMessage as M;
    let id = match &msg {
        M::EntriesResponse { request_id, .. }
        | M::MetadataResponse { request_id, .. }
        | M::CreateDirectoryResponse { request_id, .. }
        | M::DeleteEntryResponse { request_id, .. }
        | M::RenameEntryResponse { request_id, .. }
        | M::SetPermissionsResponse { request_id, .. } => *request_id,
        M::FileTransferResponse { transfer_id, .. } | M::FileTransferComplete { transfer_id, .. } => {
            *transfer_id
        }
        _ => return Some(msg),
    };
    let tx = get_pending_requests().lock().unwrap().get(&id).cloned();
    match tx {
        Some(tx) => {
            let _ = tx.send(msg);
            None
        }
        None => Some(msg),
    }
}

fn os_mount(port: u16, mount: bool, creds: Option<(String, String)>, secret: Option<String>) {
    if std::env::var("NODEINNET_SKIP_OS_MOUNT").is_ok() {
        return;
    }
    std::thread::spawn(move || {
        if mount {
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        let http = http_uri(port, &secret);
        let dav = dav_uri(port, &secret);
        if !mount {
            mount_uris().lock().unwrap().remove(&port);
        }
        if cfg!(target_os = "macos") {
            if mount {
                let script = format!(
                    "set d to (mount volume \"{http}\")\n\
                     return POSIX path of (d as alias)"
                );
                if let Ok(out) = std::process::Command::new("osascript")
                    .arg("-e")
                    .arg(script)
                    .output()
                {
                    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
                    if !path.is_empty() {
                        mapped_drives().lock().unwrap().insert(port, path);
                    }
                }
            } else {
                let path = mapped_drives().lock().unwrap().remove(&port);
                if let Some(path) = path {
                    let _ = std::process::Command::new("umount").arg(&path).status();
                }
            }
        } else if cfg!(target_os = "windows") {
            if mount {
                let drive = {
                    let taken = mapped_drives().lock().unwrap();
                    free_drive_letter(&taken)
                };
                if let Some(drive) = drive {
                    let mut args = vec!["use".to_string(), drive.clone(), http.clone()];
                    if let Some((user, pass)) = &creds {
                        args.push(format!("/user:{user}"));
                        args.push(pass.clone());
                    }
                    if matches!(std::process::Command::new("net").args(&args).status(),
                                Ok(s) if s.success())
                    {
                        mapped_drives().lock().unwrap().insert(port, drive);
                    }
                }
            } else {
                let drive = mapped_drives().lock().unwrap().remove(&port);
                if let Some(drive) = drive {
                    let _ = std::process::Command::new("net")
                        .args(["use", &drive, "/delete", "/y"])
                        .status();
                }
            }
        } else if mount {
            let _ = std::process::Command::new("gio")
                .args(["mount", &dav])
                .status();
        } else {
            let _ = std::process::Command::new("gio")
                .args(["mount", "-u", &dav])
                .status();
        }
        set_kde_place(port, mount, &secret);
    });
}

fn home() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(std::path::PathBuf::from)
}

fn mount_uris() -> &'static Mutex<HashMap<u16, (String, String)>> {
    static URIS: OnceLock<Mutex<HashMap<u16, (String, String)>>> = OnceLock::new();
    URIS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn dav_uri(port: u16, secret: &Option<String>) -> String {
    match secret {
        Some(s) => format!("dav://localhost:{port}/{s}/"),
        None => format!("dav://localhost:{port}/"),
    }
}

fn http_uri(port: u16, secret: &Option<String>) -> String {
    match secret {
        Some(s) => format!("http://127.0.0.1:{port}/{s}/"),
        None => format!("http://127.0.0.1:{port}/"),
    }
}

fn set_kde_place(port: u16, add: bool, secret: &Option<String>) {
    let Some(path) = home().map(|h| h.join(".local/share/user-places.xbel")) else {
        return;
    };
    let Ok(content) = std::fs::read_to_string(&path) else {
        return;
    };
    let uri = dav_uri(port, secret);
    let mut content = remove_xbel_entry(&content, port);
    if add {
        let block = format!(
            " <bookmark href=\"{uri}\">\n  <title>NodeInNet ({port})</title>\n  <info>\n   \
             <metadata owner=\"http://freedesktop.org\">\n    \
             <bookmark:icon name=\"folder-network\"/>\n   </metadata>\n  </info>\n \
             </bookmark>\n"
        );
        if let Some(pos) = content.rfind("</xbel>") {
            content.insert_str(pos, &block);
        }
    }
    let _ = std::fs::write(&path, content);
}

fn remove_xbel_entry(content: &str, port: u16) -> String {
    let href = format!("localhost:{port}/");
    let Some(hpos) = content.find(&href) else {
        return content.to_string();
    };
    let Some(start) = content[..hpos].rfind("<bookmark") else {
        return content.to_string();
    };
    let Some(end_rel) = content[start..].find("</bookmark>") else {
        return content.to_string();
    };
    let end = start + end_rel + "</bookmark>".len();
    let mut out = content.to_string();
    out.replace_range(start..end, "");
    out
}

pub fn mount_resource(
    resource_id: String,
    drive_name: String,
    net_tx: tokio::sync::mpsc::Sender<nodeinnet_p2p::P2pMessage>,
) -> Option<u16> {
    if let Some(m) = mounts().lock().unwrap().get(&resource_id) {
        return Some(m.port);
    }
    let port = free_port()?;

    let (p2p_tx, mut p2p_rx) = tokio::sync::mpsc::channel::<nodeinnet_p2p::P2pMessage>(32);
    let mut drives = HashMap::new();
    drives.insert(
        drive_name,
        Arc::new(Mutex::new(DavDrive::new(resource_id.clone()))),
    );
    let session = Arc::new(Mutex::new(WebDavSession {
        username: "local".to_string(),
        last_access: Instant::now(),
        drives,
    }));
    // The server is on loopback, which every local process can reach, so the mount is.
    let windows = cfg!(target_os = "windows");
    let creds = windows.then(|| ("nodeinnet".to_string(), Uuid::new_v4().simple().to_string()));
    let secret = (!windows).then(|| Uuid::new_v4().simple().to_string());
    let prefix = MountPrefix(secret.as_ref().map(|s| format!("/{s}")).unwrap_or_default());
    mount_uris()
        .lock()
        .unwrap()
        .insert(port, (dav_uri(port, &secret), http_uri(port, &secret)));
    let auth = crate::auth::AuthConfig {
        mode: if creds.is_some() {
            crate::auth::AuthMode::Digest
        } else {
            crate::auth::AuthMode::None
        },
        credentials: creds.clone(),
    };

    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let sys = actix_web::rt::System::new();
        let _ = sys.block_on(async move {
            tokio::spawn(async move {
                while let Some(msg) = p2p_rx.recv().await {
                    let _ = net_tx.send(msg).await;
                }
            });
            let server = HttpServer::new(move || {
                let is_digest = matches!(auth.mode, crate::auth::AuthMode::Digest);
                let is_basic = matches!(auth.mode, crate::auth::AuthMode::Basic);
                App::new()
                    .app_data(web::Data::new(session.clone()))
                    .app_data(web::Data::new(auth.clone()))
                    .app_data(web::Data::new(prefix.clone()))
                    .app_data(web::Data::new(p2p_tx.clone()))
                    .wrap(actix_web::middleware::Condition::new(
                        is_digest,
                        actix_web::middleware::from_fn(crate::auth::digest_auth_middleware),
                    ))
                    .wrap(actix_web::middleware::Condition::new(
                        is_basic,
                        HttpAuthentication::basic(crate::auth::basic_validator),
                    ))
                    .route("/{tail:.*}", web::to(local_handler))
            })
            .bind(("127.0.0.1", port));
            match server {
                Ok(srv) => {
                    let srv = srv.run();
                    let _ = ready_tx.send(Some(srv.handle()));
                    srv.await
                }
                Err(_) => {
                    let _ = ready_tx.send(None);
                    Ok(())
                }
            }
        });
    });

    match ready_rx.recv() {
        Ok(Some(handle)) => {
            mounts()
                .lock()
                .unwrap()
                .insert(resource_id, MountedServer { handle, port });
            os_mount(port, true, creds, secret);
            Some(port)
        }
        _ => None,
    }
}

pub fn unmount_resource(resource_id: &str) {
    if let Some(m) = mounts().lock().unwrap().remove(resource_id) {
        let handle = m.handle;
        std::thread::spawn(move || {
            actix_web::rt::System::new().block_on(handle.stop(true));
        });
        os_mount(m.port, false, None, None);
    }
}

pub fn unmount_all() {
    let drained: Vec<MountedServer> = mounts().lock().unwrap().drain().map(|(_, m)| m).collect();
    for m in drained {
        let handle = m.handle;
        std::thread::spawn(move || {
            actix_web::rt::System::new().block_on(handle.stop(true));
        });
        os_mount(m.port, false, None, None);
    }
}

pub fn open_in_explorer(port: u16) {
    if std::env::var("NODEINNET_SKIP_OS_MOUNT").is_ok() {
        return;
    }
    let mounted = mapped_drives().lock().unwrap().get(&port).cloned();
    let Some((dav, http)) = mount_uris().lock().unwrap().get(&port).cloned() else {
        return;
    };
    std::thread::spawn(move || {
        let _ = if cfg!(target_os = "macos") {
            std::process::Command::new("open")
                .arg(mounted.unwrap_or(http))
                .status()
        } else if cfg!(target_os = "windows") {
            std::process::Command::new("explorer")
                .arg(mounted.unwrap_or(http))
                .status()
        } else {
            std::process::Command::new("xdg-open").arg(dav).status()
        };
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uris_carry_the_secret_path() {
        let secret = Some("abc123".to_string());
        assert_eq!(dav_uri(8801, &secret), "dav://localhost:8801/abc123/");
        assert_eq!(http_uri(8801, &secret), "http://127.0.0.1:8801/abc123/");
        assert_eq!(dav_uri(8801, &None), "dav://localhost:8801/");
    }

    #[test]
    fn only_the_secret_path_is_served() {
        assert!(path_is_under("/abc123", "/abc123"));
        assert!(path_is_under("/abc123/", "/abc123"));
        assert!(path_is_under("/abc123/dir/file.txt", "/abc123"));
        assert!(!path_is_under("/", "/abc123"));
        assert!(!path_is_under("/abc124/file", "/abc123"));
        assert!(!path_is_under("/abc1234", "/abc123"));
    }

    #[test]
    fn xbel_entry_is_removed_despite_the_secret() {
        let xbel = concat!(
            "<xbel>\n",
            " <bookmark href=\"dav://localhost:8801/abc123/\">\n",
            "  <title>NodeInNet (8801)</title>\n",
            " </bookmark>\n",
            " <bookmark href=\"file:///home/user\">\n",
            "  <title>Home</title>\n",
            " </bookmark>\n",
            "</xbel>\n"
        );
        let out = remove_xbel_entry(xbel, 8801);
        assert!(!out.contains("8801"), "the mount entry should be gone");
        assert!(
            out.contains("file:///home/user"),
            "other places must survive"
        );
    }

    #[test]
    fn xbel_untouched_when_the_port_is_absent() {
        let xbel = "<xbel>\n <bookmark href=\"file:///x\"/>\n</xbel>\n";
        assert_eq!(remove_xbel_entry(xbel, 9999), xbel);
    }
}
