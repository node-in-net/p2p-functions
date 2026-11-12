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
            .filter(|r| r.resource_type == nodeinnet_p2p::ResourceType::SyncFolder && r.is_active)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[tokio::test]
    async fn a_switched_off_sync_folder_is_not_reported() {
        let dir = Scratch(
            std::env::temp_dir().join(format!("p2p-handlers-sync-{}", uuid::Uuid::new_v4())),
        );
        for (folder, file) in [("work", "a.txt"), ("private", "b.txt")] {
            std::fs::create_dir_all(dir.0.join(folder)).unwrap();
            std::fs::write(dir.0.join(folder).join(file), folder).unwrap();
        }
        let folder = |name: &str, is_active| nodeinnet_p2p::SharedResource {
            id: name.to_string(),
            name: name.to_string(),
            resource_type: nodeinnet_p2p::ResourceType::SyncFolder,
            config: Some(serde_json::json!({ "path": dir.0.join(name) }).to_string()),
            is_active,
            session_token: None,
        };
        let me = nodeinnet_p2p::NodeInfo {
            id: "me".to_string(),
            name: "me".to_string(),
            os: "linux".to_string(),
            version: "0.0.0".to_string(),
            app_type: "test".to_string(),
            build_type: "debug".to_string(),
            public_key: String::new(),
            resources: vec![folder("work", true), folder("private", false)],
            is_online: true,
            last_used: 0,
            is_temporary: false,
        };
        let (out_tx, mut out_rx) = tokio::sync::mpsc::channel(8);
        let (log_tx, _) = tokio::sync::mpsc::channel(8);
        let (event_tx, _) = tokio::sync::mpsc::channel(8);
        let ctx = p2p_node::NodeContext::new(
            out_tx,
            log_tx,
            event_tx,
            me,
            "peer".to_string(),
            std::sync::Arc::new(nodeinnet_p2p::MemoryPeerStore::default()),
        );

        handle_sync_folder_message(
            P2pMessage::SyncStateRequest {
                resource_id: "work".to_string(),
                target_folder_names: vec!["work".to_string(), "private".to_string()],
            },
            ctx,
        )
        .await;

        let Ok(nodeinnet_p2p::OutboundP2pPayload::Message(nodeinnet_p2p::SecuredP2pEnvelope {
            message:
                P2pMessage::SyncStateResponse {
                    available_folders,
                    folder_states,
                    ..
                },
            ..
        })) = out_rx.try_recv()
        else {
            panic!("no SyncStateResponse");
        };
        assert_eq!(available_folders.into_iter().collect::<Vec<_>>(), ["work"]);
        assert_eq!(folder_states.keys().collect::<Vec<_>>(), ["work"]);
    }
}
