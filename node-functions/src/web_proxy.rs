use reqwest::{Client, Method};
use std::collections::BTreeMap;
use tokio::sync::mpsc as tokio_mpsc;

#[derive(Debug)]
pub enum WebProxyEvent {
    ResponseStart {
        status: u16,
        headers: BTreeMap<String, String>,
    },
    Chunk(Vec<u8>),
    Complete,
    Error(String),
}

const MAX_REDIRECTS: usize = 10;

async fn client_for(url: &reqwest::Url, allow_local: bool) -> Result<Client, String> {
    let host = url.host_str().ok_or("URL has no host")?;
    let port = url
        .port_or_known_default()
        .ok_or("URL has no port and no default")?;
    let addrs = crate::net_guard::resolve_allowed(host, port, allow_local).await?;

    Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .resolve_to_addrs(host, &addrs)
        .build()
        .map_err(|e| e.to_string())
}

pub async fn execute_proxied_request(
    method_str: &str,
    url: &str,
    headers: &BTreeMap<String, String>,
    body: Option<Vec<u8>>,
    allow_local: bool,
    tx: tokio_mpsc::Sender<WebProxyEvent>,
) {
    let mut req_method = match method_str.to_uppercase().as_str() {
        "POST" => Method::POST,
        "PUT" => Method::PUT,
        "DELETE" => Method::DELETE,
        "PATCH" => Method::PATCH,
        "HEAD" => Method::HEAD,
        "OPTIONS" => Method::OPTIONS,
        _ => Method::GET,
    };

    let mut target = match reqwest::Url::parse(url) {
        Ok(u) => u,
        Err(e) => {
            let _ = tx.send(WebProxyEvent::Error(e.to_string())).await;
            return;
        }
    };
    let mut body = body;
    let mut resp = None;

    for _ in 0..=MAX_REDIRECTS {
        let client = match client_for(&target, allow_local).await {
            Ok(c) => c,
            Err(e) => {
                let _ = tx.send(WebProxyEvent::Error(e)).await;
                return;
            }
        };

        let mut req_builder = client.request(req_method.clone(), target.clone());
        for (k, v) in headers.iter() {
            if k.to_lowercase() != "host" {
                req_builder = req_builder.header(k, v);
            }
        }
        if let Some(b) = &body {
            req_builder = req_builder.body(b.clone());
        }

        let hop = match req_builder.send().await {
            Ok(r) => r,
            Err(e) => {
                let _ = tx.send(WebProxyEvent::Error(e.to_string())).await;
                return;
            }
        };

        let next = hop
            .status()
            .is_redirection()
            .then(|| hop.headers().get(reqwest::header::LOCATION))
            .flatten()
            .and_then(|l| l.to_str().ok())
            .and_then(|l| target.join(l).ok());

        match next {
            Some(next) => {
                if !matches!(hop.status().as_u16(), 307 | 308) {
                    req_method = Method::GET;
                    body = None;
                }
                target = next;
            }
            None => {
                resp = Some(hop);
                break;
            }
        }
    }

    let Some(resp) = resp else {
        let _ = tx
            .send(WebProxyEvent::Error("Too many redirects".to_string()))
            .await;
        return;
    };

    let mut resp = resp;
    let status = resp.status().as_u16();
    let mut resp_headers = BTreeMap::new();
    for (k, v) in resp.headers().iter() {
        let lower_k = k.as_str().to_lowercase();
        if lower_k == "x-frame-options"
            || lower_k == "content-security-policy"
            || lower_k == "strict-transport-security"
        {
            continue;
        }
        if let Ok(val_str) = v.to_str() {
            resp_headers.insert(k.as_str().to_string(), val_str.to_string());
        }
    }

    if tx
        .send(WebProxyEvent::ResponseStart {
            status,
            headers: resp_headers,
        })
        .await
        .is_err()
    {
        return;
    }

    while let Ok(Some(chunk)) = resp.chunk().await {
        if tx.send(WebProxyEvent::Chunk(chunk.to_vec())).await.is_err() {
            return;
        }
    }

    let _ = tx.send(WebProxyEvent::Complete).await;
}
