use node_functions::socks5;
use nodeinnet_p2p::P2pMessage;
use p2p_node::NodeContext;
use tokio::sync::mpsc as tokio_mpsc;

pub async fn handle_socks_message(p2p_msg: P2pMessage, ctx: NodeContext) {
    match p2p_msg {
        P2pMessage::SocksConnectRequest {
            resource_id,
            stream_id,
            host,
            port,
        } => {
            ctx.log(format!(
                "🌐 Peer requested SOCKS connect to {}:{}",
                host, port
            ));

            let (local_tx, local_rx) = tokio_mpsc::channel::<Vec<u8>>(1024);
            ctx.active_socks_streams
                .lock()
                .await
                .insert(stream_id, local_tx);

            let allow_local = crate::net_config::allows_local_network(&ctx, &resource_id);
            let ctx_clone = ctx.clone();
            let res_id_clone = resource_id.clone();

            tokio::spawn(async move {
                match socks5::start_socks_client(&host, port, allow_local, local_rx).await {
                    Ok(mut byte_stream) => {
                        let response = P2pMessage::SocksConnectResponse {
                            resource_id: res_id_clone.clone(),
                            stream_id,
                            is_success: true,
                            error_msg: None,
                        };
                        ctx_clone.send_msg(response).await;

                        while let Some(data) = byte_stream.recv().await {
                            let msg = P2pMessage::SocksData {
                                resource_id: res_id_clone.clone(),
                                stream_id,
                                data,
                            };
                            ctx_clone.send_msg(msg).await;
                        }

                        let msg = P2pMessage::SocksClose {
                            resource_id: res_id_clone,
                            stream_id,
                        };
                        ctx_clone.send_msg(msg).await;
                        ctx_clone
                            .active_socks_streams
                            .lock()
                            .await
                            .remove(&stream_id);
                    }
                    Err(e) => {
                        let response = P2pMessage::SocksConnectResponse {
                            resource_id: res_id_clone,
                            stream_id,
                            is_success: false,
                            error_msg: Some(e),
                        };
                        ctx_clone.send_msg(response).await;
                        ctx_clone
                            .active_socks_streams
                            .lock()
                            .await
                            .remove(&stream_id);
                    }
                }
            });
        }
        P2pMessage::SocksData {
            stream_id, data, ..
        } => {
            let map = ctx.active_socks_streams.lock().await;
            if let Some(tx) = map.get(&stream_id) {
                let _ = tx.try_send(data);
            }
        }
        P2pMessage::SocksClose { stream_id, .. } => {
            ctx.active_socks_streams.lock().await.remove(&stream_id);
        }
        _ => {}
    }
}
