use node_functions::system_info;
use nodeinnet_p2p::P2pMessage;
use p2p_node::NodeContext;

pub async fn handle_system_info_message(p2p_msg: P2pMessage, ctx: NodeContext) {
    if let P2pMessage::RequestSystemInfo { resource_id } = p2p_msg {
        ctx.log("ℹ️ Peer requested a one-time system info snapshot.");
        let info = system_info::get_system_info();

        let response = P2pMessage::SystemInfoResponse { resource_id, info };
        ctx.send_msg(response).await;
    }
}
