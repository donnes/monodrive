//! Finding devices without making this Mac one: it hears announcements and
//! asks the local subnets who's there, but never announces or registers, so
//! nobody lists a Mac that isn't receiving. Receiving announces; see
//! `receive.rs`.

use super::{Device, Identity, Kind, PORT, runtime, tidy};
use futures_util::StreamExt;
use localsend::model::discovery::ProtocolType;
use localsend::multicast::{self, MulticastConfig, MulticastEvent};
use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

/// How long a host on the subnet gets to answer. LAN peers answer at once
/// or not at all; LocalSend's own scan waits as long.
const ASK_TIMEOUT: Duration = Duration::from_millis(500);

/// How many hosts are asked at once, as LocalSend's own scan does.
const AT_ONCE: usize = 50;

type Found = Box<dyn Fn(&[Device], bool) + Send + Sync>;

struct Shared {
    identity: Arc<Identity>,
    devices: Mutex<Vec<Device>>,
    /// Fingerprints and hosts being checked, so a burst of announcements
    /// checks each once.
    checking: Mutex<HashSet<(String, String)>>,
    scanning: Mutex<bool>,
    found: Found,
}

impl Shared {
    fn tell(&self) {
        let devices = self.devices.lock().map(|d| d.clone()).unwrap_or_default();
        let scanning = self.scanning.lock().map(|s| *s).unwrap_or(false);
        (self.found)(&devices, scanning);
    }

    fn add(&self, device: Device) {
        if device.fingerprint == self.identity.fingerprint {
            return;
        }
        {
            let Ok(mut devices) = self.devices.lock() else {
                return;
            };
            match devices
                .iter_mut()
                .find(|known| known.fingerprint == device.fingerprint)
            {
                Some(known) => match merged(known, device) {
                    Some(merged) => *known = merged,
                    None => return,
                },
                None => devices.push(device),
            }
        }
        self.tell();
    }
}

/// What `known` becomes when `new` claims its fingerprint, or `None` when
/// nothing changes. A plain HTTP claim never replaces a device met over
/// HTTPS, whose certificate proved the fingerprint: it would take the pin
/// off the next send.
pub fn merged(known: &Device, new: Device) -> Option<Device> {
    if known.https && !new.https {
        return None;
    }
    // An upgrade to HTTPS takes the endpoint that serves it.
    let merged = if known.https != new.https || better(known, &new) {
        new
    } else {
        Device {
            host: known.host.clone(),
            port: known.port,
            ..new
        }
    };
    (merged != *known).then_some(merged)
}

/// Whether `new`'s address should replace `known`'s: IPv4 over link-local
/// IPv6, which networks such as phone hotspots often don't deliver.
fn better(known: &Device, new: &Device) -> bool {
    let v4 = |host: &str| host.parse::<Ipv4Addr>().is_ok();
    v4(&new.host) || !v4(&known.host)
}

/// Looking for devices, until dropped.
pub struct Nearby {
    shared: Arc<Shared>,
    stop: Option<oneshot::Sender<()>>,
    scan: Option<tokio::task::JoinHandle<()>>,
}

impl Nearby {
    /// Listens for announcements and asks the subnets once. `found` hears
    /// every device so far, and whether the subnets are still being asked,
    /// each time either changes.
    pub fn start(
        identity: Arc<Identity>,
        found: impl Fn(&[Device], bool) + Send + Sync + 'static,
    ) -> Nearby {
        let shared = Arc::new(Shared {
            identity,
            devices: Mutex::new(Vec::new()),
            checking: Mutex::new(HashSet::new()),
            scanning: Mutex::new(false),
            found: Box::new(found),
        });
        let (stop, stopped) = oneshot::channel();
        runtime().spawn(listen(shared.clone(), stopped));
        let mut nearby = Nearby {
            shared,
            stop: Some(stop),
            scan: None,
        };
        nearby.scan();
        nearby
    }

    /// Asks the subnets again, unless they're being asked now.
    pub fn scan(&mut self) {
        if self.scan.as_ref().is_some_and(|scan| !scan.is_finished()) {
            return;
        }
        if let Ok(mut scanning) = self.shared.scanning.lock() {
            *scanning = true;
        }
        self.shared.tell();
        let shared = self.shared.clone();
        self.scan = Some(runtime().spawn(async move {
            scan(&shared).await;
            if let Ok(mut scanning) = shared.scanning.lock() {
                *scanning = false;
            }
            shared.tell();
        }));
    }

    /// A device found another way, such as one that sent this Mac a file.
    pub fn add(&self, device: Device) {
        self.shared.add(device);
    }

    pub fn devices(&self) -> Vec<Device> {
        self.shared
            .devices
            .lock()
            .map(|d| d.clone())
            .unwrap_or_default()
    }
}

impl Drop for Nearby {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(scan) = self.scan.take() {
            scan.abort();
        }
    }
}

/// Looks for `wait` at most, or until the subnets have all answered, and
/// says who's there.
pub fn look(identity: Arc<Identity>, wait: Duration) -> Vec<Device> {
    let (tell, told) = std::sync::mpsc::channel();
    let nearby = Nearby::start(identity, move |_, scanning| {
        let _ = tell.send(scanning);
    });
    let deadline = std::time::Instant::now() + wait;
    while let Ok(scanning) =
        told.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
    {
        if !scanning {
            break;
        }
    }
    nearby.devices()
}

/// Hears announcements until `stopped`. A port another app holds is shared,
/// so this works beside the LocalSend app.
async fn listen(shared: Arc<Shared>, stopped: oneshot::Receiver<()>) {
    let (heard, mut hearing) = mpsc::channel(64);
    let started = multicast::start(
        MulticastConfig {
            group: multicast::DEFAULT_MULTICAST_GROUP,
            group_v6: Some(multicast::DEFAULT_MULTICAST_GROUP_V6),
            port: PORT,
            interface_filter: Default::default(),
            device: shared.identity.multicast(PORT),
            event_tx: heard,
        },
        stopped,
    )
    .await;
    let Ok(_multicast) = started else {
        // The subnet scan still finds devices.
        return;
    };
    while let Some(event) = hearing.recv().await {
        let MulticastEvent::Discovered {
            ip,
            scope_id,
            message,
        } = event
        else {
            continue;
        };
        let host = match scope_id {
            Some(scope) => format!("{ip}%{scope}"),
            None => ip.to_string(),
        };
        let claimed = Device {
            alias: tidy(&message.alias),
            fingerprint: message.fingerprint,
            kind: Kind::of(message.device_type.as_ref()),
            model: message.device_model.as_deref().map(tidy),
            host,
            port: message.port,
            https: message.protocol == ProtocolType::Https,
        };
        let known = shared.devices.lock().is_ok_and(|devices| {
            devices.iter().any(|d| {
                d.fingerprint == claimed.fingerprint
                    && d.host == claimed.host
                    && d.port == claimed.port
            })
        });
        if known {
            continue;
        }
        let shared = shared.clone();
        tokio::spawn(async move {
            if let Some(device) = check(&shared, claimed).await {
                shared.add(device);
            }
        });
    }
}

/// `claimed` once its certificate shows it is what it says: announcements
/// and unpinned answers prove nothing.
async fn check(shared: &Shared, claimed: Device) -> Option<Device> {
    let key = (claimed.fingerprint.clone(), claimed.host.clone());
    if !shared.checking.lock().ok()?.insert(key.clone()) {
        return None;
    }
    let answer = async {
        let client = shared
            .identity
            .client(claimed.pin(), Some(ASK_TIMEOUT))
            .ok()?;
        client
            .info(claimed.protocol(), &claimed.host, claimed.port)
            .await
            .ok()
    }
    .await;
    if let Ok(mut checking) = shared.checking.lock() {
        checking.remove(&key);
    }
    let info = answer?;
    // Over plain HTTP there's no certificate, so only the claim is left.
    let fingerprint = if claimed.https {
        claimed.fingerprint
    } else {
        info.fingerprint
    };
    Some(Device {
        alias: tidy(&info.alias),
        fingerprint,
        kind: Kind::of(info.device_type.as_ref()),
        model: info.device_model.as_deref().map(tidy),
        ..claimed
    })
}

/// Asks every other host on each Wi-Fi or Ethernet /24 for LocalSend's info
/// on its usual port. VPN and other tunnels' subnets aren't asked: they
/// aren't the room this Mac is in.
async fn scan(shared: &Arc<Shared>) {
    let Ok(client) = shared.identity.client(None, Some(ASK_TIMEOUT)) else {
        return;
    };
    let own = local_addresses();
    let mut subnets: Vec<[u8; 3]> = own
        .iter()
        .map(|ip| {
            let [a, b, c, _] = ip.octets();
            [a, b, c]
        })
        .collect();
    subnets.sort();
    subnets.dedup();
    let hosts: Vec<Ipv4Addr> = subnets
        .into_iter()
        .flat_map(|[a, b, c]| (1..=254).map(move |d| Ipv4Addr::new(a, b, c, d)))
        .filter(|host| !own.contains(host))
        .collect();
    let client = &client;
    futures_util::stream::iter(hosts)
        .map(|host| async move {
            let host = host.to_string();
            let info = client.info(ProtocolType::Https, &host, PORT).await.ok()?;
            Some(Device {
                alias: tidy(&info.alias),
                fingerprint: info.fingerprint,
                kind: Kind::of(info.device_type.as_ref()),
                model: info.device_model.as_deref().map(tidy),
                host,
                port: PORT,
                https: true,
            })
        })
        .buffer_unordered(AT_ONCE)
        .for_each_concurrent(None, |claimed| async move {
            let Some(claimed) = claimed else {
                return;
            };
            if let Some(device) = check(shared, claimed).await {
                shared.add(device);
            }
        })
        .await;
}

/// This Mac's private IPv4 addresses on Wi-Fi and Ethernet.
fn local_addresses() -> Vec<Ipv4Addr> {
    let Ok(interfaces) = if_addrs::get_if_addrs() else {
        return Vec::new();
    };
    interfaces
        .into_iter()
        .filter(|interface| {
            interface.name.starts_with("en") || interface.name.starts_with("bridge")
        })
        .filter_map(|interface| match interface.ip() {
            IpAddr::V4(ip) if ip.is_private() => Some(ip),
            _ => None,
        })
        .collect()
}
