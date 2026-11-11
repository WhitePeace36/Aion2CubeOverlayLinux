//! aion-cube-overlay: the Hidden Cube radar and a DPS meter of aion2-tui as Wayland overlays for Linux.
//!
//! The decoding (`aion-net`, `cube-core` and `meter-core`) is aion2-tui's, unchanged. This crate adds Linux
//! capture, the radar with height, the per-enemy DPS meter and the overlay windows.

// Drawing helpers take coordinates, colours and sizes side by side; a struct for each would not read better.
#![allow(clippy::too_many_arguments)]

mod demo;
mod dps;
mod meter_demo;
mod meter_draw;
mod overlay;
mod radar;
mod source;
mod store;
mod text;
mod wayland;

use anyhow::{anyhow, bail, Context};
use overlay::{Overlay, Visibility};
use radar::Look;
use source::{CaptureOptions, Source};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use wayland::{Corner, MeterOptions, Toggles, WindowOptions};

const USAGE: &str = "\
aion-cube-overlay: Aion 2 Hidden Cube radar and DPS meter as Wayland overlays

USAGE:
  aion-cube-overlay [OPTIONS]

DATA (live capture by default; needs cap_net_raw, see README):
  --demo                  scripted demo data, no capture
  --replay <file>         play a saved .pcap or .pcapng at its own pace
  --speed <x>             replay speed (default 1)
  --interface <name>      capture on this interface only (default: all, the `any` device)
  --port <n>              game port, repeatable (default 13328)
  --template <n>          treat this object template as a cube, repeatable
  --data-dir <dir>        learned templates and cube spots (default ~/.local/share/aion-cube-overlay)

LOOK:
  --corner <where>        top-left (default), top, top-right, right, bottom-right, bottom, bottom-left, left
  --margin <x>,<y>        distance from the screen edges in pixels (default 24,120)
  --scale <f>             size of both overlays (default 1.25 for the meter, 1 for the radar; 1.5 or 2 for bigger text)
  --radar-scale <f>       size of the radar alone (default 1)
  --output <name>         monitor to show it on (default: the compositor's choice)
  --range <m>             radar radius in metres (default 100, the game sends cubes within about 100 m)
  --opacity <f>           background opacity, 0 to 1 (default 0.5)
  --north-offset <deg>    turn the map, if north turns out not to be the game's north (default 0)
  --no-spots              do not draw the spots where cubes were seen before
  --no-trail              do not draw your path
  --font <file.ttf>       font for the text (default: fontconfig's sans-serif)

DPS METER (its own window: drag it anywhere, click the tabs to switch mode):
  --meter-mode <m>        damage, dps (damage / time the enemy was attacked) or 5s (moving average of the
                          last 5 s); default: the mode it was left in, else 5s
  --meter-pos <x>,<y>     where to put it (default: where it was left, else 24,560)
  --meter-rows <n>        characters shown at most (default 8)
  --meter-scale <f>       size of the meter (default: as it was left; the mouse wheel over it resizes it)
  --no-meter              no DPS meter
  --no-radar              no cube radar

BEHAVIOUR:
  --always                keep the radar up even without cubes (default: shows itself when a cube is near)
  --alert-cmd <command>   run this with `sh -c` when a cube appears; gets CUBE_DISTANCE, CUBE_HEIGHT,
                          CUBE_DIRECTION and CUBE_ALERT in the environment
  --window                use a normal window even if the compositor has layer-shell
  --png <file>            draw one radar frame to a PNG and exit (with --demo: --at <seconds> into the demo)
  --meter-png <file>      the same for the meter
  -h, --help              this text

While running: SIGUSR1 hides or shows the radar, SIGUSR2 pins it on. For example:
  pkill -USR1 aion-cube-overl

Options can also go in ~/.config/aion-cube-overlay/options (one or more per line, # for comments); the command
line comes after them, so it wins.
";

struct Options {
    demo: bool,
    replay: Option<PathBuf>,
    speed: f64,
    interface: Option<String>,
    ports: Vec<u16>,
    templates: Vec<u32>,
    data_dir: Option<PathBuf>,
    corner: Corner,
    margin: (i32, i32),
    /// `--scale`: both overlays.
    scale: Option<f32>,
    radar_scale: Option<f32>,
    meter_scale: Option<f32>,
    output: Option<String>,
    look: Look,
    font: Option<PathBuf>,
    always: bool,
    alert_cmd: Option<String>,
    window: bool,
    png: Option<PathBuf>,
    meter_png: Option<PathBuf>,
    at_s: f64,
    meter_mode: Option<dps::Mode>,
    meter_pos: Option<(i32, i32)>,
    meter_rows: usize,
    meter: bool,
    radar: bool,
}

fn pair(flag: &str, v: &str) -> anyhow::Result<(i32, i32)> {
    let (x, y) = v.split_once(',').ok_or_else(|| anyhow!("{flag} {v}: use x,y, for example 24,120"))?;
    Ok((x.trim().parse().context(flag.to_string())?, y.trim().parse().context(flag.to_string())?))
}

fn parse_args(args: Vec<String>) -> anyhow::Result<Option<Options>> {
    let mut o = Options {
        demo: false,
        replay: None,
        speed: 1.0,
        interface: None,
        ports: Vec::new(),
        templates: Vec::new(),
        data_dir: None,
        corner: Corner::TopLeft,
        margin: (24, 120),
        scale: None,
        radar_scale: None,
        meter_scale: None,
        output: None,
        look: Look::default(),
        font: None,
        always: false,
        alert_cmd: None,
        window: false,
        png: None,
        meter_png: None,
        at_s: 16.0,
        meter_mode: None,
        meter_pos: None,
        meter_rows: 8,
        meter: true,
        radar: true,
    };
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        let mut value = || it.next().ok_or_else(|| anyhow!("{a} needs a value (see --help)"));
        match a.as_str() {
            "-h" | "--help" => return Ok(None),
            "--demo" => o.demo = true,
            "--replay" => o.replay = Some(PathBuf::from(value()?)),
            "--speed" => o.speed = value()?.parse().context("--speed")?,
            "--interface" => o.interface = Some(value()?),
            "--port" => o.ports.push(value()?.parse().context("--port")?),
            "--template" => o.templates.push(value()?.parse().context("--template")?),
            "--data-dir" => o.data_dir = Some(PathBuf::from(value()?)),
            "--corner" => {
                let v = value()?;
                o.corner = Corner::parse(&v).ok_or_else(|| anyhow!("--corner {v}: use top-left, top, top-right, right, bottom-right, bottom, bottom-left or left"))?;
            }
            "--margin" => o.margin = pair("--margin", &value()?)?,
            "--meter-pos" => o.meter_pos = Some(pair("--meter-pos", &value()?)?),
            "--meter-mode" => {
                let v = value()?;
                o.meter_mode = Some(dps::Mode::parse(&v).ok_or_else(|| anyhow!("--meter-mode {v}: use damage, dps or 5s"))?);
            }
            "--meter-rows" => o.meter_rows = value()?.parse::<usize>().context("--meter-rows")?.clamp(1, 24),
            "--no-meter" => o.meter = false,
            "--no-radar" => o.radar = false,
            "--meter-png" => o.meter_png = Some(PathBuf::from(value()?)),
            "--scale" => o.scale = Some(value()?.parse::<f32>().context("--scale")?.clamp(0.4, 4.0)),
            "--radar-scale" => o.radar_scale = Some(value()?.parse::<f32>().context("--radar-scale")?.clamp(0.4, 4.0)),
            "--meter-scale" => o.meter_scale = Some(value()?.parse::<f32>().context("--meter-scale")?.clamp(0.4, 4.0)),
            "--output" => o.output = Some(value()?),
            "--range" => o.look.range_m = value()?.parse::<f32>().context("--range")?.clamp(10.0, 5_000.0),
            "--opacity" => o.look.opacity = value()?.parse::<f32>().context("--opacity")?.clamp(0.0, 1.0),
            "--north-offset" => o.look.north_offset_deg = value()?.parse().context("--north-offset")?,
            "--no-spots" => o.look.show_spots = false,
            "--no-trail" => o.look.show_trail = false,
            "--font" => o.font = Some(PathBuf::from(value()?)),
            "--always" => o.always = true,
            "--alert-cmd" => o.alert_cmd = Some(value()?),
            "--window" => o.window = true,
            "--png" => o.png = Some(PathBuf::from(value()?)),
            "--at" => o.at_s = value()?.parse().context("--at")?,
            other => bail!("unknown option {other} (see --help)"),
        }
    }
    if o.demo && o.replay.is_some() {
        bail!("--demo and --replay do not go together");
    }
    if !o.meter && !o.radar {
        bail!("--no-meter and --no-radar leave nothing to show");
    }
    if o.ports.is_empty() {
        o.ports.push(cube_core::pipeline::DEFAULT_PORT);
    }
    Ok(Some(o))
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let options_file = store::default_config_dir().join("options");
    let mut args = match store::read_options_file(&options_file) {
        Ok(words) => words,
        Err(e) => {
            eprintln!("aion-cube-overlay: {e:#}");
            std::process::exit(2);
        }
    };
    if !args.is_empty() {
        log::info!("options from {}: {}", options_file.display(), args.join(" "));
    }
    args.extend(std::env::args().skip(1));
    match parse_args(args) {
        Ok(None) => print!("{USAGE}"),
        Ok(Some(options)) => {
            if let Err(e) = run(options) {
                eprintln!("aion-cube-overlay: {e:#}");
                std::process::exit(1);
            }
        }
        Err(e) => {
            eprintln!("aion-cube-overlay: {e:#}");
            std::process::exit(2);
        }
    }
}

fn run(o: Options) -> anyhow::Result<()> {
    let data_dir = o.data_dir.clone().unwrap_or_else(store::default_data_dir);
    let templates = store::TemplateStore::open(data_dir.join("templates.json"));
    if templates.len() > 0 {
        log::info!("{} cube templates learned earlier", templates.len());
    }
    let book = source::book(&templates, &o.templates);
    let now = source::now_ms();
    let source: Box<dyn Source> = if o.demo {
        Box::new(demo::Demo::new(now, book))
    } else if let Some(path) = &o.replay {
        Box::new(source::Replay::load(path, o.ports.clone(), book, o.speed)?)
    } else if o.png.is_some() || o.meter_png.is_some() {
        bail!("--png and --meter-png need --demo or --replay");
    } else {
        let spots = store::SpotStore::new(data_dir.join("spots.json"));
        Box::new(source::Live::start(CaptureOptions { interface: o.interface.clone(), ports: o.ports.clone() }, book, templates, spots))
    };
    let fonts = text::Fonts::load(o.font.as_deref());
    let visibility = if o.always { Visibility::Always } else { Visibility::Auto };
    let mut overlay = Overlay::new(source, o.look.clone(), fonts, o.alert_cmd.clone(), visibility);
    let settings_path = store::default_config_dir().join("meter.json");
    let settings = store::MeterSettings::load(&settings_path);
    overlay.dps.mode = o.meter_mode.or_else(|| dps::Mode::parse(&settings.mode)).unwrap_or(dps::Mode::Recent);
    overlay.meter_rows = o.meter_rows;
    overlay.meter_minimized = settings.minimized && o.meter_png.is_none();

    if o.png.is_some() || o.meter_png.is_some() {
        return write_png(&mut overlay, &o, now);
    }

    let toggles = Toggles { hide: Arc::new(AtomicBool::new(false)), always: Arc::new(AtomicBool::new(false)) };
    signal_hook::flag::register(signal_hook::consts::SIGUSR1, toggles.hide.clone())?;
    signal_hook::flag::register(signal_hook::consts::SIGUSR2, toggles.always.clone())?;
    let meter = o.meter.then(|| MeterOptions {
        pos: o.meter_pos.unwrap_or((settings.x, settings.y)),
        scale: o.meter_scale.or(o.scale).unwrap_or(settings.scale).clamp(0.4, 4.0),
        settings_path,
    });
    let options = WindowOptions {
        corner: o.corner,
        margin: o.margin,
        scale: o.radar_scale.or(o.scale).unwrap_or(store::DEFAULT_RADAR_SCALE),
        output: o.output.clone(),
        force_window: o.window,
        radar: o.radar,
        meter,
    };
    wayland::run(overlay, options, toggles)
}

/// One frame to a PNG: steps the source in 100 ms ticks up to `--at` seconds, as the overlay would live.
fn write_png(overlay: &mut Overlay, o: &Options, start: u64) -> anyhow::Result<()> {
    let end = start + (o.at_s.max(0.0) * 1000.0) as u64;
    let mut t = start;
    while t < end {
        overlay.step(t);
        t += 100;
    }
    overlay.step(end);
    if overlay.visibility == Visibility::Auto && !overlay.visible() {
        overlay.visibility = Visibility::Always;
    }
    let radar_scale = o.radar_scale.or(o.scale).unwrap_or(store::DEFAULT_RADAR_SCALE);
    let meter_scale = o.meter_scale.or(o.scale).unwrap_or(store::DEFAULT_SCALE);
    if let Some(path) = &o.png {
        let w = (radar::BASE_W * radar_scale).round() as u32;
        let h = (radar::BASE_H * radar_scale).round() as u32;
        let mut pix = tiny_skia::Pixmap::new(w, h).ok_or_else(|| anyhow!("bad size"))?;
        overlay.draw(&mut pix, radar_scale);
        pix.save_png(path)?;
        log::info!("wrote {}", path.display());
    }
    if let Some(path) = &o.meter_png {
        let w = (meter_draw::W * meter_scale).round() as u32;
        let h = (meter_draw::height(o.meter_rows) * meter_scale).round() as u32;
        let mut pix = tiny_skia::Pixmap::new(w, h).ok_or_else(|| anyhow!("bad size"))?;
        overlay.draw_meter(&mut pix, meter_scale);
        pix.save_png(path)?;
        log::info!("wrote {}", path.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn parses_the_options() {
        let o = parse_args(args("--demo --corner bottom-left --margin 10,20 --scale 1.5 --range 60 --north-offset 90 --port 7777")).unwrap().unwrap();
        assert!(o.demo);
        assert_eq!(o.corner, Corner::BottomLeft);
        assert_eq!(o.margin, (10, 20));
        assert_eq!(o.scale, Some(1.5));
        let sizes = parse_args(args("--radar-scale 2 --meter-scale 1.75")).unwrap().unwrap();
        assert_eq!((sizes.radar_scale, sizes.meter_scale, sizes.scale), (Some(2.0), Some(1.75), None));
        assert_eq!(o.look.range_m, 60.0);
        assert_eq!(o.look.north_offset_deg, 90.0);
        assert_eq!(o.ports, vec![7777]);
        assert!(parse_args(args("--help")).unwrap().is_none());
        assert!(parse_args(args("--corner middle")).is_err());
        assert!(parse_args(args("--demo --replay x.pcap")).is_err());
        assert_eq!(parse_args(args("")).unwrap().unwrap().ports, vec![13328]);
        assert_eq!(parse_args(args("")).unwrap().unwrap().corner, Corner::TopLeft, "top left unless told otherwise");
        let o = parse_args(args("--meter-mode dps --meter-pos 500,80 --meter-rows 5 --no-radar")).unwrap().unwrap();
        assert_eq!(o.meter_mode, Some(dps::Mode::Average));
        assert_eq!(o.meter_pos, Some((500, 80)));
        assert_eq!(o.meter_rows, 5);
        assert!(!o.radar && o.meter);
        assert!(parse_args(args("--meter-mode fast")).is_err());
        assert!(parse_args(args("--no-meter --no-radar")).is_err());
    }
}
