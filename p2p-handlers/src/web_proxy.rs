use node_functions::web_proxy::{self, WebProxyEvent};
use nodeinnet_p2p::p2p::P2pMessage;
use p2p_node::NodeContext;
use tokio::sync::mpsc as tokio_mpsc;

pub async fn handle_web_proxy_message(msg: P2pMessage, ctx: NodeContext) {
    if let P2pMessage::HttpRequest {
        request_id,
        resource_id,
        method,
        url,
        headers,
        body,
    } = msg
    {
        ctx.log(format!("🌐 Intercepting HTTP Request: {} {}", method, url));

        let allow_local = crate::net_config::allows_local_network(&ctx, &resource_id);
        let (tx, mut rx) = tokio_mpsc::channel::<WebProxyEvent>(1024);
        let ctx_clone = ctx.clone();

        tokio::spawn(async move {
            web_proxy::execute_proxied_request(&method, &url, &headers, body, allow_local, tx)
                .await;
        });

        tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                match event {
                    WebProxyEvent::ResponseStart { status, headers } => {
                        ctx_clone.log(format!("🌐 Received {} (streaming started)", status));
                        ctx_clone
                            .send_msg(P2pMessage::HttpResponseStart {
                                request_id,
                                resource_id: resource_id.clone(),
                                status,
                                headers,
                            })
                            .await;
                    }
                    WebProxyEvent::Chunk(chunk) => {
                        let mut offset = 0;
                        let max_payload = 60000;
                        while offset < chunk.len() {
                            let end = std::cmp::min(offset + max_payload, chunk.len());
                            let slice = &chunk[offset..end];

                            ctx_clone
                                .send_msg(P2pMessage::HttpResponseChunk {
                                    request_id,
                                    resource_id: resource_id.clone(),
                                    chunk: slice.to_vec(),
                                })
                                .await;

                            offset = end;
                        }
                    }
                    WebProxyEvent::Complete => {
                        ctx_clone
                            .send_msg(P2pMessage::HttpResponseComplete {
                                request_id,
                                resource_id: resource_id.clone(),
                            })
                            .await;
                    }
                    WebProxyEvent::Error(e) => {
                        ctx_clone.log(format!("❌ HTTP Request Failed: {}", e));
                        let mut error_headers = std::collections::BTreeMap::new();
                        error_headers.insert("Content-Type".to_string(), "text/plain".to_string());

                        ctx_clone
                            .send_msg(P2pMessage::HttpResponseStart {
                                request_id,
                                resource_id: resource_id.clone(),
                                status: 502,
                                headers: error_headers,
                            })
                            .await;

                        let error_msg = format!("Proxy Error: {}", e).into_bytes();
                        ctx_clone
                            .send_msg(P2pMessage::HttpResponseChunk {
                                request_id,
                                resource_id: resource_id.clone(),
                                chunk: error_msg,
                            })
                            .await;

                        ctx_clone
                            .send_msg(P2pMessage::HttpResponseComplete {
                                request_id,
                                resource_id: resource_id.clone(),
                            })
                            .await;
                    }
                }
            }
        });
    }
}
