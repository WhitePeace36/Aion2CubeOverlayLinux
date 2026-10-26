//! aion-cube-overlay: the Hidden Cube radar of aion2-tui as a click-through Wayland overlay for Linux.
//!
//! The decoding (`aion-net` and `cube-core`) is aion2-tui's, unchanged. This crate adds Linux capture, the
//! radar drawing with height and the overlay window.

// Drawing helpers take coordinates, colours and sizes side by side; a struct for each would not read better.
#![allow(clippy::too_many_arguments)]

mod demo;
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
use wayland::{Corner, Toggles, WindowOptions};

const USAGE: &str = "\
aion-cube-overlay: Aion 2 Hidden Cube radar as a click-through Wayland overlay

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
  --scale <f>             size of the overlay (default 1.0)
  --output <name>         monitor to show it on (default: the compositor's choice)
  --range <m>             radar radius in metres (default 100, the game sends cubes within about 100 m)
  --opacity <f>           background opacity, 0 to 1 (default 1)
  --north-offset <deg>    turn the map, if north turns out not to be the game's north (default 0)
  --no-spots              do not draw the spots where cubes were seen before
  --no-trail              do not draw your path
  --font <file.ttf>       font for the text (default: fontconfig's sans-serif)

BEHAVIOUR:
  --always                keep the radar up even without cubes (default: shows itself when a cube is near)
  --alert-cmd <command>   run this with `sh -c` when a cube appears; gets CUBE_DISTANCE, CUBE_HEIGHT,
                          CUBE_DIRECTION and CUBE_ALERT in the environment
  --window                use a normal window even if the compositor has layer-shell
  --png <file>            draw one frame to a PNG and exit (with --demo: --at <seconds> into the demo)
  -h, --help              this text

While running: SIGUSR1 hides or shows the radar, SIGUSR2 pins it on. For example:
  pkill -USR1 aion-cube-overl
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
    scale: f32,
    output: Option<String>,
    look: Look,
    font: Option<PathBuf>,
    always: bool,
    alert_cmd: Option<String>,
    window: bool,
    png: Option<PathBuf>,
    at_s: f64,
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
        scale: 1.0,
        output: None,
        look: Look::default(),
        font: None,
        always: false,
        alert_cmd: None,
        window: false,
        png: None,
        at_s: 16.0,
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
            "--margin" => {
                let v = value()?;
                let (x, y) = v.split_once(',').ok_or_else(|| anyhow!("--margin {v}: use x,y, for example 24,120"))?;
                o.margin = (x.trim().parse().context("--margin")?, y.trim().parse().context("--margin")?);
            }
            "--scale" => o.scale = value()?.parse::<f32>().context("--scale")?.clamp(0.4, 4.0),
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
    if o.ports.is_empty() {
        o.ports.push(cube_core::pipeline::DEFAULT_PORT);
    }
    Ok(Some(o))
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    match parse_args(std::env::args().skip(1).collect()) {
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
    } else if o.png.is_some() {
        bail!("--png needs --demo or --replay");
    } else {
        let spots = store::SpotStore::new(data_dir.join("spots.json"));
        Box::new(source::Live::start(CaptureOptions { interface: o.interface.clone(), ports: o.ports.clone() }, book, templates, spots))
    };
    let fonts = text::Fonts::load(o.font.as_deref());
    let visibility = if o.always { Visibility::Always } else { Visibility::Auto };
    let mut overlay = Overlay::new(source, o.look.clone(), fonts, o.alert_cmd.clone(), visibility);

    if let Some(path) = &o.png {
        return write_png(&mut overlay, &o, now, path);
    }

    let toggles = Toggles { hide: Arc::new(AtomicBool::new(false)), always: Arc::new(AtomicBool::new(false)) };
    signal_hook::flag::register(signal_hook::consts::SIGUSR1, toggles.hide.clone())?;
    signal_hook::flag::register(signal_hook::consts::SIGUSR2, toggles.always.clone())?;
    wayland::run(overlay, WindowOptions { corner: o.corner, margin: o.margin, scale: o.scale, output: o.output.clone(), force_window: o.window }, toggles)
}

/// One frame to a PNG: steps the source in 100 ms ticks up to `--at` seconds, as the overlay would live.
fn write_png(overlay: &mut Overlay, o: &Options, start: u64, path: &std::path::Path) -> anyhow::Result<()> {
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
    let w = (radar::BASE_W * o.scale).round() as u32;
    let h = (radar::BASE_H * o.scale).round() as u32;
    let mut pix = tiny_skia::Pixmap::new(w, h).ok_or_else(|| anyhow!("bad size"))?;
    overlay.draw(&mut pix, o.scale);
    pix.save_png(path)?;
    log::info!("wrote {}", path.display());
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
        assert_eq!(o.scale, 1.5);
        assert_eq!(o.look.range_m, 60.0);
        assert_eq!(o.look.north_offset_deg, 90.0);
        assert_eq!(o.ports, vec![7777]);
        assert!(parse_args(args("--help")).unwrap().is_none());
        assert!(parse_args(args("--corner middle")).is_err());
        assert!(parse_args(args("--demo --replay x.pcap")).is_err());
        assert_eq!(parse_args(args("")).unwrap().unwrap().ports, vec![13328]);
        assert_eq!(parse_args(args("")).unwrap().unwrap().corner, Corner::TopLeft, "top left unless told otherwise");
    }
}
