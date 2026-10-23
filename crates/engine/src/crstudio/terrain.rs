//! Real elevation for the Blender map shots: Terrarium elevation tiles (Mapzen / AWS Terrain
//! Tiles, open data: SRTM, GMTED, ETOPO1 and others; attribution in the licence inventory),
//! cached in `~/.crstudio/data/terrarium`, sampled onto a grid around a place and written for
//! `tools/blender/crstudio_shot.py` as `heights.f32` (metres, little-endian), `land.u8` and
//! `meta.json` (markers and route in 0–1 frame coordinates).

use std::path::{Path, PathBuf};

use serde_json::json;

use super::{MapSpec, persist};

const TILE_URL: &str = "https://s3.amazonaws.com/elevation-tiles-prod/terrarium";
pub const LICENSE: &str = "Yükseklik: Mapzen/AWS Terrain Tiles (SRTM, GMTED, ETOPO1 – açık veri, kaynak gösterilerek)";
const MAX_TILES: usize = 49;

fn tiles_dir() -> PathBuf {
    persist::config_dir().join("data").join("terrarium")
}

fn merc_y(lat: f64) -> f64 {
    let l = lat.clamp(-85.0, 85.0).to_radians();
    (std::f64::consts::FRAC_PI_4 + l / 2.0).tan().ln()
}

fn inv_merc_y(y: f64) -> f64 {
    (2.0 * y.exp().atan() - std::f64::consts::FRAC_PI_2).to_degrees()
}

/// Global pixel coordinates at `zoom` (256-px tiles).
fn global_px(lon: f64, lat: f64, zoom: u32) -> (f64, f64) {
    let n = 256.0 * f64::from(1u32 << zoom.min(15));
    let x = (lon + 180.0) / 360.0 * n;
    let y = (1.0 - merc_y(lat) / std::f64::consts::PI) / 2.0 * n;
    (x, y)
}

struct Tile {
    /// Elevation in metres, 256×256.
    h: Vec<f32>,
}

fn fetch_tile(z: u32, x: u32, y: u32) -> Option<Tile> {
    let path = tiles_dir().join(format!("{z}")).join(format!("{x}")).join(format!("{y}.png"));
    if !path.is_file() {
        if cfg!(target_arch = "wasm32") {
            return None;
        }
        std::fs::create_dir_all(path.parent()?).ok()?;
        let tmp = path.with_extension("part");
        let ok = std::process::Command::new("curl")
            .args(["-sS", "-L", "--fail", "--max-time", "30", "-o"])
            .arg(&tmp)
            .arg(format!("{TILE_URL}/{z}/{x}/{y}.png"))
            .status()
            .is_ok_and(|s| s.success());
        if !ok || std::fs::rename(&tmp, &path).is_err() {
            let _ = std::fs::remove_file(&tmp);
            return None;
        }
    }
    let bytes = std::fs::read(&path).ok()?;
    let src = filmcraft_media::open_bytes(&path.to_string_lossy(), bytes.into(), &[]).ok()?;
    let frame = src.video_frame(filmcraft_media::FrameRequest::full(filmcraft_time::Tick::ZERO)).ok()?;
    if frame.width != 256 || frame.height != 256 {
        return None;
    }
    let rgba = frame.to_rgba8();
    let h = rgba
        .chunks_exact(4)
        .map(|p| match p {
            [r, g, b, _] => (f32::from(*r) * 256.0 + f32::from(*g) + f32::from(*b) / 256.0) - 32768.0,
            _ => 0.0,
        })
        .collect();
    Some(Tile { h })
}

/// What [`prepare`] wrote.
#[derive(Debug)]
pub struct Prepared {
    pub dir: PathBuf,
    pub real: bool,
}

/// Sample elevation around `spec` onto a `grid_w`-wide grid with the frame's `aspect`, and write
/// the files the Blender script reads into `dir`.
pub fn prepare(spec: &MapSpec, aspect: f64, grid_w: usize, dir: &Path) -> Result<Prepared, String> {
    let mut spec = spec.clone();
    spec.sanitize();
    let aspect = if aspect.is_finite() && aspect > 0.2 { aspect.min(5.0) } else { 16.0 / 9.0 };
    let gw = grid_w.clamp(64, 1200);
    let gh = ((gw as f64 / aspect).round() as usize).clamp(32, 1200);
    // span: from zoom, widened to fit the route start and extra pins; the frame is centred on
    // all the places (not just the main one) so none of them sits at the map's edge
    let mut span = 240.0 / 2f64.powf(spec.zoom - 1.0);
    let pts: Vec<(f64, f64)> =
        spec.extra_pins.iter().map(|(_, la, lo)| (*la, *lo)).chain(spec.from_lat.zip(spec.from_lon)).chain(std::iter::once((spec.lat, spec.lon))).collect();
    let (mut la_min, mut la_max, mut lo_min, mut lo_max) = (spec.lat, spec.lat, spec.lon, spec.lon);
    for (la, lo) in &pts {
        la_min = la_min.min(*la);
        la_max = la_max.max(*la);
        lo_min = lo_min.min(*lo);
        lo_max = lo_max.max(*lo);
    }
    let (c_lat, c_lon) = ((la_min + la_max) / 2.0, (lo_min + lo_max) / 2.0);
    if pts.len() > 1 {
        span = span.max((lo_max - lo_min) * 1.9).max((la_max - la_min) * 1.9 * aspect);
    }
    let span = span.clamp(0.05, 60.0);
    let lon0 = c_lon - span / 2.0;
    let lon1 = c_lon + span / 2.0;
    let my_c = merc_y(c_lat);
    let my_half = (span.to_radians() / aspect) / 2.0;
    let (my_top, my_bot) = (my_c + my_half, my_c - my_half);
    // zoom where a tile pixel is about one grid cell
    let zoom = ((360.0 / span) * (gw as f64 / 256.0)).log2().round().clamp(2.0, 12.0) as u32;
    let (gx0, gy0) = global_px(lon0, inv_merc_y(my_top), zoom);
    let (gx1, gy1) = global_px(lon1, inv_merc_y(my_bot), zoom);
    let (tx0, tx1) = ((gx0 / 256.0).floor() as i64, (gx1 / 256.0).floor() as i64);
    let (ty0, ty1) = ((gy0 / 256.0).floor() as i64, (gy1 / 256.0).floor() as i64);
    let count = ((tx1 - tx0 + 1) * (ty1 - ty0 + 1)).max(0) as usize;
    if count == 0 || count > MAX_TILES {
        return Err(format!("harita alanı için {count} karo gerekiyor; yakınlığı artırın"));
    }
    let nmax = 1i64 << zoom;
    let mut tiles: std::collections::HashMap<(i64, i64), Tile> = std::collections::HashMap::new();
    for ty in ty0..=ty1 {
        for tx in tx0..=tx1 {
            let (x, y) = (tx.rem_euclid(nmax), ty.clamp(0, nmax - 1));
            if let Some(t) = fetch_tile(zoom, x as u32, y as u32) {
                tiles.insert((tx, ty), t);
            }
        }
    }
    let real = tiles.len() == count;
    if tiles.is_empty() {
        return Err("yükseklik verisi indirilemedi (internet bağlantısı?)".into());
    }
    let sample = |gx: f64, gy: f64| -> f32 {
        let (tx, ty) = ((gx / 256.0).floor() as i64, (gy / 256.0).floor() as i64);
        let Some(t) = tiles.get(&(tx, ty)) else { return 0.0 };
        let px = (gx - tx as f64 * 256.0).clamp(0.0, 255.0) as usize;
        let py = (gy - ty as f64 * 256.0).clamp(0.0, 255.0) as usize;
        t.h.get(py * 256 + px).copied().unwrap_or(0.0)
    };
    let mut heights = Vec::with_capacity(gw * gh);
    let mut land = Vec::with_capacity(gw * gh);
    for j in 0..gh {
        let my = my_top - (my_top - my_bot) * (j as f64 + 0.5) / gh as f64;
        let lat = inv_merc_y(my);
        for i in 0..gw {
            let lon = lon0 + (lon1 - lon0) * (i as f64 + 0.5) / gw as f64;
            let (gx, gy) = global_px(lon, lat, zoom);
            let h = sample(gx, gy);
            let h = if h.is_finite() { h.clamp(-11_000.0, 9_000.0) } else { 0.0 };
            heights.push(h.max(0.0));
            land.push(u8::from(h > 0.5));
        }
    }
    // 0–1 frame coordinates
    let norm = |la: f64, lo: f64| -> (f64, f64) { ((lo - lon0) / (lon1 - lon0), (my_top - merc_y(la)) / (my_top - my_bot)) };
    let (mx, my) = norm(spec.lat, spec.lon);
    let mut markers = vec![json!({"name": spec.location_name, "sub": spec.subtitle, "x": mx, "y": my})];
    for (name, la, lo) in &spec.extra_pins {
        let (x, y) = norm(*la, *lo);
        markers.push(json!({"name": name, "sub": "", "x": x, "y": y}));
    }
    let route: Vec<[f64; 2]> = match spec.from_lat.zip(spec.from_lon) {
        Some((fla, flo)) => {
            let (ax, ay) = norm(fla, flo);
            // a gentle arc
            let (dx, dy) = (mx - ax, my - ay);
            let (cx, cy) = ((ax + mx) / 2.0 - dy * 0.18, (ay + my) / 2.0 + dx * 0.18);
            (0..=40)
                .map(|k| {
                    let t = k as f64 / 40.0;
                    let u = 1.0 - t;
                    [u * u * ax + 2.0 * u * t * cx + t * t * mx, u * u * ay + 2.0 * u * t * cy + t * t * my]
                })
                .collect()
        }
        None => Vec::new(),
    };
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let hb: Vec<u8> = heights.iter().flat_map(|v| v.to_le_bytes()).collect();
    std::fs::write(dir.join("heights.f32"), hb).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("land.u8"), &land).map_err(|e| e.to_string())?;
    let meta = json!({"w": gw, "h": gh, "zoom": zoom, "bbox": [lon0, inv_merc_y(my_bot), lon1, inv_merc_y(my_top)], "markers": markers, "route": route, "real": real, "license": LICENSE});
    std::fs::write(dir.join("meta.json"), serde_json::to_vec_pretty(&meta).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    Ok(Prepared { dir: dir.to_path_buf(), real })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projection_round_trips() {
        for lat in [-60.0, -10.0, 0.0, 37.94, 70.0] {
            assert!((inv_merc_y(merc_y(lat)) - lat).abs() < 1e-9);
        }
        let (x, y) = global_px(0.0, 0.0, 1);
        assert!((x - 256.0).abs() < 1e-9 && (y - 256.0).abs() < 1e-9);
    }
}
