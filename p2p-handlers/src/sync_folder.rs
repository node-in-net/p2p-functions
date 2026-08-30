use nodeinnet_p2p::P2pMessage;
use std::collections::BTreeSet;

pub async fn handle_sync_folder_message(p2p_msg: P2pMessage, ctx: p2p_node::NodeContext) {
    if let P2pMessage::SyncStateRequest {
        resource_id,
        target_folder_names,
    } = p2p_msg
    {
        let my_sync_resources: Vec<_> = ctx
            .my_info
            .resources
            .iter()
            .filter(|r| r.resource_type == nodeinnet_p2p::ResourceType::SyncFolder)
            .cloned()
            .collect();

        let mut available_folders = BTreeSet::new();
        for res in my_sync_resources.iter() {
            available_folders.insert(res.name.clone());
        }

        let mut states =
            node_functions::sync_folder::scan_local_resource_folders(&my_sync_resources);
        states.retain(|k, _| target_folder_names.contains(k));

        ctx.send_msg(P2pMessage::SyncStateResponse {
            resource_id,
            available_folders,
            folder_states: states,
        })
        .await;
    }
}
