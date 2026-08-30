use node_functions::registry;
use nodeinnet_p2p::P2pMessage;
use p2p_node::NodeContext;

#[allow(unused_variables)]
pub async fn handle_registry_message(p2p_msg: P2pMessage, ctx: NodeContext) {
    match p2p_msg {
        #[cfg(target_os = "windows")]
        P2pMessage::RequestRegistryKeys {
            request_id,
            resource_id,
            path,
        } => {
            let ctx_clone = ctx.clone();
            tokio::task::spawn_blocking(move || {
                let (subkeys, values, error) = registry::read_registry_keys(&path);
                let response = P2pMessage::RegistryKeysResponse {
                    request_id,
                    resource_id,
                    path,
                    subkeys,
                    values,
                    error,
                };
                tokio::runtime::Handle::current().block_on(ctx_clone.send_msg(response));
            });
        }
        #[cfg(target_os = "windows")]
        P2pMessage::CreateRegistryKeyRequest {
            request_id,
            resource_id,
            parent_path,
            key_name,
        } => {
            let ctx_clone = ctx.clone();
            tokio::task::spawn_blocking(move || {
                let result = registry::create_registry_key(&parent_path, &key_name);
                let response = P2pMessage::CreateRegistryKeyResponse {
                    request_id,
                    resource_id,
                    parent_path,
                    result,
                };
                tokio::runtime::Handle::current().block_on(ctx_clone.send_msg(response));
            });
        }
        #[cfg(target_os = "windows")]
        P2pMessage::DeleteRegistryEntryRequest {
            request_id,
            resource_id,
            path,
            value_name,
            is_key,
        } => {
            let ctx_clone = ctx.clone();
            tokio::task::spawn_blocking(move || {
                let result = registry::delete_registry_entry(&path, is_key, value_name);
                let response = P2pMessage::DeleteRegistryEntryResponse {
                    request_id,
                    resource_id,
                    parent_path: if let Some(last_slash) = path.rfind('/') {
                        path[0..last_slash].to_string()
                    } else {
                        "/".to_string()
                    },
                    result,
                };
                tokio::runtime::Handle::current().block_on(ctx_clone.send_msg(response));
            });
        }
        #[cfg(target_os = "windows")]
        P2pMessage::SetRegistryValueRequest {
            request_id,
            resource_id,
            path,
            value_name,
            value_data,
        } => {
            let ctx_clone = ctx.clone();
            tokio::task::spawn_blocking(move || {
                let result = registry::set_registry_value(&path, &value_name, &value_data);
                let response = P2pMessage::SetRegistryValueResponse {
                    request_id,
                    resource_id,
                    path,
                    result,
                };
                tokio::runtime::Handle::current().block_on(ctx_clone.send_msg(response));
            });
        }
        _ => {}
    }
}
