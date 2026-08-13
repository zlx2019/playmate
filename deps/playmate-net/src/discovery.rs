//! LAN service discovery: hosts advertise rooms through mDNS and clients browse them.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};

use crate::NetError;

/// mDNS service type used by Playmate.
pub const SERVICE_TYPE: &str = "_playmate._tcp.local.";

/// A discovered LAN room.
#[derive(Debug, Clone)]
pub struct Room {
    /// Room instance name, taken from the mDNS instance component.
    pub name: String,
    /// User-facing room name from the host-provided TXT record.
    pub display_name: String,
    /// Host connection address.
    pub addr: SocketAddr,
}

/// Host advertisement handle; dropping it unregisters the service and stops mDNS.
pub struct Announcer {
    /// mDNS daemon.
    daemon: ServiceDaemon,
    /// Fully qualified registered service name, used for unregistration.
    fullname: String,
}

impl Announcer {
    /// Starts advertising with `instance` as the service instance and
    /// `display_name` as the room name. mdns-sd discovers addresses automatically.
    pub fn start(instance: &str, port: u16, display_name: &str) -> Result<Self, NetError> {
        let daemon = ServiceDaemon::new()?;
        let host_name = format!("{instance}.local.");
        let properties = [("name", display_name)];
        let info = ServiceInfo::new(
            SERVICE_TYPE,
            instance,
            &host_name,
            "",
            port,
            &properties[..],
        )?
        .enable_addr_auto();
        let fullname = info.get_fullname().to_string();
        daemon.register(info)?;
        log::info!("mDNS advertisement started: {fullname} (port {port})");
        Ok(Self { daemon, fullname })
    }
}

impl Drop for Announcer {
    fn drop(&mut self) {
        // Send a goodbye packet so clients immediately see the room close, then stop the daemon.
        let _ = self.daemon.unregister(&self.fullname);
        let _ = self.daemon.shutdown();
    }
}

/// Browses for `timeout` and returns currently available rooms, deduplicated by instance.
///
/// A room may advertise addresses on multiple interfaces and produce multiple
/// resolved events. Only one record is kept per instance, using the address
/// selected by `pick_best_ipv4`.
///
/// This function is **synchronous and blocking** because mdns-sd uses worker
/// threads. Wrap it in `tokio::task::spawn_blocking` from an async context.
pub fn browse_rooms(timeout: Duration) -> Result<Vec<Room>, NetError> {
    let daemon = ServiceDaemon::new()?;
    let receiver = daemon.browse(SERVICE_TYPE)?;
    let deadline = Instant::now() + timeout;
    let mut rooms: Vec<Room> = Vec::new();

    // Receive events until the deadline, timeout, or channel closure.
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        let Ok(event) = receiver.recv_timeout(remaining) else {
            break;
        };
        if let ServiceEvent::ServiceResolved(service) = event {
            let Some(ip) = pick_best_ipv4(service.get_addresses_v4()) else {
                continue;
            };
            let addr = SocketAddr::from((ip, service.get_port()));
            let name = service
                .get_fullname()
                .split('.')
                .next()
                .unwrap_or("未知房间")
                .to_string();
            // Later events for the same instance update its address without adding a row.
            if let Some(existing) = rooms.iter_mut().find(|r| r.name == name) {
                existing.addr = addr;
                continue;
            }
            let display_name = service
                .get_property_val_str("name")
                .unwrap_or("未命名房间")
                .to_string();
            log::info!("discovered room: {display_name} ({name}) @ {addr}");
            rooms.push(Room {
                name,
                display_name,
                addr,
            });
        }
    }
    let _ = daemon.shutdown();
    Ok(rooms)
}

/// Chooses the best IPv4 address for a direct connection:
/// private network > other address > link-local > loopback.
fn pick_best_ipv4(
    addrs: impl IntoIterator<Item = std::net::Ipv4Addr>,
) -> Option<std::net::Ipv4Addr> {
    fn score(ip: &std::net::Ipv4Addr) -> u8 {
        if ip.is_loopback() {
            0
        } else if ip.is_link_local() {
            1
        } else if ip.is_private() {
            3
        } else {
            2
        }
    }
    addrs.into_iter().max_by_key(score)
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use super::*;

    /// Private addresses are preferred over loopback and link-local addresses.
    #[test]
    fn best_ipv4_prefers_private_over_loopback() {
        let addrs = [
            Ipv4Addr::new(127, 0, 0, 1),
            Ipv4Addr::new(169, 254, 3, 7),
            Ipv4Addr::new(192, 168, 10, 95),
        ];
        assert_eq!(pick_best_ipv4(addrs), Some(Ipv4Addr::new(192, 168, 10, 95)));
    }

    /// Loopback is still returned when it is the only option for local testing.
    #[test]
    fn best_ipv4_falls_back_to_loopback() {
        let addrs = [Ipv4Addr::new(127, 0, 0, 1)];
        assert_eq!(pick_best_ipv4(addrs), Some(Ipv4Addr::new(127, 0, 0, 1)));
        assert_eq!(pick_best_ipv4([]), None);
    }
}
