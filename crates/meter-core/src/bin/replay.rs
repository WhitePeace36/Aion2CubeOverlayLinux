//! Offline decoder: runs a pcap or pcapng capture through the pipeline.
//!
//! ```text
//! replay <capture> [--port 13328] [--opcodes] [--dump 0438] [--limit N] [--events [type]] [--summary]
//!        [--names] [--spawns] [--party] [--encounters] [--detail] [--flows] [--json]
//! ```
//! Event types for `--events`: damage, heal, cast, miss, identity, spawn, owner, death, party,
//! zone, hp, pong. `--encounters` runs the events through the combat tracker and prints every
//! encounter; `--detail` adds per actor hit statistics and the top skills.

use anyhow::{anyhow, Context};
use meter_core::combat::{describe, format_duration, Tracker};
use meter_core::gamedata::GameData;
use meter_core::net::LinkType;
use meter_core::pipeline::{Pipeline, DEFAULT_PORT};
use meter_core::snapshot::Snapshot;
use meter_core::{opcodes, parser::Event};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

const EVENT_TYPES: &[&str] = &["damage", "heal", "cast", "miss", "identity", "spawn", "owner", "death", "party", "zone", "hp", "pong"];

struct Options {
    path: String,
    ports: Vec<u16>,
    show_opcodes: bool,
    dump: Option<u16>,
    limit: usize,
    events: bool,
    event_type: Option<String>,
    summary: bool,
    names: bool,
    spawns: bool,
    party: bool,
    encounters: bool,
    detail: bool,
    flows: bool,
    json: bool,
}

fn usage() -> String {
    "usage: replay <capture.pcap|pcapng> [--port 13328] [--opcodes] [--dump 0438] [--limit N] [--events [type]] [--summary] [--names] [--spawns] [--party] [--encounters] [--detail] [--flows] [--json]".into()
}

fn parse_args() -> anyhow::Result<Options> {
    let mut o = Options {
        path: String::new(),
        ports: vec![DEFAULT_PORT],
        show_opcodes: false,
        dump: None,
        limit: 50,
        events: false,
        event_type: None,
        summary: false,
        names: false,
        spawns: false,
        party: false,
        encounters: false,
        detail: false,
        flows: false,
        json: false,
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "--port" => {
                i += 1;
                o.ports = vec![args.get(i).ok_or_else(usage_err)?.parse()?];
            }
            "--opcodes" => o.show_opcodes = true,
            "--dump" => {
                i += 1;
                let s = args.get(i).ok_or_else(usage_err)?;
                o.dump = Some(opcodes::parse(s).ok_or_else(|| anyhow!("bad opcode {s}"))?);
            }
            "--limit" => {
                i += 1;
                o.limit = args.get(i).ok_or_else(usage_err)?.parse()?;
            }
            "--events" => {
                o.events = true;
                if let Some(t) = args.get(i + 1) {
                    if EVENT_TYPES.contains(&t.as_str()) {
                        o.event_type = Some(t.clone());
                        i += 1;
                    }
                }
            }
            "--summary" => o.summary = true,
            "--names" => o.names = true,
            "--spawns" => o.spawns = true,
            "--party" => o.party = true,
            "--encounters" => o.encounters = true,
            "--detail" => o.detail = true,
            "--flows" => o.flows = true,
            "--json" => o.json = true,
            _ if o.path.is_empty() && !a.starts_with("--") => o.path = a.to_string(),
            _ => return Err(anyhow!("{}\nunknown argument {a}", usage())),
        }
        i += 1;
    }
    if o.path.is_empty() {
        return Err(anyhow!(usage()));
    }
    Ok(o)
}

fn usage_err() -> anyhow::Error {
    anyhow!(usage())
}

/// Calls `f(link_type_code, t_ms, data)` for every packet in a pcap or pcapng file.
fn read_packets(bytes: &[u8], mut f: impl FnMut(u32, u64, &[u8])) -> anyhow::Result<()> {
    if bytes.len() >= 4 && bytes[..4] == [0x0A, 0x0D, 0x0D, 0x0A] {
        use pcap_file::pcapng::{Block, PcapNgReader};
        let mut r = PcapNgReader::new(std::io::Cursor::new(bytes)).context("pcapng header")?;
        let mut links: Vec<u32> = Vec::new();
        while let Some(block) = r.next_block() {
            match block.context("pcapng block")? {
                Block::InterfaceDescription(idb) => links.push(u32::from(idb.linktype)),
                Block::EnhancedPacket(epb) => {
                    let link = links.get(epb.interface_id as usize).copied().unwrap_or(1);
                    f(link, epb.timestamp.as_millis() as u64, &epb.data);
                }
                _ => {}
            }
        }
    } else {
        use pcap_file::pcap::PcapReader;
        let mut r = PcapReader::new(std::io::Cursor::new(bytes)).context("pcap header")?;
        let link = u32::from(r.header().datalink);
        while let Some(pkt) = r.next_packet() {
            match pkt {
                Ok(pkt) => f(link, pkt.timestamp.as_millis() as u64, &pkt.data),
                Err(e) => {
                    // A capture still being written ends in a partial record; stop there.
                    eprintln!("stopped reading: {e}");
                    break;
                }
            }
        }
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(" ")
}

fn print_summary(events: &[Event], ps: &meter_core::parser::ParserStats) {
    let mut counts: BTreeMap<&'static str, u64> = BTreeMap::new();
    for ev in events {
        let key = match ev {
            Event::RemainHp { .. } => "remain_hp",
            Event::HpUpdate { .. } => "hp_update",
            other => other.kind(),
        };
        *counts.entry(key).or_default() += 1;
    }
    println!("\nevents:");
    for (k, n) in &counts {
        println!("  {k:<12} {n:>8}");
    }
    println!("  (no value records with hit type 0, not emitted: {})", ps.cast_starts);
    let r = &ps.rejected;
    println!("\ndamage records: {} attempted, {} rejected", ps.damage_records, ps.damage_rejected);
    println!(
        "  rejected: truncated {} bad_id {} bad_switch {} bad_skill {} bad_damage {} tail_failure {}",
        r.truncated, r.bad_id, r.bad_switch, r.bad_skill, r.bad_damage, r.tail_failure
    );
    println!(
        "  outcomes: damage {} heal {} cast {} miss {} resist {} spirit_link {} resource_restore {} self_hit {}",
        ps.damage_events, ps.heals, ps.casts, ps.misses, ps.resists, ps.spirit_links, ps.resource_restores, ps.self_hits
    );
    println!(
        "  tail_values {} tail_flag_bytes {} effect_lists {} trailing_bytes {} | pad_shifts {} crit_heuristics {} zero_trailers {} no_value_tail_failures {}",
        ps.tail_values, ps.tail_flag_bytes, ps.effect_lists, ps.trailing_bytes, ps.pad_shifts, ps.crit_heuristics, ps.zero_trailers, ps.no_value_tail_failures
    );
    println!("  hit types: {:?}", ps.hit_types);
    println!("  unknown hit types: {:?}", ps.unknown_hit_types);
    println!("  switch values: {:?}", ps.switch_values);
    println!(
        "dot: ticks {} heals {} effect_03 {} ignored {} rejected {} unlisted {}",
        ps.dot_ticks, ps.dot_heals, ps.dot_effect_03, ps.dot_ignored, ps.dot_rejected, ps.dot_unlisted
    );
    println!(
        "identity: {} ({} self, {} embedded, {} rejected), bundles {}, rosters {} ({} embedded, {} rejected)",
        ps.identities, ps.self_identities, ps.embedded_identities, ps.identity_rejected, ps.embedded_bundles, ps.rosters, ps.embedded_rosters, ps.roster_rejected
    );
    println!(
        "spawns: {} ({} no code, {} owners, {} names, {} names rejected, {} folded, {} rejected), deaths {} ({} rejected), zones {} ({} rejected)",
        ps.spawns, ps.spawn_no_code, ps.spawn_owners, ps.spawn_names, ps.spawn_names_rejected, ps.spawn_folded, ps.spawn_rejected, ps.deaths, ps.death_rejected, ps.zone_changes, ps.map_rejected
    );
    println!(
        "hp: remain {} update {} ({} rejected), owner links {} (ignored {}), pongs {} ({} rejected), hint_self_id {:?}",
        ps.remain_hp, ps.hp_updates, ps.hp_rejected, ps.owner_links, ps.ownership_ignored, ps.pongs, ps.pong_rejected, ps.hint_self_id
    );

    // Per actor hit distribution: the largest hit against the median shows a misread value at once.
    let mut per_actor: BTreeMap<u32, Vec<u64>> = BTreeMap::new();
    for ev in events {
        if let Event::Damage { actor, amount, flags, .. } = ev {
            if !flags.dot {
                per_actor.entry(*actor).or_default().push(*amount);
            }
        }
    }
    let mut rows: Vec<(u32, usize, u64, u64)> = per_actor
        .into_iter()
        .map(|(a, mut v)| {
            v.sort_unstable();
            (a, v.len(), v[v.len() / 2], *v.last().unwrap_or(&0))
        })
        .collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r.1));
    println!("\n{:<10} {:>6} {:>10} {:>10} {:>8}", "actor", "hits", "median", "max", "ratio");
    for (a, n, med, max) in rows.iter().take(15) {
        println!("{a:<10} {n:>6} {med:>10} {max:>10} {:>8.1}", *max as f64 / (*med).max(1) as f64);
    }
}

/// Occurrences, server, job, level and self flag of one named id.
type NameRow = (u64, u16, u32, u32, bool);

fn print_names(events: &[Event]) {
    let gd = GameData::get();
    let mut seen: BTreeMap<(u32, String), NameRow> = BTreeMap::new();
    for ev in events {
        if let Event::Identity { id, name, server, job, class_id, level, is_self, .. } = ev {
            let e = seen.entry((*id, name.clone())).or_insert((0, *server, *job, *level, *is_self));
            e.0 += 1;
            e.1 = *server;
            e.3 = (*level).max(e.3);
            let _ = class_id;
        }
    }
    println!("\n{:<9} {:<18} {:<14} {:>6} {:>6} {:>5} {:>5}", "id", "name", "class", "job", "server", "lvl", "n");
    for ((id, name), (n, server, job, level, is_self)) in &seen {
        let class = gd.class_from_job(*job).and_then(|c| gd.class_name(c)).unwrap_or("?");
        println!("{id:<9} {name:<18} {class:<14} {job:>6} {server:>6} {level:>5} {n:>5}{}", if *is_self { "  self" } else { "" });
    }
}

fn print_spawns(events: &[Event]) {
    let gd = GameData::get();
    let mut by_code: BTreeMap<(u32, u8), (u64, Option<u64>, u64)> = BTreeMap::new();
    for ev in events {
        if let Event::NpcSpawn { npc_code, kind, max_hp, owner, .. } = ev {
            let e = by_code.entry((*npc_code, *kind)).or_insert((0, None, 0));
            e.0 += 1;
            e.1 = (*max_hp).or(e.1);
            if owner.is_some() {
                e.2 += 1;
            }
        }
    }
    println!("\n{:<9} {:>4} {:<36} {:>5} {:>5} {:>10} {:>6}", "code", "kind", "name", "boss", "n", "max hp", "owned");
    for ((code, kind), (n, hp, owned)) in &by_code {
        let name = gd.npc_name(*code);
        let boss = if gd.is_boss(*code) { "yes" } else { "" };
        let hp = hp.map(|h| h.to_string()).unwrap_or_default();
        println!("{code:<9} {kind:>4X} {name:<36} {boss:>5} {n:>5} {hp:>10} {owned:>6}");
    }
}

fn print_party(events: &[Event]) {
    let gd = GameData::get();
    let mut last: Option<String> = None;
    for ev in events {
        if let Event::Party { t_ms, members } = ev {
            let key = members.iter().map(|m| format!("{}:{}:{}", m.name, m.class_id, m.level)).collect::<Vec<_>>().join(",");
            if last.as_deref() == Some(&key) {
                continue;
            }
            last = Some(key);
            println!("\n[{t_ms}] party of {}", members.len());
            for m in members {
                println!(
                    "  {:<18} {:<14} job {:>3} lvl {:>3} gear {:>6} cp {:>9}",
                    m.name,
                    gd.class_name(m.class_id).unwrap_or("?"),
                    m.job,
                    m.level,
                    m.gear_score,
                    m.combat_power
                );
            }
        }
    }
}

/// `HH:MM:SS` UTC of a Unix millisecond timestamp.
fn clock(t_ms: u64) -> String {
    let s = (t_ms / 1000) % 86_400;
    format!("{:02}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
}

/// Prints the encounters a tracker produced (`finished`, in order) and whatever is still live.
fn print_encounters(tr: &Tracker, mut encounters: Vec<Snapshot>, detail: bool) {
    if let Some(live) = tr.snapshot().filter(|s| s.active) {
        encounters.push(live);
    }
    println!("\n{} encounters, self {:?} (id {:?})", encounters.len(), tr.self_name(), tr.self_id());
    for s in &encounters {
        let boss = s.boss.as_ref();
        let boss_name = boss.map(|b| b.name.as_str()).unwrap_or("no target");
        let boss_flag = match boss {
            Some(b) if b.is_boss => " [boss]",
            Some(b) if b.is_dummy => " [dummy]",
            _ => "",
        };
        let hp = boss.and_then(|b| b.max_hp).map(|m| format!(" max hp {m}")).unwrap_or_default();
        let end = match s.end_reason {
            Some(r) => serde_json::to_string(&r).unwrap_or_default().trim_matches('"').to_owned(),
            None => "live".to_owned(),
        };
        let players = s.actors.iter().filter(|a| a.is_player).count();
        println!(
            "\n=== #{} {} | map {} {} | {}{boss_flag}{hp} | {end} | {} | party dps {:.0} | {players} players, {} deaths, {} targets",
            s.id,
            clock(s.started_ms),
            s.map_id.map(|m| m.to_string()).unwrap_or_else(|| "?".into()),
            s.map_name.as_deref().unwrap_or("?"),
            boss_name,
            format_duration(s.duration_ms),
            s.party_dps,
            s.deaths.len(),
            s.targets.len()
        );
        print!("{}", describe(s));
        if !detail {
            continue;
        }
        for a in &s.actors {
            let mean = if a.hits > 0 { a.damage as f64 / a.hits as f64 } else { 0.0 };
            println!(
                "    {:<18} id {:<7} {}{}{} hits {:>5} max {:>7} mean {:>7.0} x{:<5.1} casts {:>4} crit {:>4.1}% heal {:>8} taken {:>8} deaths {}",
                a.name,
                a.id,
                if a.is_self { "self " } else { "" },
                if a.is_party { "party " } else { "" },
                a.level.map(|l| format!("lvl {l} ")).unwrap_or_default(),
                a.hits,
                a.max_hit,
                mean,
                if mean > 0.0 { a.max_hit as f64 / mean } else { 0.0 },
                a.casts,
                a.crit_rate * 100.0,
                a.healing,
                a.damage_taken,
                a.deaths
            );
            for sk in a.skills.iter().take(4) {
                println!(
                    "        {:<28} {:>10} ({:>4.1}%) hits {:>4} max {:>7} avg {:>7.0} casts {:>3}",
                    format!("{} {}", sk.skill_id, sk.name),
                    sk.damage,
                    sk.share * 100.0,
                    sk.hits,
                    sk.max_hit,
                    sk.avg_hit,
                    sk.casts
                );
            }
            if a.targets.len() > 1 {
                let others: Vec<String> = a.targets.iter().skip(1).take(4).map(|t| format!("{} {}", t.name, t.damage)).collect();
                println!("        other targets: {}", others.join(", "));
            }
        }
        for t in &s.targets {
            if Some(t.id) == boss.map(|b| b.id) {
                continue;
            }
            println!(
                "    target {:<28} id {:<7} code {:<8} taken {:>10}{}{}",
                t.name,
                t.id,
                t.npc_code.map(|c| c.to_string()).unwrap_or_default(),
                t.damage_taken,
                if t.is_boss { " boss" } else { "" },
                if t.dead { " dead" } else { "" }
            );
        }
        for d in &s.deaths {
            println!("    death {} {} ({})", clock(d.t_ms), d.name, if d.is_player { "player" } else { "npc" });
        }
    }
    println!("\ntracker: {:?}", tr.stats);
}

fn main() -> anyhow::Result<()> {
    let opts = parse_args()?;
    let bytes = std::fs::read(&opts.path).with_context(|| format!("reading {}", opts.path))?;

    let mut pipe = Pipeline::new(opts.ports.clone());
    let dumped = Arc::new(Mutex::new(0usize));
    if let Some(want) = opts.dump {
        let dumped = dumped.clone();
        let limit = opts.limit;
        pipe.tap = Some(Box::new(move |t_ms, op, body| {
            if op != want {
                return;
            }
            let mut n = dumped.lock().expect("lock");
            if *n < limit {
                println!("[{t_ms}] {} len={} | {}", opcodes::hex(op), body.len(), hex(body));
            }
            *n += 1;
        }));
    }

    let mut events: Vec<Event> = Vec::new();
    let mut unknown_links: BTreeMap<u32, u64> = BTreeMap::new();
    let (mut first_ms, mut last_ms) = (u64::MAX, 0u64);
    let keep_events = opts.events || opts.summary || opts.names || opts.spawns || opts.party;
    // The tracker runs inline with the pipeline, the way the shell does: a tick every 250 ms of capture
    // time, the parser's self hint handed over after every packet, a last tick ten minutes after the
    // final packet so idle encounters close.
    let mut tracker = opts.encounters.then(Tracker::default);
    let mut finished: Vec<Snapshot> = Vec::new();
    let mut last_tick = 0u64;
    let mut pending: Vec<Event> = Vec::new();
    read_packets(&bytes, |link_code, t_ms, data| {
        first_ms = first_ms.min(t_ms);
        last_ms = last_ms.max(t_ms);
        let Some(link) = LinkType::from_pcap(link_code) else {
            *unknown_links.entry(link_code).or_default() += 1;
            return;
        };
        pipe.push(link, t_ms, data, &mut |ev| pending.push(ev));
        if let Some(tr) = tracker.as_mut() {
            if t_ms.saturating_sub(last_tick) >= 250 {
                tr.tick(t_ms);
                last_tick = t_ms;
                finished.extend(tr.drain_finished());
            }
            tr.set_self_hint(pipe.parser.stats.hint_self_id);
            for ev in &pending {
                tr.push(ev);
            }
        }
        if keep_events {
            events.append(&mut pending);
        } else {
            pending.clear();
        }
    })?;
    if let Some(tr) = tracker.as_mut() {
        tr.tick(last_ms.saturating_add(600_000));
        finished.extend(tr.drain_finished());
    }

    if let Some(want) = opts.dump {
        let n = *dumped.lock().expect("lock");
        println!("{} packets with opcode {} ({} shown)", n, opcodes::hex(want), n.min(opts.limit));
    }

    let ps = &pipe.parser.stats;
    let span_s = if last_ms > first_ms { (last_ms - first_ms) as f64 / 1000.0 } else { 0.0 };
    let hb_span_s = match (ps.first_heartbeat_ms, ps.last_heartbeat_ms) {
        (Some(a), Some(b)) if b > a => (b - a) as f64 / 1000.0,
        _ => 0.0,
    };

    if opts.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "pipeline": pipe.stats,
                "parser": ps,
                "flows": pipe.flow_summary(),
                "span_s": span_s,
            }))?
        );
    } else {
        println!("capture: {} packets over {span_s:.1}s", pipe.stats.packets_seen);
        println!(
            "segments: {} server->client, {} client->server, {} flows, {} tcp gaps",
            pipe.stats.segments_server_to_client, pipe.stats.segments_client_to_server, pipe.stats.flows, pipe.stats.tcp_gaps
        );
        let f = pipe.stats.frames;
        println!(
            "frames: {} ({} bundles, {} bad), resyncs {}, hunts {}, dropped bytes {}",
            f.frames, f.bundles, f.bad_bundles, f.resyncs, f.hunts, f.dropped_bytes
        );
        println!(
            "packets: {} total, {} heartbeats ({:.1}/s over {hb_span_s:.1}s)",
            ps.packets,
            ps.heartbeats,
            if hb_span_s > 0.0 { ps.heartbeats as f64 / hb_span_s } else { 0.0 }
        );
        if !unknown_links.is_empty() {
            println!("unknown link types skipped: {unknown_links:?}");
        }
    }

    if opts.flows {
        for fl in pipe.flow_summary() {
            println!(
                "flow {} -> {}: {} segments, {} bytes, syn={} gaps={} frames={} resyncs={} closed={}",
                fl.server, fl.client, fl.segments, fl.bytes, fl.saw_syn, fl.gaps, fl.frames.frames, fl.frames.resyncs, fl.closed
            );
        }
    }

    if opts.show_opcodes {
        let mut rows: Vec<(u16, u64, u64)> =
            ps.opcode_counts.iter().map(|(op, n)| (*op, *n, *ps.opcode_bytes.get(op).unwrap_or(&0))).collect();
        rows.sort_by_key(|r| std::cmp::Reverse(r.1));
        println!("\n{:<8} {:<12} {:>8} {:>10} {:>8}", "opcode", "name", "count", "bytes", "avg");
        for (op, n, b) in rows {
            println!("{:<8} {:<12} {:>8} {:>10} {:>8.1}", opcodes::hex(op), opcodes::name(op), n, b, b as f64 / n as f64);
        }
    }

    if opts.summary {
        print_summary(&events, ps);
    }
    if opts.names {
        print_names(&events);
    }
    if opts.spawns {
        print_spawns(&events);
    }
    if opts.party {
        print_party(&events);
    }
    if let Some(tr) = &tracker {
        print_encounters(tr, std::mem::take(&mut finished), opts.detail);
    }

    if opts.events {
        let mut shown = 0usize;
        let kinds: BTreeSet<&str> = EVENT_TYPES.iter().copied().collect();
        for ev in &events {
            if let Some(t) = &opts.event_type {
                if ev.kind() != t.as_str() || !kinds.contains(t.as_str()) {
                    continue;
                }
            }
            println!("{}", serde_json::to_string(ev)?);
            shown += 1;
        }
        println!("{shown} events");
    }

    Ok(())
}
