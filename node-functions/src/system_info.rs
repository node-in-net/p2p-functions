use nodeinnet_p2p::p2p::SysInfo;
use sysinfo::{CpuExt, System, SystemExt};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_info_has_sensible_values() {
        let info = get_system_info();
        assert!(!info.os_family.is_empty(), "os_family is empty");
        assert!(info.cpu_cores > 0, "cpu_cores should be > 0");
        assert!(info.total_memory > 0, "total_memory should be > 0");
    }
}

fn get_network_interfaces() -> Vec<String> {
    use network_interface::{NetworkInterface, NetworkInterfaceConfig};
    let mut network_interfaces = Vec::new();
    if let Ok(interfaces) = NetworkInterface::show() {
        for iface in interfaces {
            for addr in iface.addr {
                let ip = match addr {
                    network_interface::Addr::V4(v4) => std::net::IpAddr::V4(v4.ip),
                    network_interface::Addr::V6(v6) => std::net::IpAddr::V6(v6.ip),
                };
                if !ip.is_loopback() {
                    network_interfaces.push(format!("{}: {}", iface.name, ip));
                }
            }
        }
    }
    network_interfaces
}

pub fn get_system_info() -> SysInfo {
    let mut sys = System::new_all();
    sys.refresh_all();

    SysInfo {
        hostname: sys.host_name().unwrap_or_else(|| "N/A".to_string()),
        os_family: std::env::consts::OS.to_string(),
        os_type: sys.name().unwrap_or_else(|| "N/A".to_string()),
        os_version: sys.os_version().unwrap_or_else(|| "N/A".to_string()),
        cpu_arch: std::env::consts::ARCH.to_string(),
        cpu_cores: sys.cpus().len(),
        cpu_usage: sys.global_cpu_info().cpu_usage(),
        total_memory: sys.total_memory(),
        used_memory: sys.used_memory(),
        total_swap: sys.total_swap(),
        used_swap: sys.used_swap(),
        uptime: sys.uptime(),
        network_interfaces: get_network_interfaces(),
    }
}
