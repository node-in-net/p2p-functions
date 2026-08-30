use client_core::launcher::{app_launch_provider, refusal};
use nodeinnet_p2p::P2pMessage;
use p2p_node::NodeContext;

pub async fn handle_app_launch_message(msg: P2pMessage, ctx: NodeContext) {
    match msg {
        P2pMessage::AppListRequest {
            resource_id,
            request_id,
        } => {
            let (apps, sessions, refused) = match provider_view(&ctx) {
                Ok((apps, sessions)) => (apps, sessions, None),
                Err(code) => (Vec::new(), Vec::new(), Some(code.to_string())),
            };
            ctx.send_msg(P2pMessage::AppListResponse {
                resource_id,
                request_id: Some(request_id),
                apps,
                sessions,
                refused,
                event: None,
            })
            .await;
        }

        P2pMessage::AppLaunchRequest {
            resource_id,
            request_id,
            session_id,
            app_id,
        } => {
            let error = if app_launch_provider().is_none() {
                Some(refusal::NOT_SUPPORTED.to_string())
            } else {
                let egress = ctx
                    .remote_resources
                    .lock()
                    .await
                    .get(&nodeinnet_p2p::ResourceType::SharedNetwork)
                    .cloned();
                match egress {
                    None => Some(refusal::NO_EGRESS.to_string()),
                    Some(egress) => {
                        match act(&ctx, |p| {
                            p.launch(&ctx.peer_id, &egress, session_id, &app_id)
                        }) {
                            Ok(()) => None,
                            Err(code) => Some(code.to_string()),
                        }
                    }
                }
            };
            ctx.log(match &error {
                None => format!(
                    "▶️ {} started {app_id} for {}",
                    ctx.my_info.name, ctx.peer_id
                ),
                Some(code) => format!("⛔ refused {app_id} for {}: {code}", ctx.peer_id),
            });
            ctx.send_msg(P2pMessage::AppActionResponse {
                resource_id,
                request_id,
                session_id: Some(session_id),
                error,
                detail: None,
            })
            .await;
        }

        P2pMessage::AppStopRequest {
            resource_id,
            request_id,
            session_id,
        } => {
            let error = match act(&ctx, |p| p.stop(&ctx.peer_id, session_id)) {
                Ok(()) => None,
                Err(code) => Some(code.to_string()),
            };
            ctx.send_msg(P2pMessage::AppActionResponse {
                resource_id,
                request_id,
                session_id: Some(session_id),
                error,
                detail: None,
            })
            .await;
        }

        _ => {}
    }
}

fn provider_view(
    ctx: &NodeContext,
) -> Result<
    (
        Vec<nodeinnet_p2p::p2p::LaunchableApp>,
        Vec<nodeinnet_p2p::p2p::RemoteAppSession>,
    ),
    &'static str,
> {
    let provider = app_launch_provider().ok_or(refusal::NOT_SUPPORTED)?;
    provider.view(&ctx.peer_id).ok_or(refusal::NETWORK_OFF)
}

fn act<F>(ctx: &NodeContext, f: F) -> Result<(), &'static str>
where
    F: FnOnce(
        &std::sync::Arc<dyn client_core::launcher::AppLaunchProvider>,
    ) -> Result<(), &'static str>,
{
    let provider = app_launch_provider().ok_or(refusal::NOT_SUPPORTED)?;
    if provider.view(&ctx.peer_id).is_none() {
        return Err(refusal::NETWORK_OFF);
    }
    f(provider)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nodeinnet_p2p::{NodeInfo, OutboundP2pPayload};
    use std::collections::HashMap;
    use tokio::sync::mpsc;

    struct NoPeers;
    impl nodeinnet_p2p::PeerStore for NoPeers {
        fn load(&self) -> HashMap<String, nodeinnet_p2p::PeerConfig> {
            HashMap::new()
        }
        fn update(&self, _: &mut dyn FnMut(&mut HashMap<String, nodeinnet_p2p::PeerConfig>)) {}
    }

    fn context(peer: &str) -> (NodeContext, mpsc::Receiver<OutboundP2pPayload>) {
        let (out_tx, out_rx) = mpsc::channel(4);
        let (log_tx, _log_rx) = mpsc::channel(16);
        let (evt_tx, _evt_rx) = mpsc::channel(4);
        let info = NodeInfo {
            id: "runner".into(),
            name: "Runner".into(),
            os: "linux".into(),
            version: "0.0.0".into(),
            app_type: "test".into(),
            build_type: "debug".into(),
            public_key: String::new(),
            resources: Vec::new(),
            is_online: true,
            last_used: 0,
            is_temporary: false,
        };
        let ctx = NodeContext::new(
            out_tx,
            log_tx,
            evt_tx,
            info,
            peer.to_string(),
            std::sync::Arc::new(NoPeers),
        );
        (ctx, out_rx)
    }

    async fn answer(mut rx: mpsc::Receiver<OutboundP2pPayload>) -> P2pMessage {
        match tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv()).await {
            Ok(Some(OutboundP2pPayload::Message(env))) => env.message,
            other => panic!("the handler answered nothing: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_node_that_cannot_run_anything_still_answers() {
        let (ctx, rx) = context("asking-peer");
        handle_app_launch_message(
            P2pMessage::AppLaunchRequest {
                resource_id: "network-runner".into(),
                request_id: uuid::Uuid::new_v4(),
                session_id: uuid::Uuid::new_v4(),
                app_id: "firefox".into(),
            },
            ctx,
        )
        .await;

        match answer(rx).await {
            P2pMessage::AppActionResponse { error, .. } => assert_eq!(
                error.as_deref(),
                Some(refusal::NOT_SUPPORTED),
                "a node without a provider must refuse by name"
            ),
            other => panic!("wrong reply: {other:?}"),
        }
    }

    #[tokio::test]
    async fn listing_is_answered_too_even_when_there_is_nothing_to_list() {
        let (ctx, rx) = context("asking-peer");
        handle_app_launch_message(
            P2pMessage::AppListRequest {
                resource_id: "network-runner".into(),
                request_id: uuid::Uuid::new_v4(),
            },
            ctx,
        )
        .await;

        match answer(rx).await {
            P2pMessage::AppListResponse { refused, apps, .. } => {
                assert_eq!(refused.as_deref(), Some(refusal::NOT_SUPPORTED));
                assert!(apps.is_empty(), "nothing is offered when nothing can run");
            }
            other => panic!("wrong reply: {other:?}"),
        }
    }

    #[tokio::test]
    async fn stopping_an_unknown_session_is_refused_not_ignored() {
        let (ctx, rx) = context("asking-peer");
        handle_app_launch_message(
            P2pMessage::AppStopRequest {
                resource_id: "network-runner".into(),
                request_id: uuid::Uuid::new_v4(),
                session_id: uuid::Uuid::new_v4(),
            },
            ctx,
        )
        .await;
        assert!(matches!(
            answer(rx).await,
            P2pMessage::AppActionResponse { error: Some(_), .. }
        ));
    }
}
