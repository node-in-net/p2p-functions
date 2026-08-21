use node_functions::terminal;
use nodeinnet_p2p::P2pMessage;
use p2p_node::NodeContext;

pub async fn handle_terminal_message(p2p_msg: P2pMessage, ctx: NodeContext) {
    match p2p_msg {
        P2pMessage::StartTerminal { resource_id } => {
            ctx.log("🚀 Peer requested to start a true PTY terminal session.");

            match terminal::spawn_pty_session(None) {
                Ok(mut session) => {
                    ctx.log("✅ PTY process spawned successfully.");

                    let ctx_out = ctx.clone();
                    let res_id_out = resource_id.clone();

                    tokio::spawn(async move {
                        while let Some(data) = session.output_rx.recv().await {
                            let msg = P2pMessage::TerminalOutput {
                                resource_id: res_id_out.clone(),
                                data,
                            };
                            ctx_out.send_msg(msg).await;
                        }
                    });

                    let session_id = uuid::Uuid::new_v4();

                    ctx.active_terminals.lock().await.insert(
                        resource_id.clone(),
                        (session.input_tx, session.resize_tx, session_id),
                    );
                }
                Err(e) => {
                    ctx.log(format!("❌ Failed to open PTY system: {}", e));
                    let error_msg = P2pMessage::TerminalOutput {
                        resource_id,
                        data: format!("Error: Failed to allocate PTY: {}\r\n", e).into_bytes(),
                    };
                    ctx.send_msg(error_msg).await;
                }
            }
        }
        P2pMessage::StopTerminal { resource_id } => {
            ctx.log("🛑 Peer stopped terminal session.");
            // Removing from the map drops the Sender (input_tx), which gracefully kills the PTY.
            ctx.active_terminals.lock().await.remove(&resource_id);
        }
        P2pMessage::TerminalInput { resource_id, data } => {
            if data.len() == 1 && data[0] == 0x03 {
                ctx.log("🛑 Received SIGINT (Ctrl+C) command from peer.");
            }
            let map = ctx.active_terminals.lock().await;
            if let Some((tx, _, _)) = map.get(&resource_id) {
                if let Err(e) = tx.try_send(data) {
                    ctx.log(format!("⚠️ Failed to send input to PTY: {}", e));
                }
            } else {
                ctx.log(format!(
                    "⚠️ Received TerminalInput, but session {} is not active.",
                    resource_id
                ));
            }
        }
        P2pMessage::TerminalResize {
            resource_id,
            rows,
            cols,
        } => {
            let map = ctx.active_terminals.lock().await;
            if let Some((_, resize_tx, _)) = map.get(&resource_id) {
                if let Err(e) = resize_tx.try_send((rows, cols)) {
                    ctx.log(format!("⚠️ Failed to send resize to PTY: {}", e));
                }
            } else {
                ctx.log(format!(
                    "⚠️ Received TerminalResize, but session {} is not active.",
                    resource_id
                ));
            }
        }
        _ => {}
    }
}
