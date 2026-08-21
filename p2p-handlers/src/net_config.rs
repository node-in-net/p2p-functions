
use p2p_node::NodeContext;
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct NetConfig {
    #[serde(default)]
    pub allow_local_network: bool,
}

pub fn allows_local_network(ctx: &NodeContext, resource_id: &str) -> bool {
    ctx.my_info
        .resources
        .iter()
        .find(|r| r.id == resource_id)
        .and_then(|r| r.config.as_ref())
        .and_then(|c| serde_json::from_str::<NetConfig>(c).ok())
        .map(|c| c.allow_local_network)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::NetConfig;

    #[test]
    fn absent_flag_defaults_to_off() {
        let cfg: NetConfig = serde_json::from_str("{}").unwrap();
        assert!(!cfg.allow_local_network);
    }

    #[test]
    fn round_trips() {
        let json = serde_json::to_string(&NetConfig {
            allow_local_network: true,
        })
        .unwrap();
        let back: NetConfig = serde_json::from_str(&json).unwrap();
        assert!(back.allow_local_network);
    }
}
