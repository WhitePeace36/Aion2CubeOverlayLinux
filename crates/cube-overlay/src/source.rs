//! Where the radar gets its data: live capture, a saved capture played back at its own pace, or the demo.
//!
//! Live capture opens libpcap on Linux (the `any` pseudo device by default, so it does not matter which
//! interface the game uses) with a BPF filter on the game port, and feeds every packet through the unchanged
//! aion2-tui decoder (`aion-net` and `cube-core`). It only reads traffic; nothing is ever sent.

use crate::store::{SpotStore, TemplateStore};
use aion_net::net::LinkType;
use cube_core::cubes::{CubeBook, CubeTemplate};
use cube_core::parser::Event;
use cube_core::pipeline::Pipeline;
use cube_core::snapshot::{Alert, RadarSnapshot};
use cube_core::tracker::{Tracker, TrackerConfig};
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

/// Flows quiet for this long are forgotten, and the status drops back to waiting.
const FLOW_IDLE_MS: u64 = 120_000;
/// Libpcap's link types for Linux cooked capture, v1 and v2 (what the `any` device gives).
const DLT_LINUX_SLL: u32 = 113;
const DLT_LINUX_SLL2: u32 = 276;
const ETHERTYPE_IPV4: u16 = 0x0800;
const ETHERTYPE_IPV6: u16 = 0x86DD;

/// The link layer the decoder should read `data` as, and the bytes to hand it.
///
/// Linux cooked headers are unwrapped here down to the IP packet: the decoder's own reader only takes the
/// hardware types of Ethernet and Wi-Fi, while on Linux the game may well come through `ppp0` (PPPoE dialled
/// on the PC), a VPN tunnel or loopback, whose cooked headers it would drop.
pub fn ip_view(link_code: u32, data: &[u8]) -> Option<(LinkType, &[u8])> {
    let be16 = |at: usize| data.get(at..at + 2).map(|b| u16::from_be_bytes([b[0], b[1]]));
    let ip = |proto: Option<u16>| matches!(proto, Some(ETHERTYPE_IPV4 | ETHERTYPE_IPV6));
    match link_code {
        DLT_LINUX_SLL if ip(be16(14)) => Some((LinkType::Raw, &data[16..])),
        DLT_LINUX_SLL2 if ip(be16(0)) && data.len() >= 20 => Some((LinkType::Raw, &data[20..])),
        DLT_LINUX_SLL2 => None,
        code => LinkType::from_pcap(code).map(|l| (l, data)),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    Good,
    Waiting,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub text: String,
    pub health: Health,
}

pub trait Source {
    /// Advances clocks and collects alerts. Called about ten times a second with the wall clock in ms.
    fn step(&mut self, now_ms: u64);
    fn snapshot(&self) -> RadarSnapshot;
    /// Alerts raised since the last call, oldest first.
    fn drain_alerts(&mut self) -> Vec<Alert>;
    fn status(&self) -> Status;
    /// Why no cube may show even though the game is being read, for the status line.
    fn hint(&self) -> Option<String> {
        None
    }
}

/// How often the live source logs what it sees.
const SUMMARY_EVERY_MS: u64 = 30_000;

/// A hint when no cube can show yet: the map is unknown, or no cube template is known for this zone.
pub fn zone_hint(map_id: Option<u32>, book: &CubeBook) -> Option<String> {
    let Some(map_id) = map_id else {
        return Some("map unknown until a loading screen (teleport or change zone once)".into());
    };
    let gd = cube_core::gamedata::GameData::get();
    if gd.is_instance(map_id) {
        return None;
    }
    // Only zones the game data names can be checked: the Altgard template, for one, sits on a map (probably
    // 1110) that is not named yet, so an unnamed map gets no hint rather than a wrong one.
    let zone = gd.zone(map_id)?;
    let known = book.all().iter().any(|t| t.zone.as_deref() == Some(zone) || (t.zone.is_none() && t.evidence == cube_core::cubes::Evidence::Manual));
    (!known).then(|| format!("no cube type known for {zone} yet: open one cube here with a key to teach it"))
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// The cube book: built in templates, the ones learned in earlier runs and the ones given on the command line.
pub fn book(store: &TemplateStore, manual: &[u32]) -> CubeBook {
    let mut book = CubeBook::default();
    store.load_into(&mut book);
    for &template in manual {
        book.add(CubeTemplate { template, zone: None, evidence: cube_core::cubes::Evidence::Manual });
    }
    book
}

pub struct CaptureOptions {
    /// `None`: the `any` pseudo device.
    pub interface: Option<String>,
    pub ports: Vec<u16>,
}

struct Shared {
    pipeline: Mutex<Pipeline>,
    tracker: Mutex<Tracker>,
    status: Mutex<Status>,
    live: AtomicBool,
    last_packet_ms: AtomicU64,
}

pub struct Live {
    shared: Arc<Shared>,
    templates: TemplateStore,
    spots: SpotStore,
    last_tick_ms: u64,
    alerts: Vec<Alert>,
    hint: Option<String>,
    last_map: Option<u32>,
    last_summary_ms: u64,
}

pub fn bpf_filter(ports: &[u16]) -> String {
    ports.iter().map(|p| format!("tcp port {p}")).collect::<Vec<_>>().join(" or ")
}

fn permission_hint(error: &str) -> String {
    let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "aion-cube-overlay".into());
    if error.contains("ermission") || error.contains("not permitted") {
        format!("No permission to capture. Run once: sudo setcap cap_net_raw,cap_net_admin=eip {exe}")
    } else {
        format!("Capture failed: {error}")
    }
}

fn open(options: &CaptureOptions) -> Result<(pcap::Capture<pcap::Active>, u32, String), String> {
    let name = options.interface.clone().unwrap_or_else(|| "any".into());
    let capture = pcap::Capture::from_device(name.as_str())
        .and_then(|c| c.promisc(false).snaplen(65_535).timeout(250).immediate_mode(true).open())
        .map_err(|e| permission_hint(&e.to_string()))?;
    let mut capture = capture;
    capture.filter(&bpf_filter(&options.ports), true).map_err(|e| format!("Could not set the capture filter: {e}"))?;
    let code = capture.get_datalink().0 as u32;
    if code != DLT_LINUX_SLL2 && LinkType::from_pcap(code).is_none() {
        return Err(format!("Interface {name} has link type {code}, which the decoder does not read"));
    }
    Ok((capture, code, name))
}

impl Live {
    /// Opens the capture and starts its thread. A failure is not fatal: it shows on the overlay.
    pub fn start(options: CaptureOptions, book: CubeBook, templates: TemplateStore, spots: SpotStore) -> Live {
        let mut tracker = Tracker::new(TrackerConfig::default(), book);
        let saved = spots.load();
        log::info!("{} cube spots saved earlier", saved.len());
        tracker.set_spots(saved);
        let shared = Arc::new(Shared {
            pipeline: Mutex::new(Pipeline::new(options.ports.clone())),
            tracker: Mutex::new(tracker),
            status: Mutex::new(Status { text: "starting capture".into(), health: Health::Waiting }),
            live: AtomicBool::new(false),
            last_packet_ms: AtomicU64::new(0),
        });
        match open(&options) {
            Ok((capture, link, name)) => {
                log::info!("capturing on {name} (link type {link}) with filter \"{}\"", bpf_filter(&options.ports));
                *shared.status.lock() = Status { text: "waiting for game traffic".into(), health: Health::Waiting };
                let thread_shared = shared.clone();
                let spawned = std::thread::Builder::new().name("capture".into()).spawn(move || capture_loop(thread_shared, capture, link));
                if let Err(e) = spawned {
                    *shared.status.lock() = Status { text: format!("could not start the capture thread: {e}"), health: Health::Error };
                }
            }
            Err(message) => {
                log::error!("{message}");
                *shared.status.lock() = Status { text: message, health: Health::Error };
            }
        }
        Live { shared, templates, spots, last_tick_ms: 0, alerts: Vec::new(), hint: None, last_map: None, last_summary_ms: 0 }
    }
}

fn capture_loop(shared: Arc<Shared>, mut capture: pcap::Capture<pcap::Active>, link_code: u32) {
    let mut events: Vec<Event> = Vec::new();
    let mut packets: u64 = 0;
    loop {
        match capture.next_packet() {
            Ok(packet) => {
                packets += 1;
                if packets == 1 {
                    log::info!("first packet on the game port ({} bytes)", packet.data.len());
                }
                let ts = &packet.header.ts;
                // timeval fields are 32 bit on some targets.
                #[allow(clippy::unnecessary_cast)]
                let t_ms = (ts.tv_sec as i64 * 1000 + ts.tv_usec as i64 / 1000).max(1) as u64;
                shared.last_packet_ms.fetch_max(t_ms, Ordering::Relaxed);
                let Some((link, data)) = ip_view(link_code, packet.data) else { continue };
                let has_flow = {
                    let mut pipe = shared.pipeline.lock();
                    pipe.push(link, t_ms, data, &mut |event| events.push(event));
                    pipe.has_game_flow()
                };
                if has_flow && !shared.live.swap(true, Ordering::SeqCst) {
                    log::info!("game traffic seen");
                    *shared.status.lock() = Status { text: "reading the game".into(), health: Health::Good };
                }
                if !events.is_empty() {
                    let mut tracker = shared.tracker.lock();
                    for e in events.drain(..) {
                        tracker.push(&e);
                    }
                }
            }
            Err(pcap::Error::TimeoutExpired) => {}
            Err(e) => {
                log::error!("capture stopped: {e}");
                *shared.status.lock() = Status { text: format!("capture stopped: {e}"), health: Health::Error };
                return;
            }
        }
    }
}

impl Source for Live {
    fn step(&mut self, wall_ms: u64) {
        // Packet time while packets flow (it can lag the wall clock a little), wall time when the game is quiet.
        let now = wall_ms.max(self.last_tick_ms);
        self.last_tick_ms = now;
        let has_flow = {
            let mut pipe = self.shared.pipeline.lock();
            pipe.expire_flows(now, FLOW_IDLE_MS);
            pipe.has_game_flow()
        };
        if !has_flow && self.shared.live.swap(false, Ordering::SeqCst) {
            let mut status = self.shared.status.lock();
            if status.health == Health::Good {
                log::info!("game traffic stopped");
                *status = Status { text: "waiting for game traffic".into(), health: Health::Waiting };
            }
        }
        let (alerts, learned, spots) = {
            let mut tracker = self.shared.tracker.lock();
            tracker.tick(now);
            let map = tracker.map_id();
            self.hint = zone_hint(map, tracker.book());
            if map != self.last_map {
                self.last_map = map;
                if let Some(m) = map {
                    log::info!("map: {} ({m})", cube_core::gamedata::GameData::get().map(m).name);
                }
            }
            if self.shared.live.load(Ordering::Relaxed) && now.saturating_sub(self.last_summary_ms) >= SUMMARY_EVERY_MS {
                self.last_summary_ms = now;
                let s = tracker.snapshot(now);
                let fix = s.me.map(|m| format!("{} s old", now.saturating_sub(m.t_ms) / 1000)).unwrap_or_else(|| "none yet (use a skill)".into());
                log::info!(
                    "reading: map {}, {} objects in view, {} cubes on the radar, {} seen this run, your position {fix}{}",
                    s.map.as_ref().map(|m| m.name.as_str()).unwrap_or("unknown"),
                    s.objects_in_view,
                    s.open_cubes().count(),
                    s.counts.cubes_seen,
                    self.hint.as_ref().map(|h| format!("; {h}")).unwrap_or_default()
                );
            }
            (tracker.drain_alerts(), tracker.drain_learned(), tracker.take_spots_changed())
        };
        if let Some(spots) = spots {
            if let Err(e) = self.spots.save(&spots) {
                log::warn!("could not save the cube spots: {e}");
            }
        }
        for t in learned {
            log::info!("learned cube template {} ({:?})", t.template, t.zone);
            if let Err(e) = self.templates.add(t) {
                log::warn!("could not save the learned template: {e}");
            }
        }
        for a in &alerts {
            log::info!("alert {:?}: cube {} template {}, {:?} m, height {:?} m", a.kind, a.cube.id, a.cube.template, a.cube.distance_m, a.cube.height_m);
        }
        self.alerts.extend(alerts);
    }

    fn snapshot(&self) -> RadarSnapshot {
        self.shared.tracker.lock().snapshot(self.last_tick_ms)
    }

    fn drain_alerts(&mut self) -> Vec<Alert> {
        std::mem::take(&mut self.alerts)
    }

    fn status(&self) -> Status {
        self.shared.status.lock().clone()
    }

    fn hint(&self) -> Option<String> {
        self.hint.clone()
    }
}

/// A saved pcap or pcapng capture, played back at its own pace (times `speed`), so the overlay behaves as it
/// would have live. Nothing is saved to disk.
pub struct Replay {
    packets: Vec<(u32, u64, Vec<u8>)>,
    next: usize,
    pipeline: Pipeline,
    tracker: Tracker,
    first_ms: u64,
    started_wall_ms: Option<u64>,
    speed: f64,
    now_ms: u64,
    alerts: Vec<Alert>,
    skipped_links: usize,
}

impl Replay {
    pub fn load(path: &std::path::Path, ports: Vec<u16>, book: CubeBook, speed: f64) -> anyhow::Result<Replay> {
        let bytes = std::fs::read(path).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
        let mut packets = Vec::new();
        let mut skipped_links = 0;
        aion_net::capture_file::read_packets(&bytes, |link, t_ms, data| match ip_view(link, data) {
            Some(_) => packets.push((link, t_ms, data.to_vec())),
            None => skipped_links += 1,
        })?;
        packets.sort_by_key(|p| p.1);
        let first_ms = packets.first().map(|p| p.1).unwrap_or(0);
        log::info!("{} packets in {}", packets.len(), path.display());
        Ok(Replay {
            packets,
            next: 0,
            pipeline: Pipeline::new(ports),
            tracker: Tracker::new(TrackerConfig::default(), book),
            first_ms,
            started_wall_ms: None,
            speed: speed.max(0.01),
            now_ms: first_ms,
            alerts: Vec::new(),
            skipped_links,
        })
    }

    fn done(&self) -> bool {
        self.next >= self.packets.len()
    }
}

impl Source for Replay {
    fn step(&mut self, wall_ms: u64) {
        let started = *self.started_wall_ms.get_or_insert(wall_ms);
        let target = self.first_ms + (wall_ms.saturating_sub(started) as f64 * self.speed) as u64;
        let mut events = Vec::new();
        while let Some((code, t_ms, data)) = self.packets.get(self.next).filter(|p| p.1 <= target) {
            if let Some((link, data)) = ip_view(*code, data) {
                self.pipeline.push(link, *t_ms, data, &mut |e| events.push(e));
            }
            self.next += 1;
        }
        for e in &events {
            self.tracker.push(e);
        }
        if !self.done() {
            self.now_ms = target;
        }
        self.tracker.tick(self.now_ms);
        self.alerts.extend(self.tracker.drain_alerts());
    }

    fn snapshot(&self) -> RadarSnapshot {
        self.tracker.snapshot(self.now_ms)
    }

    fn drain_alerts(&mut self) -> Vec<Alert> {
        std::mem::take(&mut self.alerts)
    }

    fn status(&self) -> Status {
        let total = self.packets.len();
        if total == 0 {
            let extra = if self.skipped_links > 0 { format!(" ({} packets with an unreadable link type)", self.skipped_links) } else { String::new() };
            return Status { text: format!("the capture holds no packets{extra}"), health: Health::Error };
        }
        if self.done() {
            Status { text: format!("replay finished, {total} packets"), health: Health::Waiting }
        } else {
            let secs = self.now_ms.saturating_sub(self.first_ms) / 1000;
            Status { text: format!("replay {}:{:02}, packet {} of {total}", secs / 60, secs % 60, self.next), health: Health::Good }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_joins_ports() {
        assert_eq!(bpf_filter(&[13328]), "tcp port 13328");
        assert_eq!(bpf_filter(&[13328, 7777]), "tcp port 13328 or tcp port 7777");
    }

    #[test]
    fn permission_errors_say_how_to_fix_them() {
        assert!(permission_hint("socket: Operation not permitted").contains("setcap cap_net_raw,cap_net_admin=eip"));
        assert!(permission_hint("You don't have permission to capture on that device").contains("setcap"));
        assert_eq!(permission_hint("no such device"), "Capture failed: no such device");
    }

    /// A Linux cooked (`any` device) capture of the game port: your position, then a cube spawn in an LZ4
    /// bundle, the way the server sends it.
    fn linux_capture() -> Vec<u8> {
        use aion_net::frame::{encode_bundle, encode_frame};
        use cube_core::opcodes as op;
        let hex = |s: &str| (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect::<Vec<u8>>();
        // The cube of cube-core's spawn fixture stands at (-50 205, 126 283, 15 449); you stand 30 m west,
        // 40 m south and 12 m lower: 50 m away, north east, 12 m up.
        let mut teleport = vec![0u8; 6];
        for v in [-50_205.0f32 - 3_000.0, 126_283.0 + 4_000.0, 15_449.0 - 1_200.0] {
            teleport.extend_from_slice(&v.to_le_bytes());
        }
        teleport.extend_from_slice(&hex("05000000a805"));
        let spawn = hex("e5ff030400a74f1200001d44c780a5f647006471460000000000000000010101aae4a534a101000000000000000000002c4d4502070000");
        let mut stream = encode_frame(op::TELEPORT, &teleport);
        stream.extend(encode_bundle(&encode_frame(op::OBJECT_SPAWN, &spawn)));

        let segment = |seq: u32, payload: &[u8], syn: bool| {
            let mut b = etherparse::PacketBuilder::ipv4([203, 0, 113, 7], [192, 168, 1, 20], 64).tcp(13328, 50_000, seq, 1000);
            if syn {
                b = b.syn();
            }
            // Linux cooked v1 header as the `any` device writes it for loopback (hardware type 772), which the
            // decoder's own reader would refuse.
            let mut out = vec![0, 0, 0x03, 0x04, 0, 6, 0, 0, 0, 0, 0, 0, 0, 0, 0x08, 0x00];
            b.write(&mut out, payload).unwrap();
            out
        };
        let mut file = Vec::new();
        for v in [0xa1b2_c3d4u32, 0x0004_0002, 0, 0, 65_535, DLT_LINUX_SLL] {
            file.extend_from_slice(&v.to_le_bytes());
        }
        let half = stream.len() / 2;
        for (i, data) in [segment(100, &[], true), segment(101, &stream[..half], false), segment(101 + half as u32, &stream[half..], false)].into_iter().enumerate() {
            for v in [1_791_884_650u32, i as u32 * 100_000, data.len() as u32, data.len() as u32] {
                file.extend_from_slice(&v.to_le_bytes());
            }
            file.extend_from_slice(&data);
        }
        file
    }

    #[test]
    fn a_linux_capture_puts_the_cube_on_the_radar() {
        let path = std::env::temp_dir().join(format!("cube-overlay-linux-{}.pcap", std::process::id()));
        std::fs::write(&path, linux_capture()).unwrap();
        let mut replay = Replay::load(&path, vec![13328], CubeBook::default(), 1.0).unwrap();
        let _ = std::fs::remove_file(&path);
        replay.step(1_000);
        replay.step(2_000);
        let alerts = replay.drain_alerts();
        assert_eq!(alerts.len(), 1, "{alerts:?}");
        assert_eq!(alerts[0].kind, cube_core::snapshot::AlertKind::Appeared);
        let snap = replay.snapshot();
        let cube = snap.open_cubes().next().expect("the cube is on the radar");
        assert!((cube.distance_m.unwrap() - 50.0).abs() < 0.1, "{:?}", cube.distance_m);
        assert!((cube.height_m.unwrap() - 12.0).abs() < 0.1, "{:?}", cube.height_m);
        assert_eq!(cube_core::geo::compass(cube.bearing.unwrap()), "NE");
        assert!(replay.status().text.starts_with("replay finished"), "{:?}", replay.status());
    }

    #[test]
    fn cooked_headers_are_unwrapped_to_ip() {
        let ip = [0x45u8, 0, 0, 20];
        // v1: PPP (hardware type 512) carrying IPv4.
        let mut v1 = vec![0, 0, 0x02, 0x00, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x08, 0x00];
        v1.extend_from_slice(&ip);
        assert_eq!(ip_view(DLT_LINUX_SLL, &v1), Some((LinkType::Raw, &ip[..])));
        // v2: protocol first, then reserved, interface index, hardware type, packet type, address.
        let mut v2 = vec![0x86, 0xDD, 0, 0, 0, 0, 0, 3, 0xFF, 0xFE, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        v2.extend_from_slice(&ip);
        assert_eq!(ip_view(DLT_LINUX_SLL2, &v2), Some((LinkType::Raw, &ip[..])));
        assert_eq!(ip_view(DLT_LINUX_SLL2, &[0x08, 0x06, 0, 0]), None, "ARP or a short record");
        let ether = [0u8; 20];
        assert_eq!(ip_view(1, &ether), Some((LinkType::Ethernet, &ether[..])));
        assert_eq!(ip_view(147, &ether), None, "a link type nobody reads");
    }

    #[test]
    fn hints_when_no_cube_can_show_yet() {
        let book = CubeBook::default();
        assert!(zone_hint(None, &book).unwrap().contains("map unknown"));
        assert_eq!(zone_hint(Some(1010), &book), None, "Verteron cubes are known");
        assert_eq!(zone_hint(Some(600_072), &book), None, "no cubes in dungeons");
        assert_eq!(zone_hint(Some(1110), &book), None, "probably Altgard, whose template is known: no wrong hint");
    }

    #[test]
    fn manual_templates_join_the_book() {
        let store = TemplateStore::open(std::env::temp_dir().join("cube-overlay-no-such-dir/templates.json"));
        let b = book(&store, &[4_242_424]);
        assert!(b.is_cube(4_242_424));
        assert!(b.is_cube(1_200_039), "the built in templates stay");
    }
}
