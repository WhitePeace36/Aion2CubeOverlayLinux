//! Offline decoder: runs one or more pcap or pcapng captures, in order, through the pipeline and the cube tracker,
//! the way the app would have seen them live.
//!
//! ```text
//! replay <capture>... [--port 13328] [--opcodes] [--dump 3436] [--objects] [--template N]... [--limit N]
//!                     [--events [type]] [--json]
//! ```
//! Without a mode flag it prints the alerts and the cube log. `--objects` lists every object template the capture
//! spawned with what opened it, which is how a new cube template is found after a patch (docs/PROTOCOL.md).
//! Event types for `--events`: spawn, removed, open, position, zone, self, heartbeat.

use anyhow::{anyhow, Context};
use cube_core::capture_file::read_packets;
use cube_core::cubes::{CubeBook, CubeTemplate, Evidence, KEY_ITEM};
use cube_core::gamedata::GameData;
use cube_core::geo::{compass, Vec3, UNITS_PER_METRE};
use cube_core::net::LinkType;
use cube_core::opcodes;
use cube_core::parser::{EntityId, Event};
use cube_core::pipeline::{Pipeline, DEFAULT_PORT};
use cube_core::snapshot::{Alert, AlertKind, LogKind};
use cube_core::tracker::{Tracker, TrackerConfig};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

const EVENT_TYPES: &[&str] = &["spawn", "removed", "open", "position", "zone", "self", "heartbeat"];

struct Options {
    paths: Vec<String>,
    ports: Vec<u16>,
    show_opcodes: bool,
    dump: Option<u16>,
    objects: bool,
    templates: Vec<u32>,
    limit: usize,
    events: bool,
    event_type: Option<String>,
    json: bool,
}

fn usage() -> String {
    "usage: replay <capture.pcap|pcapng>... [--port 13328] [--opcodes] [--dump 3436] [--objects] [--template N]... [--limit N] [--events [spawn|removed|open|position|zone|self|heartbeat]] [--json]".into()
}

fn parse_args() -> anyhow::Result<Options> {
    let mut o = Options {
        paths: Vec::new(),
        ports: vec![DEFAULT_PORT],
        show_opcodes: false,
        dump: None,
        objects: false,
        templates: Vec::new(),
        limit: 50,
        events: false,
        event_type: None,
        json: false,
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "--port" => {
                i += 1;
                o.ports = vec![args.get(i).ok_or_else(|| anyhow!(usage()))?.parse()?];
            }
            "--opcodes" => o.show_opcodes = true,
            "--dump" => {
                i += 1;
                let s = args.get(i).ok_or_else(|| anyhow!(usage()))?;
                o.dump = Some(opcodes::parse(s).ok_or_else(|| anyhow!("bad opcode {s}"))?);
            }
            "--objects" => o.objects = true,
            "--template" => {
                i += 1;
                o.templates.push(args.get(i).ok_or_else(|| anyhow!(usage()))?.parse()?);
            }
            "--limit" => {
                i += 1;
                o.limit = args.get(i).ok_or_else(|| anyhow!(usage()))?.parse()?;
            }
            "--events" => {
                o.events = true;
                if let Some(next) = args.get(i + 1) {
                    if EVENT_TYPES.contains(&next.as_str()) {
                        o.event_type = Some(next.clone());
                        i += 1;
                    }
                }
            }
            "--json" => o.json = true,
            "-h" | "--help" => return Err(anyhow!(usage())),
            _ if a.starts_with("--") => return Err(anyhow!("{}\nunknown argument {a}", usage())),
            _ => o.paths.push(a.to_string()),
        }
        i += 1;
    }
    if o.paths.is_empty() {
        return Err(anyhow!(usage()));
    }
    Ok(o)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(" ")
}

fn clock(t_ms: u64, origin_ms: u64) -> String {
    let s = t_ms.saturating_sub(origin_ms) / 1000;
    format!("{:>2}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

fn metres(d: Option<f32>) -> String {
    d.map(|d| format!("{d:.0} m")).unwrap_or_else(|| "? m".into())
}

/// Per template: what `--objects` prints.
#[derive(Default)]
struct TemplateStats {
    spawns: u64,
    key_opens: u64,
    other_opens: u64,
    first_pos: Option<Vec3>,
    /// Closest planar distance between a spawn and your position at that moment, in game units.
    closest: Option<f32>,
}

fn print_alert(a: &Alert, origin_ms: u64) {
    let c = &a.cube;
    let what = match a.kind {
        AlertKind::Appeared => "CUBE NEARBY",
        AlertKind::StillThere => "cube still there",
        AlertKind::OpenedByYou => "opened by you",
        AlertKind::OpenedByOther => "opened by someone else",
        AlertKind::Learned => "NEW TEMPLATE learned",
    };
    let direction = c.bearing.map(|b| format!(" {}", compass(b))).unwrap_or_default();
    let age = c.age_ms.map(|a| format!(", spawned {} s ago", a / 1000)).unwrap_or_default();
    println!("{} {what}: cube {} template {}, {}{direction}{age}", clock(a.t_ms, origin_ms), c.id, c.template, metres(c.distance_m));
}

fn main() -> anyhow::Result<()> {
    let opts = parse_args()?;
    let mut pipe = Pipeline::new(opts.ports.clone());
    let dumped = Arc::new(Mutex::new(0usize));
    if let Some(want) = opts.dump {
        let limit = opts.limit;
        let dumped = dumped.clone();
        pipe.tap = Some(Box::new(move |t_ms, op, body| {
            if op == want {
                let mut n = dumped.lock().unwrap();
                if *n < limit {
                    *n += 1;
                    println!("{t_ms} {} len {}: {}", opcodes::hex(op), body.len(), hex(body));
                }
            }
        }));
    }

    let mut book = CubeBook::default();
    for &template in &opts.templates {
        book.add(CubeTemplate { template, zone: None, evidence: Evidence::Manual });
    }
    let mut tracker = Tracker::new(TrackerConfig::default(), book);
    let mut alerts: Vec<Alert> = Vec::new();
    let mut printed_events = 0usize;
    let (mut first_ms, mut last_ms) = (u64::MAX, 0u64);
    let mut last_tick = 0u64;
    let mut unknown_links: BTreeMap<u32, u64> = BTreeMap::new();
    let mut pending: Vec<Event> = Vec::new();
    let mut templates: BTreeMap<u32, TemplateStats> = BTreeMap::new();
    let mut object_templates: HashMap<EntityId, u32> = HashMap::new();
    let mut maps_seen: BTreeMap<u32, u64> = BTreeMap::new();

    for path in &opts.paths {
        let bytes = std::fs::read(path).with_context(|| format!("reading {path}"))?;
        read_packets(&bytes, |link_code, t_ms, data| {
            first_ms = first_ms.min(t_ms);
            last_ms = last_ms.max(t_ms);
            let Some(link) = LinkType::from_pcap(link_code) else {
                *unknown_links.entry(link_code).or_default() += 1;
                return;
            };
            pipe.push(link, t_ms, data, &mut |ev| pending.push(ev));
            // A tick every 250 ms of capture time, as the terminal app does.
            if t_ms.saturating_sub(last_tick) >= 250 {
                tracker.tick(t_ms);
                last_tick = t_ms;
            }
            for ev in pending.drain(..) {
                if opts.events && printed_events < opts.limit {
                    let wanted = match &opts.event_type {
                        Some(kind) => ev.kind() == kind,
                        None => !matches!(ev, Event::Heartbeat { .. } | Event::Position { .. }),
                    };
                    if wanted {
                        printed_events += 1;
                        println!("{}", serde_json::to_string(&ev).unwrap_or_default());
                    }
                }
                match &ev {
                    Event::ObjectSpawn { id, template, pos, .. } => {
                        object_templates.insert(*id, *template);
                        let me = tracker.me().map(|m| m.pos);
                        let s = templates.entry(*template).or_default();
                        s.spawns += 1;
                        s.first_pos.get_or_insert(*pos);
                        if let Some(me) = me {
                            let d = me.planar_distance(pos);
                            s.closest = Some(s.closest.map_or(d, |c| c.min(d)));
                        }
                    }
                    Event::OpenDone { object, item, .. } => {
                        if let Some(t) = object_templates.get(object) {
                            let s = templates.entry(*t).or_default();
                            if *item == KEY_ITEM {
                                s.key_opens += 1;
                            } else {
                                s.other_opens += 1;
                            }
                        }
                    }
                    Event::ZoneChange { map_id, .. } => *maps_seen.entry(*map_id).or_default() += 1,
                    _ => {}
                }
                tracker.push(&ev);
                alerts.extend(tracker.drain_alerts());
            }
        })?;
    }

    if first_ms == u64::MAX {
        return Err(anyhow!("no packets in {}", opts.paths.join(", ")));
    }
    let ps = &pipe.parser.stats;
    let st = &pipe.stats;
    eprintln!("capture: {} packets over {:.1}s from {} file(s)", st.packets_seen, (last_ms - first_ms) as f64 / 1000.0, opts.paths.len());
    eprintln!(
        "frames: {} ({} bundles, {} bad), resyncs {}, tcp gaps {}; packets {} ({} heartbeats)",
        st.frames.frames, st.frames.bundles, st.frames.bad_bundles, st.frames.resyncs, st.tcp_gaps, ps.packets, ps.heartbeats
    );
    eprintln!(
        "objects: {} spawns, {} removals, {} openings done, {} rejected; positions: {} fixes, {} buffs without one; zone changes {}, self records {}",
        ps.object_spawns, ps.object_removals, ps.opens_done, ps.object_rejected, ps.position_fixes, ps.buffs_without_position, ps.zone_changes, ps.self_records
    );
    if !unknown_links.is_empty() {
        eprintln!("unsupported link types: {unknown_links:?}");
    }

    if opts.show_opcodes {
        let mut counts: Vec<(u16, u64)> = ps.opcode_counts.iter().map(|(k, v)| (*k, *v)).collect();
        counts.sort_by_key(|c| std::cmp::Reverse(c.1));
        println!("\nopcode   name           count      bytes");
        for (op, n) in counts.iter().take(opts.limit.max(20)) {
            println!("{}    {:<13} {:>7} {:>10}", opcodes::hex(*op), opcodes::name(*op), n, ps.opcode_bytes.get(op).copied().unwrap_or(0));
        }
        return Ok(());
    }
    if opts.objects {
        println!("\ntemplate   spawns  key opens  other opens  closest  first position             cube?");
        let book = tracker.book();
        let mut rows: Vec<(&u32, &TemplateStats)> = templates.iter().collect();
        rows.sort_by(|a, b| b.1.key_opens.cmp(&a.1.key_opens).then(b.1.spawns.cmp(&a.1.spawns)));
        for (template, s) in rows.into_iter().take(opts.limit.max(20)) {
            let pos = s.first_pos.map(|p| format!("({:.0}, {:.0}, {:.0})", p.x, p.y, p.z)).unwrap_or_default();
            let closest = s.closest.map(|d| format!("{:.0} m", d / UNITS_PER_METRE)).unwrap_or_else(|| "?".into());
            let cube = match book.get(*template).map(|t| t.evidence) {
                Some(Evidence::Verified) => "yes",
                Some(Evidence::Hypothesis) => "yes (hypothesis)",
                Some(Evidence::Learned) => "yes (learned here)",
                Some(Evidence::Manual) => "yes (--template)",
                None if s.key_opens > 0 => "LIKELY: opened with the cube key",
                None => "",
            };
            println!("{template:<10} {:>6} {:>10} {:>12} {closest:>8}  {pos:<26} {cube}", s.spawns, s.key_opens, s.other_opens);
        }
        return Ok(());
    }
    if opts.dump.is_some() || opts.events {
        return Ok(());
    }

    let snap = tracker.snapshot(last_ms);
    if opts.json {
        println!("{}", serde_json::to_string_pretty(&snap)?);
        return Ok(());
    }
    let gd = GameData::get();
    let maps: Vec<String> = maps_seen.keys().map(|m| format!("{} ({m})", gd.map(*m).name)).collect();
    let who = snap.character.as_ref().map(|c| c.name.clone()).or_else(|| snap.self_id.map(|id| format!("entity {id}{}", if snap.self_guessed { " (guessed)" } else { "" })));
    println!("\nmaps loaded: {}", if maps.is_empty() { "none (the capture started inside a zone)".into() } else { maps.join(", ") });
    println!("you: {}", who.unwrap_or_else(|| "not identified".into()));
    println!(
        "cubes: {} seen, {} opened by you, {} by others, {} templates learned, {} key openings of unknown objects",
        snap.counts.cubes_seen, snap.counts.opened_by_you, snap.counts.opened_by_others, snap.counts.templates_learned, snap.counts.key_opens_unknown
    );
    if alerts.is_empty() {
        println!("\nno cube in this capture. `replay <capture> --objects` lists every object template it saw.");
        return Ok(());
    }
    println!("\nalerts:");
    for a in &alerts {
        print_alert(a, first_ms);
    }
    println!("\nlog:");
    for l in &snap.log {
        let who = match (l.actor, l.by_you) {
            (_, true) => " by you".to_string(),
            (Some(a), false) => format!(" by entity {a}"),
            (None, _) => String::new(),
        };
        let kind = match l.kind {
            LogKind::Appeared => "appeared",
            LogKind::LeftView => "left view",
            LogKind::BackInView => "back in view",
            LogKind::OpeningStarted => "opening",
            LogKind::Opened => "opened",
            LogKind::Gone => "gone",
            LogKind::Learned => "learned",
        };
        println!("  {} cube {:<7} {kind:<12} {:>6}{who}", clock(l.t_ms, first_ms), l.cube, metres(l.distance_m));
    }
    Ok(())
}
