//! Agent 5 — maps and graphic cards, and the thumbnail.
//!
//! Maps are drawn from Natural Earth (public domain) coastlines and borders in Web Mercator,
//! centred so the place sits under the `doc-map-pin` graphic; the data is downloaded once to
//! `~/.crstudio/data`. Without it a plain graticule map is drawn and the log says so.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use filmcraft_geom::Affine;
use filmcraft_project::gtemplate::builtin_templates;
use filmcraft_project::{ItemKind, Label, ParamValue, TrackKind};
use filmcraft_render::Image;
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, Tick, TimeRange};
use serde_json::Value;

use super::{CrStudioPipeline, MapSpec, SceneSpec, persist, truncate_line, turkish};
use crate::{EngineError, Result};

// ------------------------------------------------------------------------------------------------
// Drawing helpers (premultiplied linear RGBA)
// ------------------------------------------------------------------------------------------------

pub(crate) fn put_px(img: &mut Image, x: i32, y: i32, rgba: [f32; 4]) {
    if x < 0 || y < 0 || x as usize >= img.w || y as usize >= img.h {
        return;
    }
    let idx = (y as usize * img.w + x as usize) * 4;
    let Some(dst) = img.px.get_mut(idx..idx + 4) else { return };
    let a = rgba[3].clamp(0.0, 1.0);
    let inv = 1.0 - a;
    dst[0] = rgba[0] * a + dst[0] * inv;
    dst[1] = rgba[1] * a + dst[1] * inv;
    dst[2] = rgba[2] * a + dst[2] * inv;
    dst[3] = (a + dst[3] * inv).clamp(0.0, 1.0);
}

pub(crate) fn fill_rect(img: &mut Image, x0: i32, y0: i32, w: i32, h: i32, rgba: [f32; 4]) {
    let xa = x0.max(0);
    let ya = y0.max(0);
    let xb = x0.saturating_add(w).min(img.w as i32);
    let yb = y0.saturating_add(h).min(img.h as i32);
    for y in ya..yb {
        for x in xa..xb {
            put_px(img, x, y, rgba);
        }
    }
}

pub(crate) fn fill_circle(img: &mut Image, cx: f32, cy: f32, r: f32, rgba: [f32; 4]) {
    let x0 = (cx - r - 2.0).floor() as i32;
    let x1 = (cx + r + 2.0).ceil() as i32;
    let y0 = (cy - r - 2.0).floor() as i32;
    let y1 = (cy + r + 2.0).ceil() as i32;
    for y in y0..=y1 {
        for x in x0..=x1 {
            let d = ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)).sqrt();
            if d <= r + 1.0 {
                let aa = (r + 0.5 - d).clamp(0.0, 1.0);
                put_px(img, x, y, [rgba[0], rgba[1], rgba[2], rgba[3] * aa]);
            }
        }
    }
}

pub(crate) fn stroke_circle(img: &mut Image, cx: f32, cy: f32, r: f32, thickness: f32, rgba: [f32; 4]) {
    let half = thickness * 0.5;
    let x0 = (cx - r - half - 2.0).floor() as i32;
    let x1 = (cx + r + half + 2.0).ceil() as i32;
    let y0 = (cy - r - half - 2.0).floor() as i32;
    let y1 = (cy + r + half + 2.0).ceil() as i32;
    for y in y0..=y1 {
        for x in x0..=x1 {
            let d = ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)).sqrt();
            let dist = (d - r).abs();
            if dist <= half + 1.0 {
                let aa = (half + 0.5 - dist).clamp(0.0, 1.0);
                put_px(img, x, y, [rgba[0], rgba[1], rgba[2], rgba[3] * aa]);
            }
        }
    }
}

fn draw_line(img: &mut Image, a: (f32, f32), b: (f32, f32), width: f32, rgba: [f32; 4]) {
    let (wf, hf) = (img.w as f32 + 50.0, img.h as f32 + 50.0);
    if (a.0 < -50.0 && b.0 < -50.0) || (a.0 > wf && b.0 > wf) || (a.1 < -50.0 && b.1 < -50.0) || (a.1 > hf && b.1 > hf) {
        return;
    }
    let len = ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
    if !len.is_finite() || len > 20_000.0 {
        return;
    }
    let steps = (len / (width * 0.6).max(0.6)).ceil().max(1.0) as usize;
    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        fill_circle(img, a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t, width * 0.5, rgba);
    }
}

pub(crate) fn draw_dashed_arc(img: &mut Image, p0: (f32, f32), p1: (f32, f32), thickness: f32, rgba: [f32; 4]) {
    let dx = p1.0 - p0.0;
    let dy = p1.1 - p0.1;
    let dist = (dx * dx + dy * dy).sqrt().max(1.0);
    if !dist.is_finite() || dist > 20_000.0 {
        return;
    }
    let steps = (dist * 1.5) as usize;
    let ctrl = ((p0.0 + p1.0) * 0.5, (p0.1 + p1.1) * 0.5 - dist * 0.24);
    for i in 0..=steps {
        if (i / 14) % 2 == 1 {
            continue;
        }
        let t = i as f32 / steps.max(1) as f32;
        let u = 1.0 - t;
        let x = u * u * p0.0 + 2.0 * u * t * ctrl.0 + t * t * p1.0;
        let y = u * u * p0.1 + 2.0 * u * t * ctrl.1 + t * t * p1.1;
        fill_circle(img, x, y, thickness, rgba);
    }
}

/// Render a built-in `.fcgt` template over `img` with control values filled in.
pub(crate) fn stamp_template_overlay(img: &mut Image, template_id: &str, controls: &[(&str, &str)]) {
    let Some(mut tpl) = builtin_templates().into_iter().find(|t| t.id == template_id) else {
        return;
    };
    for &(cid, val) in controls {
        if let Some(c) = tpl.controls.iter().find(|c| c.id == cid)
            && let Some(layer) = tpl.layers.iter_mut().find(|l| filmcraft_project::gtemplate::layer_uid(l) == c.layer)
        {
            layer.params.insert(c.param.clone(), filmcraft_project::Param::new(ParamValue::Text(val.to_string())));
        }
    }
    let w = img.w as u32;
    let h = img.h as u32;
    let (layers, meta) = tpl.instantiate((w, h));
    let mut pr = filmcraft_project::Project::new("stamp");
    let rate = FrameRate::FPS_30;
    let gid = pr.add_item("G", Label::Rose, ItemKind::Graphic { width: w, height: h, rate }, None);
    if let Some(mut it) = pr.make_track_item(gid, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, Tick(5 * TICKS_PER_SECOND)), rate) {
        it.effects.extend(layers);
        it.graphic = Some(Box::new(meta));
        filmcraft_render::graphic_clip::render_graphic(&it, Tick(TICKS_PER_SECOND), (w, h), &Affine::IDENTITY, img);
    }
}

fn encode_png(img: &Image) -> Result<Vec<u8>> {
    filmcraft_export::still::encode(img, filmcraft_export::still::StillFormat::Png, 8).map_err(|e| EngineError::Other(e.to_string()))
}

fn srgb_to_linear(c: u8) -> f32 {
    let v = c as f32 / 255.0;
    if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
}

/// Decode a still image file (PNG/JPEG/…) into a linear `Image`.
pub fn decode_image(path: &Path) -> Option<Image> {
    let bytes = std::fs::read(path).ok()?;
    let src = filmcraft_media::open_bytes(&path.to_string_lossy(), bytes.into(), &[]).ok()?;
    let frame = src.video_frame(filmcraft_media::FrameRequest::full(Tick::ZERO)).ok()?;
    let (w, h) = (frame.width as usize, frame.height as usize);
    if w == 0 || h == 0 || w > 16_384 || h > 16_384 {
        return None;
    }
    let rgba = frame.to_rgba8();
    let mut img = Image::new(w, h);
    for (dst, src) in img.px.chunks_exact_mut(4).zip(rgba.chunks_exact(4)) {
        let a = src[3] as f32 / 255.0;
        dst[0] = srgb_to_linear(src[0]) * a;
        dst[1] = srgb_to_linear(src[1]) * a;
        dst[2] = srgb_to_linear(src[2]) * a;
        dst[3] = a;
    }
    Some(img)
}

/// Draw `photo` over the whole of `img` (cover fit), darkened by `dim` (0–1).
pub fn draw_cover(img: &mut Image, photo: &Image, dim: f32) {
    let s = (img.w as f32 / photo.w.max(1) as f32).max(img.h as f32 / photo.h.max(1) as f32);
    let ox = (photo.w as f32 * s - img.w as f32) * 0.5;
    let oy = (photo.h as f32 * s - img.h as f32) * 0.5;
    let keep = (1.0 - dim).clamp(0.0, 1.0);
    for y in 0..img.h {
        for x in 0..img.w {
            let sx = (x as f32 + ox) / s;
            let sy = (y as f32 + oy) / s;
            let p = photo.sample_bilinear_clamped(sx, sy);
            let idx = (y * img.w + x) * 4;
            if let Some(d) = img.px.get_mut(idx..idx + 4) {
                d[0] = p[0] * keep;
                d[1] = p[1] * keep;
                d[2] = p[2] * keep;
                d[3] = 1.0;
            }
        }
    }
}

// ------------------------------------------------------------------------------------------------
// Natural Earth geography
// ------------------------------------------------------------------------------------------------

const NE_BASE: &str = "https://raw.githubusercontent.com/nvkelso/natural-earth-vector/master/geojson/";
const NE_LAND: &str = "ne_50m_land.geojson";
const NE_BORDERS: &str = "ne_50m_admin_0_boundary_lines_land.geojson";

/// Rings (lon, lat) of land polygons and border lines.
pub struct Geo {
    pub land: Vec<Vec<(f64, f64)>>,
    pub borders: Vec<Vec<(f64, f64)>>,
}

fn data_dir() -> PathBuf {
    persist::config_dir().join("data")
}

fn fetch(name: &str) -> Option<PathBuf> {
    let path = data_dir().join(name);
    if path.is_file() && std::fs::metadata(&path).map(|m| m.len() > 10_000).unwrap_or(false) {
        return Some(path);
    }
    if cfg!(target_arch = "wasm32") {
        return None;
    }
    let _ = std::fs::create_dir_all(data_dir());
    let tmp = path.with_extension("part");
    let ok = std::process::Command::new("curl")
        .args(["-sS", "-L", "--fail", "--max-time", "90", "-o"])
        .arg(&tmp)
        .arg(format!("{NE_BASE}{name}"))
        .status()
        .is_ok_and(|s| s.success());
    if ok && std::fs::rename(&tmp, &path).is_ok() {
        super::write_attribution_sidecar(&path, name, "Natural Earth", "https://www.naturalearthdata.com", "Public domain", "Map data");
        Some(path)
    } else {
        let _ = std::fs::remove_file(&tmp);
        None
    }
}

fn rings_from(v: &Value, out: &mut Vec<Vec<(f64, f64)>>) {
    let Some(features) = v.get("features").and_then(Value::as_array) else {
        return;
    };
    let ring = |r: &Value| -> Vec<(f64, f64)> {
        r.as_array()
            .map(|pts| pts.iter().filter_map(|p| Some((p.get(0)?.as_f64()?, p.get(1)?.as_f64()?))).filter(|(x, y)| x.is_finite() && y.is_finite()).collect())
            .unwrap_or_default()
    };
    for f in features {
        let Some(g) = f.get("geometry") else { continue };
        let coords = g.get("coordinates");
        match g.get("type").and_then(Value::as_str) {
            Some("Polygon") => coords.and_then(Value::as_array).into_iter().flatten().for_each(|r| out.push(ring(r))),
            Some("MultiPolygon") => {
                coords.and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_array).flatten().for_each(|r| out.push(ring(r)))
            }
            Some("LineString") => {
                if let Some(c) = coords {
                    out.push(ring(c));
                }
            }
            Some("MultiLineString") => coords.and_then(Value::as_array).into_iter().flatten().for_each(|r| out.push(ring(r))),
            _ => {}
        }
    }
    out.retain(|r| r.len() >= 2);
}

static GEO: OnceLock<Mutex<Option<Arc<Geo>>>> = OnceLock::new();

/// The world geography (downloaded and parsed once per run).
pub fn geo() -> Option<Arc<Geo>> {
    let slot = GEO.get_or_init(|| Mutex::new(None));
    let mut g = slot.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(geo) = g.as_ref() {
        return Some(Arc::clone(geo));
    }
    let read = |name: &str| -> Option<Value> {
        let p = fetch(name)?;
        let b = std::fs::read(p).ok()?;
        serde_json::from_slice(&b).ok()
    };
    let land_v = read(NE_LAND)?;
    let mut land = Vec::new();
    rings_from(&land_v, &mut land);
    let mut borders = Vec::new();
    if let Some(b) = read(NE_BORDERS) {
        rings_from(&b, &mut borders);
    }
    if land.is_empty() {
        return None;
    }
    let geo = Arc::new(Geo { land, borders });
    *g = Some(Arc::clone(&geo));
    Some(geo)
}

fn merc(lat: f64) -> f64 {
    let l = lat.clamp(-85.0, 85.0).to_radians();
    (std::f64::consts::FRAC_PI_4 + l / 2.0).tan().ln().to_degrees()
}

/// A Mercator view: degrees → pixels.
struct View {
    k: f64,
    lon0: f64,
    my0: f64,
    px0: f64,
    py0: f64,
}

impl View {
    fn to_px(&self, lon: f64, lat: f64) -> (f32, f32) {
        let mut dl = lon - self.lon0;
        if dl > 180.0 {
            dl -= 360.0;
        } else if dl < -180.0 {
            dl += 360.0;
        }
        ((self.px0 + dl * self.k) as f32, (self.py0 - (merc(lat) - self.my0) * self.k) as f32)
    }
}

/// Even-odd scanline fill of polygon rings already in pixel space.
fn fill_polygons(img: &mut Image, rings: &[Vec<(f32, f32)>], rgba: [f32; 4]) {
    let h = img.h as i32;
    let mut edges: Vec<(f32, f32, f32, f32)> = Vec::new(); // (y0, y1, x at y0, dx/dy)
    for r in rings {
        for w in r.windows(2) {
            let (a, b) = (w[0], w[1]);
            if (a.1 - b.1).abs() < 1e-6 || !a.0.is_finite() || !b.0.is_finite() {
                continue;
            }
            let (p, q) = if a.1 < b.1 { (a, b) } else { (b, a) };
            if q.1 < 0.0 || p.1 > h as f32 {
                continue;
            }
            edges.push((p.1, q.1, p.0, (q.0 - p.0) / (q.1 - p.1)));
        }
    }
    edges.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut active: Vec<usize> = Vec::new();
    let mut next = 0usize;
    let mut xs: Vec<f32> = Vec::new();
    for y in 0..h {
        let yc = y as f32 + 0.5;
        while next < edges.len() && edges.get(next).is_some_and(|e| e.0 <= yc) {
            active.push(next);
            next += 1;
        }
        active.retain(|&i| edges.get(i).is_some_and(|e| e.1 > yc));
        xs.clear();
        for &i in &active {
            if let Some(e) = edges.get(i)
                && e.0 <= yc
            {
                xs.push(e.2 + (yc - e.0) * e.3);
            }
        }
        xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        for pair in xs.chunks_exact(2) {
            let x0 = pair[0].max(0.0).round() as i32;
            let x1 = pair[1].min(img.w as f32).round() as i32;
            for x in x0..x1 {
                put_px(img, x, y, rgba);
            }
        }
    }
}

/// Where the `doc-map-pin` template draws its pin on a `w`×`h` frame.
fn pin_position(w: u32, h: u32) -> (f64, f64) {
    let s = (f64::from(w) / 1920.0).min(f64::from(h) / 1080.0);
    (960.0 * s + (f64::from(w) - 1920.0 * s) / 2.0, 470.0 * s + (f64::from(h) - 1080.0 * s) / 2.0)
}

/// Render a map centred on `spec`'s place, with route and extra pins. Returns the PNG and
/// whether real geography was available.
pub fn render_map_png(w: u32, h: u32, spec: &MapSpec, scene_title: &str) -> Result<(Vec<u8>, bool)> {
    let mut spec = spec.clone();
    spec.sanitize();
    let mut img = Image::filled(w as usize, h as usize, [0.006, 0.013, 0.03, 1.0]);
    let (wf, hf) = (w as f32, h as f32);
    let (pin_x, pin_y) = pin_position(w, h);
    // Span: fit every pin, else from zoom (zoom 4 ≈ 30° wide).
    let mut span = 240.0 / 2f64.powf(spec.zoom - 1.0);
    let others: Vec<(f64, f64)> = spec.extra_pins.iter().map(|(_, a, b)| (*a, *b)).chain(spec.from_lat.zip(spec.from_lon)).collect();
    if !others.is_empty() {
        let max_dl = others.iter().map(|(_, lon)| (lon - spec.lon).abs()).fold(0.0, f64::max);
        let max_dm = others.iter().map(|(lat, _)| (merc(*lat) - merc(spec.lat)).abs()).fold(0.0, f64::max);
        span = span.max(max_dl * 2.6 + 4.0).max(max_dm * 2.6 * f64::from(w) / f64::from(h).max(1.0));
    }
    let span = span.clamp(3.0, 360.0);
    let view = View { k: f64::from(w) / span, lon0: spec.lon, my0: merc(spec.lat), px0: pin_x, py0: pin_y };

    let geo = geo();
    if let Some(g) = &geo {
        let rings: Vec<Vec<(f32, f32)>> = g.land.iter().map(|r| r.iter().map(|(lon, lat)| view.to_px(*lon, *lat)).collect()).collect();
        // land with a soft lift
        fill_polygons(&mut img, &rings, [0.055, 0.07, 0.082, 1.0]);
        for r in &rings {
            for seg in r.windows(2) {
                if (seg[0].0 - seg[1].0).abs() < wf * 0.5 {
                    draw_line(&mut img, seg[0], seg[1], (wf / 900.0).max(1.2), [0.22, 0.34, 0.46, 0.9]);
                }
            }
        }
        for b in &g.borders {
            let pts: Vec<(f32, f32)> = b.iter().map(|(lon, lat)| view.to_px(*lon, *lat)).collect();
            for seg in pts.windows(2) {
                if (seg[0].0 - seg[1].0).abs() < wf * 0.5 {
                    draw_line(&mut img, seg[0], seg[1], (wf / 1400.0).max(1.0), [0.32, 0.34, 0.40, 0.55]);
                }
            }
        }
    }
    // graticule every 5° (or 10°/30° when wide)
    let step = if span > 120.0 {
        30.0
    } else if span > 40.0 {
        10.0
    } else {
        5.0
    };
    let mut lon = (spec.lon - span).div_euclid(step) * step;
    while lon < spec.lon + span {
        let a = view.to_px(lon, -80.0);
        let b = view.to_px(lon, 80.0);
        draw_line(&mut img, a, b, 1.0, [0.14, 0.24, 0.36, 0.28]);
        lon += step;
    }
    let mut lat = -80.0;
    while lat <= 80.0 {
        let a = view.to_px(spec.lon - span, lat);
        let b = view.to_px(spec.lon + span, lat);
        draw_line(&mut img, a, b, 1.0, [0.14, 0.24, 0.36, 0.28]);
        lat += step;
    }
    // vignette
    for y in 0..img.h {
        for x in 0..img.w {
            let nx = x as f32 / wf - 0.5;
            let ny = y as f32 / hf - 0.5;
            let v = (1.0 - (nx * nx + ny * ny).sqrt() * 0.9).clamp(0.35, 1.0);
            let idx = (y * img.w + x) * 4;
            if let Some(d) = img.px.get_mut(idx..idx + 3) {
                d.iter_mut().for_each(|c| *c *= v);
            }
        }
    }
    let accent = filmcraft_color::parse_hex(&spec.accent_hex).unwrap_or([0.93, 0.26, 0.26, 1.0]);
    let target = (pin_x as f32, pin_y as f32);
    if let (Some(fl), Some(fo)) = (spec.from_lat, spec.from_lon) {
        let o = view.to_px(fo, fl);
        draw_dashed_arc(&mut img, o, target, (wf * 0.0025).max(2.0), [0.98, 0.82, 0.10, 0.9]);
        fill_circle(&mut img, o.0, o.1, wf * 0.006, [1.0, 1.0, 1.0, 1.0]);
    }
    for (_, la, lo) in &spec.extra_pins {
        let p = view.to_px(*lo, *la);
        stroke_circle(&mut img, p.0, p.1, wf * 0.010, 2.5, [0.98, 0.82, 0.10, 0.9]);
        fill_circle(&mut img, p.0, p.1, wf * 0.004, [1.0, 1.0, 1.0, 1.0]);
    }
    for mult in [0.06_f32, 0.035] {
        stroke_circle(&mut img, target.0, target.1, wf * mult, 2.0, [accent[0], accent[1], accent[2], 0.35]);
    }
    let ns = if spec.lat >= 0.0 { "K" } else { "G" };
    let ew = if spec.lon >= 0.0 { "D" } else { "B" };
    let coord = format!("{:.2}° {ns}, {:.2}° {ew}  •  {}", spec.lat.abs(), spec.lon.abs(), if spec.subtitle.is_empty() { scene_title } else { &spec.subtitle });
    let loc = truncate_line(&turkish::upper(&spec.location_name), 38);
    stamp_template_overlay(&mut img, "builtin:doc-map-pin", &[("location", &loc), ("coordinates", &truncate_line(&coord, 60))]);
    Ok((encode_png(&img)?, geo.is_some()))
}

/// Backwards-compatible map renderer (PNG only).
pub fn render_documentary_map_png(w: u32, h: u32, spec: &MapSpec, scene_title: &str) -> Result<Vec<u8>> {
    render_map_png(w, h, spec, scene_title).map(|(png, _)| png)
}

fn editorial_backdrop(w: u32, h: u32) -> Image {
    let mut img = Image::new(w as usize, h as usize);
    let (wf, hf) = (w as f32, h as f32);
    for y in 0..img.h {
        for x in 0..img.w {
            let nx = x as f32 / wf;
            let ny = y as f32 / hf;
            let glow = (1.0 - ((nx - 0.5).powi(2) + (ny - 0.38).powi(2)).sqrt() * 1.1).clamp(0.15, 1.0);
            let idx = (y * img.w + x) * 4;
            if let Some(d) = img.px.get_mut(idx..idx + 4) {
                d[0] = 0.035 * glow;
                d[1] = 0.040 * glow;
                d[2] = 0.060 * glow;
                d[3] = 1.0;
            }
        }
    }
    img
}

/// A graphic card for a scene: chapter card, documented quote, or a chart from real numbers.
/// `background` (a photo) is shown darkened behind the card when given.
pub fn render_card_png(w: u32, h: u32, scene: &SceneSpec, source_line: &str, background: Option<&Path>) -> Result<Vec<u8>> {
    let mut img = editorial_backdrop(w, h);
    if let Some(photo) = background.and_then(decode_image) {
        draw_cover(&mut img, &photo, 0.72);
    }
    let (wf, hf) = (w as f32, h as f32);
    match (scene.visual_type.as_str(), &scene.chart) {
        ("infographic_chart", Some(chart)) if !chart.values.is_empty() => {
            let max = chart.values.iter().copied().fold(f64::MIN, f64::max).max(1e-9);
            let n = chart.values.len().min(8);
            let chart_w = wf * 0.62;
            let start_x = (wf - chart_w) * 0.5;
            let base_y = hf * 0.70;
            let max_h = hf * 0.40;
            let slot = chart_w / n as f32;
            for (i, v) in chart.values.iter().take(n).enumerate() {
                let bh = (max_h * (v.max(0.0) / max) as f32).max(2.0);
                let bx = start_x + slot * i as f32 + slot * 0.18;
                let col = if i + 1 == n { [0.88, 0.12, 0.28, 0.95] } else { [0.20, 0.50, 0.85, 0.9] };
                fill_rect(&mut img, bx as i32, (base_y - bh) as i32, (slot * 0.64) as i32, bh as i32, col);
            }
            let labels = chart.labels.iter().take(n).zip(chart.values.iter()).map(|(l, v)| format!("{l}: {v}{}", chart.unit)).collect::<Vec<_>>().join("  ·  ");
            stamp_template_overlay(
                &mut img,
                "builtin:lower-third-rule",
                &[
                    ("name", &truncate_line(if scene.overlay_title.is_empty() { &scene.title } else { &scene.overlay_title }, 40)),
                    ("role", &truncate_line(&labels, 80)),
                ],
            );
        }
        ("evidence_highlight", _) => {
            let quote = truncate_line(if scene.highlight_phrase.is_empty() { &scene.narration_text } else { &scene.highlight_phrase }, 70);
            let src = if source_line.is_empty() { "Kaynak: anlatım".to_string() } else { format!("Kaynak: {source_line}") };
            stamp_template_overlay(&mut img, "builtin:doc-highlight-quote", &[("quote", &format!("“{quote}”")), ("source", &truncate_line(&src, 70))]);
        }
        _ => {
            let chap = if scene.chapter.is_empty() { format!("BÖLÜM {:02}", scene.index + 1) } else { turkish::upper(&scene.chapter) };
            let title = truncate_line(&turkish::upper(if scene.overlay_title.is_empty() { &scene.title } else { &scene.overlay_title }), 34);
            let sub = truncate_line(if scene.overlay_subtitle.is_empty() { &scene.narration_text } else { &scene.overlay_subtitle }, 64);
            stamp_template_overlay(&mut img, "builtin:doc-chapter-card", &[("chapter", &truncate_line(&chap, 30)), ("title", &title), ("subtitle", &sub)]);
        }
    }
    encode_png(&img)
}

/// Backwards-compatible card renderer.
pub fn render_documentary_card_png(w: u32, h: u32, scene: &SceneSpec) -> Result<Vec<u8>> {
    render_card_png(w, h, scene, "", None)
}

/// The 1280×720 thumbnail: the hook scene's photo (or a map), darkened, with big Turkish
/// headline text and the channel accent.
pub fn render_youtube_thumbnail_png(w: u32, h: u32, p: &CrStudioPipeline) -> Result<Vec<u8>> {
    let mut img = editorial_backdrop(w, h);
    let photo = p
        .scenes
        .iter()
        .filter(|s| s.visual_type == "stock_photo")
        .filter_map(|s| s.selected_visual.as_deref())
        .chain(p.scenes.iter().filter_map(|s| s.selected_visual.as_deref()))
        .find_map(|path| decode_image(Path::new(path)));
    if let Some(ph) = photo {
        draw_cover(&mut img, &ph, 0.35);
    }
    // left-side darkening gradient for text legibility
    let (wf, _hf) = (w as f32, h as f32);
    for y in 0..img.h {
        for x in 0..img.w {
            let t = (1.0 - x as f32 / (wf * 0.75)).clamp(0.0, 1.0);
            let idx = (y * img.w + x) * 4;
            if let Some(d) = img.px.get_mut(idx..idx + 3) {
                d.iter_mut().for_each(|c| *c *= 1.0 - 0.75 * t);
            }
        }
    }
    let raw = if !p.social_meta.thumbnail_text.trim().is_empty() {
        p.social_meta.thumbnail_text.trim().to_string()
    } else {
        p.title.split_whitespace().take(3).collect::<Vec<_>>().join(" ")
    };
    let headline = truncate_line(&turkish::upper(&raw), 28);
    let tag = truncate_line(&turkish::upper(&p.channel_name), 24);
    stamp_template_overlay(&mut img, "builtin:yt-thumbnail-bold", &[("headline", &headline), ("tag", &tag)]);
    encode_png(&img)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mercator_and_fill() {
        assert!(merc(0.0).abs() < 1e-9);
        assert!(merc(60.0) > 60.0);
        let mut img = Image::new(40, 40);
        fill_polygons(&mut img, &[vec![(5.0, 5.0), (35.0, 5.0), (35.0, 35.0), (5.0, 35.0), (5.0, 5.0)]], [1.0, 0.0, 0.0, 1.0]);
        assert!(img.get(20, 20)[0] > 0.9);
        assert!(img.get(2, 2)[0] < 0.1);
        // hostile coordinates do not panic
        fill_polygons(&mut img, &[vec![(f32::NAN, 1.0), (1e30, 1e30), (-1e30, 5.0)]], [1.0, 1.0, 1.0, 1.0]);
    }

    #[test]
    fn pin_matches_template_layout() {
        let (x, y) = pin_position(1920, 1080);
        assert!((x - 960.0).abs() < 1e-9 && (y - 470.0).abs() < 1e-9);
        let (x, _) = pin_position(1080, 1920);
        assert!((x - 540.0).abs() < 1e-9);
    }
}
