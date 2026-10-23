//! Graphics templates (`.fcgt`): FilmCraft's own format for reusable graphics with editable
//! properties, the counterpart of motion graphics templates in other editors.
//!
//! A template file is UTF-8 JSON ([`GraphicsTemplate`]):
//!
//! ```json
//! {
//!   "format": "filmcraft.graphicsTemplate",
//!   "version": 1,
//!   "id": "user:my-lower-third",
//!   "name": "My Lower Third",
//!   "category": "Lower Thirds",
//!   "description": "", "author": "", "license": "", "tags": [],
//!   "canvas": [1920, 1080],
//!   "duration": 1270080000000,
//!   "layers": [ /* graphic_text / graphic_shape effect instances, as in a project file */ ],
//!   "graphic": { /* roll, intro / outro (GraphicMeta) */ },
//!   "controls": [ { "id": "name", "name": "Name", "kind": "text", "layer": 1, "param": "text" } ],
//!   "resources": { "fonts": [ { "family": "…", "style": "…", "license": "OFL-1.1", "data": "<base64>" } ] }
//! }
//! ```
//!
//! - `version` is the format version; readers refuse newer versions.
//! - Layers are referenced by their [`LayerExtra::uid`](crate::graphic_design::LayerExtra); every
//!   layer of a template has one.
//! - `controls` are the properties a user of the template edits (Essential Graphics ▸ Edit):
//!   text, colour, slider (with `min` / `max`), checkbox, font and position, each bound to one
//!   layer parameter (`enabled` = the layer's visibility).
//! - `resources.fonts` optionally embeds font files (base64) that the template needs; installing
//!   the template registers them. Only embed fonts whose licence allows redistribution.
//!
//! FilmCraft never reads Adobe `.mogrt` files (or any other application's template format);
//! [`GraphicsTemplate::from_bytes`] rejects ZIP archives with a message saying so.
//!
//! The built-in templates ([`builtin_templates`]) are original designs made for FilmCraft in code
//! (no image, font or template files), under the project licence.

use filmcraft_geom::Vec2;
use filmcraft_time::{TICKS_PER_SECOND, Tick};
use serde::{Deserialize, Serialize};

use crate::effect::EffectInstance;
use crate::graphic::{self, new_shape_layer, new_text_layer};
use crate::graphic_design::{GraphicMeta, LayerExtra, Pin, PinTarget, Roll, RollMode};
use crate::keyframe::{Param, ParamValue};

pub const TEMPLATE_FORMAT: &str = "filmcraft.graphicsTemplate";
pub const TEMPLATE_VERSION: u32 = 1;
pub const TEMPLATE_EXTENSION: &str = "fcgt";

/// Kind of an editable template property.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ControlKind {
    #[default]
    Text,
    Color,
    Slider,
    Checkbox,
    Font,
    Position,
}

impl ControlKind {
    pub const ALL: [ControlKind; 6] =
        [ControlKind::Text, ControlKind::Color, ControlKind::Slider, ControlKind::Checkbox, ControlKind::Font, ControlKind::Position];
    pub fn name(self) -> &'static str {
        match self {
            ControlKind::Text => "text",
            ControlKind::Color => "color",
            ControlKind::Slider => "slider",
            ControlKind::Checkbox => "checkbox",
            ControlKind::Font => "font",
            ControlKind::Position => "position",
        }
    }
    pub fn from_name(s: &str) -> Option<ControlKind> {
        let s = s.to_ascii_lowercase();
        let s = if s == "colour" { "color".to_string() } else { s };
        ControlKind::ALL.into_iter().find(|k| k.name() == s)
    }
    /// The kind that edits parameter `param` of a layer.
    pub fn for_param(param: &str) -> ControlKind {
        match param {
            "text" | "name" => ControlKind::Text,
            "font" | "font_style" => ControlKind::Font,
            "position" | "anchor" => ControlKind::Position,
            "enabled" => ControlKind::Checkbox,
            p if p.ends_with("_color") => ControlKind::Color,
            p => match crate::find_effect(graphic::TEXT_LAYER)
                .and_then(|d| d.param(p))
                .or_else(|| crate::find_effect(graphic::SHAPE_LAYER).and_then(|d| d.param(p)))
            {
                Some(d) => match &d.default {
                    ParamValue::Bool(_) => ControlKind::Checkbox,
                    ParamValue::Color(_) => ControlKind::Color,
                    ParamValue::Vec2(_) => ControlKind::Position,
                    ParamValue::Text(_) => ControlKind::Text,
                    _ => ControlKind::Slider,
                },
                None => ControlKind::Slider,
            },
        }
    }
}

/// One editable property of a template (Essential Graphics ▸ Edit).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TemplateControl {
    /// Stable id (`[a-z0-9_-]`), unique in the template.
    pub id: String,
    /// Label shown to the user.
    pub name: String,
    pub kind: ControlKind,
    /// The layer (by uid) and parameter id it edits (`enabled` = layer visibility).
    pub layer: u64,
    pub param: String,
    /// Slider range.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
}

/// The template a placed graphic came from (kept on the clip, `GraphicMeta::template`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TemplateLink {
    pub id: String,
    pub name: String,
    pub controls: Vec<TemplateControl>,
}

/// A font embedded in a template.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FontResource {
    pub family: String,
    pub style: String,
    /// SPDX id of the font's licence (must allow redistribution).
    pub license: String,
    /// Original file name (informational).
    pub file: String,
    /// The font file, base64.
    pub data: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Resources {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub fonts: Vec<FontResource>,
}

impl Resources {
    pub fn is_empty(&self) -> bool {
        self.fonts.is_empty()
    }
}

/// A graphics template (`.fcgt`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GraphicsTemplate {
    pub format: String,
    pub version: u32,
    pub id: String,
    pub name: String,
    pub category: String,
    pub description: String,
    pub author: String,
    pub license: String,
    pub tags: Vec<String>,
    /// Frame size the template was designed for.
    pub canvas: [u32; 2],
    /// Default clip duration.
    pub duration: Tick,
    pub layers: Vec<EffectInstance>,
    /// Roll / responsive time settings (`template` is not stored here).
    pub graphic: GraphicMeta,
    pub controls: Vec<TemplateControl>,
    #[serde(skip_serializing_if = "Resources::is_empty")]
    pub resources: Resources,
}

impl Default for GraphicsTemplate {
    fn default() -> Self {
        GraphicsTemplate {
            format: TEMPLATE_FORMAT.into(),
            version: TEMPLATE_VERSION,
            id: String::new(),
            name: String::new(),
            category: "My Templates".into(),
            description: String::new(),
            author: String::new(),
            license: String::new(),
            tags: Vec::new(),
            canvas: [1920, 1080],
            duration: Tick(5 * TICKS_PER_SECOND),
            layers: Vec::new(),
            graphic: GraphicMeta::default(),
            controls: Vec::new(),
            resources: Resources::default(),
        }
    }
}

/// Problems reading or validating a template.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum TemplateError {
    #[error(
        "this file is a ZIP archive (an Adobe .mogrt or another application's package?): FilmCraft does not read motion graphics templates from other applications, only its own .fcgt graphics templates"
    )]
    ForeignPackage,
    #[error("not a FilmCraft graphics template: {0}")]
    NotTemplate(String),
    #[error("this graphics template was made by a newer FilmCraft (format version {0}; this build reads up to {TEMPLATE_VERSION})")]
    TooNew(u32),
    #[error("invalid graphics template: {0}")]
    Invalid(String),
}

/// Lower-case slug of a name (`My Lower Third` → `my-lower-third`).
pub fn slug(name: &str) -> String {
    let mut s = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            s.push(c.to_ascii_lowercase());
        } else if !s.ends_with('-') && !s.is_empty() {
            s.push('-');
        }
    }
    let s = s.trim_end_matches('-').to_string();
    if s.is_empty() { "template".into() } else { s }
}

impl GraphicsTemplate {
    /// Serialize (pretty JSON).
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    /// Read a template file. ZIP archives (such as `.mogrt` packages) are refused without being
    /// inspected.
    pub fn from_bytes(bytes: &[u8]) -> Result<GraphicsTemplate, TemplateError> {
        if bytes.starts_with(b"PK\x03\x04") || bytes.starts_with(b"PK\x05\x06") {
            return Err(TemplateError::ForeignPackage);
        }
        let text =
            std::str::from_utf8(bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes)).map_err(|_| TemplateError::NotTemplate("not UTF-8 text".into()))?;
        let v: serde_json::Value = serde_json::from_str(text).map_err(|e| TemplateError::NotTemplate(e.to_string()))?;
        if v.get("format").and_then(|f| f.as_str()) != Some(TEMPLATE_FORMAT) {
            return Err(TemplateError::NotTemplate(format!("`format` is not \"{TEMPLATE_FORMAT}\"")));
        }
        let ver = v.get("version").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
        if ver > TEMPLATE_VERSION {
            return Err(TemplateError::TooNew(ver));
        }
        if ver == 0 {
            return Err(TemplateError::Invalid("missing `version`".into()));
        }
        let t: GraphicsTemplate = serde_json::from_value(v).map_err(|e| TemplateError::Invalid(e.to_string()))?;
        t.validate()?;
        Ok(t)
    }

    /// Check the template's structure: layers are graphic layers with unique uids, controls
    /// point at existing layers and parameters, ids are unique.
    pub fn validate(&self) -> Result<(), TemplateError> {
        let bad = |m: String| Err(TemplateError::Invalid(m));
        if self.name.trim().is_empty() {
            return bad("the template has no name".into());
        }
        if self.layers.is_empty() {
            return bad("the template has no layers".into());
        }
        if self.canvas[0] == 0 || self.canvas[1] == 0 || self.canvas[0] > 16384 || self.canvas[1] > 16384 {
            return bad(format!("bad canvas size {:?}", self.canvas));
        }
        let mut uids = Vec::new();
        for (i, l) in self.layers.iter().enumerate() {
            if !graphic::is_layer(l) {
                return bad(format!("layer {i} is `{}`, not a graphic layer", l.effect));
            }
            let uid = layer_uid(l);
            if uid == 0 || uids.contains(&uid) {
                return bad(format!("layer {i} has no unique uid"));
            }
            uids.push(uid);
        }
        let mut ids = Vec::new();
        for c in &self.controls {
            if c.id.is_empty() || ids.contains(&c.id) {
                return bad(format!("control `{}` has no unique id", c.name));
            }
            ids.push(c.id.clone());
            let Some(l) = self.layers.iter().find(|l| layer_uid(l) == c.layer) else {
                return bad(format!("control `{}` points at a missing layer", c.name));
            };
            if c.param != "enabled" && !l.params.contains_key(&c.param) {
                return bad(format!("control `{}`: the layer has no parameter `{}`", c.name, c.param));
            }
        }
        for f in &self.resources.fonts {
            if base64_decode(&f.data).is_none() {
                return bad(format!("font `{}` is not valid base64", f.family));
            }
        }
        Ok(())
    }

    /// The layers and settings for a graphic clip on a `canvas`-sized frame. When the frame size
    /// differs from the template's, the layers are scaled uniformly (fit) and centred, and pin
    /// distances scale with them. The result links back to the template.
    pub fn instantiate(&self, canvas: (u32, u32)) -> (Vec<EffectInstance>, GraphicMeta) {
        let (w, h) = (canvas.0 as f64, canvas.1 as f64);
        let (tw, th) = (self.canvas[0] as f64, self.canvas[1] as f64);
        let s = (w / tw).min(h / th);
        let off = Vec2::new((w - tw * s) / 2.0, (h - th * s) / 2.0);
        let mut layers = self.layers.clone();
        if (s - 1.0).abs() > 1e-9 || off.x.abs() > 1e-9 || off.y.abs() > 1e-9 {
            for l in &mut layers {
                if let Some(p) = l.params.get_mut("position") {
                    p.map_values(|v| match v {
                        ParamValue::Vec2(q) if !q.x.is_nan() => ParamValue::Vec2(Vec2::new(q.x * s + off.x, q.y * s + off.y)),
                        o => o,
                    });
                }
                if let Some(p) = l.params.get_mut("scale") {
                    p.map_values(|v| match v {
                        ParamValue::Float(x) => ParamValue::Float(x * s),
                        o => o,
                    });
                }
                if let Some(pin) = l.layer.as_mut().and_then(|x| x.pin.as_mut()) {
                    pin.offsets.iter_mut().for_each(|o| *o *= s);
                }
            }
        }
        let mut meta = self.graphic.clone();
        meta.template = Some(TemplateLink { id: self.id.clone(), name: self.name.clone(), controls: self.controls.clone() });
        (layers, meta)
    }
}

/// A layer's uid (0 = none).
pub fn layer_uid(e: &EffectInstance) -> u64 {
    e.layer.as_ref().map_or(0, |x| x.uid)
}

/// Give every layer without a uid a fresh one (unique among `layers`).
pub fn ensure_uids(layers: &mut [EffectInstance]) {
    let mut next = layers.iter().map(layer_uid).max().unwrap_or(0) + 1;
    for l in layers.iter_mut() {
        if layer_uid(l) == 0 {
            l.layer.get_or_insert_with(Default::default).uid = next;
            next += 1;
        }
    }
}

// ------------------------------------------------------------------------------------------------
// base64 (RFC 4648, standard alphabet, padded)
// ------------------------------------------------------------------------------------------------

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for k in 0..4 {
            if k <= c.len() {
                out.push(B64[(n >> (18 - 6 * k) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

pub fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let mut acc = 0u32;
    let mut bits = 0;
    for b in s.bytes().filter(|b| !b.is_ascii_whitespace()) {
        if b == b'=' {
            break;
        }
        let v = B64.iter().position(|&x| x == b)? as u32;
        acc = acc << 6 | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

// ------------------------------------------------------------------------------------------------
// Themes: recolour a template to a channel look at apply time (the template itself is unchanged)
// ------------------------------------------------------------------------------------------------

/// Text layers that sit on an accent-coloured shape in the built-ins (they need dark ink once the
/// accent turns light amber).
const ON_ACCENT: &[&str] = &["CTA", "Tarih", "Etiket Yazısı", "Tag", "Badge Text"];

/// Recolour `t` for a channel theme. `atlas` (CrStudio's Kadim Atlas): every saturated accent
/// (red, rose, sky, yellow) becomes amber, the slate/navy grounds become warm near-black, light
/// greys become cream, and Inter becomes Barlow. Unknown themes leave the template as it is.
pub fn apply_theme(t: &mut GraphicsTemplate, theme: &str) {
    if theme != "atlas" {
        return;
    }
    const AMBER: [f32; 3] = [0.949, 0.635, 0.180];
    const GROUND: [f32; 3] = [0.055, 0.047, 0.039];
    const CREAM: [f32; 3] = [0.961, 0.914, 0.816];
    const INK: [f32; 3] = [0.078, 0.067, 0.051];
    let remap = |c: [f32; 4]| -> [f32; 4] {
        let [r, g, b, a] = c;
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let sat = if max > 0.0 { (max - min) / max } else { 0.0 };
        let out = if sat > 0.45 && max > 0.45 {
            AMBER
        } else if max < 0.25 {
            GROUND
        } else if min > 0.75 {
            CREAM
        } else {
            [r, g, b]
        };
        [out[0], out[1], out[2], a]
    };
    for e in &mut t.layers {
        let name = match e.params.get("name").map(|p| &p.value) {
            Some(ParamValue::Text(n)) => n.clone(),
            _ => String::new(),
        };
        for key in ["fill_color", "stroke_color", "background_color"] {
            if let Some(Param { value: ParamValue::Color(c), .. }) = e.params.get_mut(key) {
                *c = remap(*c);
            }
        }
        if ON_ACCENT.contains(&name.as_str())
            && let Some(Param { value: ParamValue::Color(c), .. }) = e.params.get_mut("fill_color")
        {
            *c = [INK[0], INK[1], INK[2], c[3]];
        }
        if let Some(Param { value: ParamValue::Text(f), .. }) = e.params.get_mut("font")
            && f == "Inter"
        {
            *f = "Barlow".into();
        }
    }
}

// ------------------------------------------------------------------------------------------------
// Built-in templates (original designs)
// ------------------------------------------------------------------------------------------------

fn set(e: &mut EffectInstance, id: &str, v: ParamValue) {
    e.params.insert(id.to_string(), Param::new(v));
}

fn rgb(hex: u32) -> ParamValue {
    let c = |s: u32| ((hex >> s) & 0xff) as f32 / 255.0;
    ParamValue::Color([c(16), c(8), c(0), 1.0])
}

fn text(uid: u64, name: &str, s: &str, pos: (f64, f64), size: f64, style: &str) -> EffectInstance {
    let mut e = new_text_layer(s, Vec2::new(pos.0, pos.1), size);
    set(&mut e, "name", ParamValue::Text(name.into()));
    set(&mut e, "font", ParamValue::Text("Inter".into()));
    set(&mut e, "font_style", ParamValue::Text(style.into()));
    e.layer = Some(Box::new(LayerExtra { uid, ..Default::default() }));
    e
}

fn shape(uid: u64, name: &str, kind: u32, pos: (f64, f64), size: (f64, f64), color: u32) -> EffectInstance {
    let mut e = new_shape_layer(kind, Vec2::new(pos.0, pos.1), Vec2::new(size.0, size.1), vec![]);
    set(&mut e, "name", ParamValue::Text(name.into()));
    set(&mut e, "fill_color", rgb(color));
    e.layer = Some(Box::new(LayerExtra { uid, ..Default::default() }));
    e
}

fn pin(e: &mut EffectInstance, to: u64, edges: [bool; 4], offsets: [f64; 4]) {
    let p =
        Pin { to: if to == 0 { PinTarget::Frame } else { PinTarget::Layer(to) }, left: edges[0], top: edges[1], right: edges[2], bottom: edges[3], offsets };
    e.layer.get_or_insert_with(Default::default).pin = Some(p);
}

fn ctl(id: &str, name: &str, layer: u64, param: &str) -> TemplateControl {
    TemplateControl { id: id.into(), name: name.into(), kind: ControlKind::for_param(param), layer, param: param.into(), min: None, max: None }
}

fn slider(id: &str, name: &str, layer: u64, param: &str, min: f64, max: f64) -> TemplateControl {
    TemplateControl { min: Some(min), max: Some(max), ..ctl(id, name, layer, param) }
}

fn builtin(
    id: &str,
    name: &str,
    category: &str,
    description: &str,
    seconds: i64,
    layers: Vec<EffectInstance>,
    controls: Vec<TemplateControl>,
) -> GraphicsTemplate {
    GraphicsTemplate {
        id: format!("builtin:{id}"),
        name: name.into(),
        category: category.into(),
        description: description.into(),
        author: "FilmCraft contributors".into(),
        license: "MIT OR Apache-2.0".into(),
        tags: vec![category.to_ascii_lowercase()],
        duration: Tick(seconds * TICKS_PER_SECOND),
        layers,
        controls,
        ..Default::default()
    }
}

/// The built-in templates: original FilmCraft designs (lower thirds, titles, end credits,
/// callouts, a ticker), defined here in code. Designed at 1920×1080 with the bundled Inter font.
pub fn builtin_templates() -> Vec<GraphicsTemplate> {
    let mut v = Vec::new();

    // Lower third: a slab that grows with the name, a small accent square and a role line.
    {
        let mut slab = shape(1, "Slab", 0, (400.0, 900.0), (560.0, 150.0), 0x1d4ed8);
        set(&mut slab, "opacity", ParamValue::Float(92.0));
        pin(&mut slab, 2, [true, false, true, false], [-40.0, 0.0, 48.0, 0.0]);
        let mut accent = shape(4, "Accent", 0, (130.0, 900.0), (12.0, 150.0), 0xfacc15);
        pin(&mut accent, 1, [true, false, false, false], [-12.0, 0.0, 0.0, 0.0]);
        let mut name = text(2, "Name", "Alex Morgan", (160.0, 895.0), 62.0, "SemiBold");
        set(&mut name, "fill_color", rgb(0xffffff));
        let mut role = text(3, "Role", "Field Producer", (162.0, 950.0), 36.0, "Regular");
        set(&mut role, "fill_color", rgb(0xdbeafe));
        v.push(builtin(
            "lower-third-slab",
            "Lower Third – Slab",
            "Lower Thirds",
            "Name and role on a coloured slab that widens with the name.",
            6,
            vec![slab, accent, name, role],
            vec![
                ctl("name", "Name", 2, "text"),
                ctl("role", "Role", 3, "text"),
                ctl("slab_color", "Slab Color", 1, "fill_color"),
                ctl("accent_color", "Accent Color", 4, "fill_color"),
                ctl("text_color", "Name Color", 2, "fill_color"),
                ctl("show_role", "Show Role", 3, "enabled"),
                ctl("font", "Font", 2, "font"),
            ],
        ));
    }
    // Lower third: name with a rule underneath that stretches to the name's width.
    {
        let mut name = text(1, "Name", "Sam Rivera", (140.0, 900.0), 58.0, "Bold");
        let shadow = |e: &mut EffectInstance| {
            set(e, "shadow", ParamValue::Bool(true));
            set(e, "shadow_distance", ParamValue::Float(3.0));
            set(e, "shadow_blur", ParamValue::Float(12.0));
            set(e, "shadow_opacity", ParamValue::Float(60.0));
        };
        shadow(&mut name);
        let mut rule = shape(2, "Rule", 0, (300.0, 920.0), (320.0, 5.0), 0x22d3ee);
        pin(&mut rule, 1, [true, false, true, false], [0.0, 0.0, 0.0, 0.0]);
        let mut role = text(3, "Role", "Lead Engineer", (140.0, 966.0), 34.0, "Medium");
        set(&mut role, "tracking", ParamValue::Float(40.0));
        shadow(&mut role);
        v.push(builtin(
            "lower-third-rule",
            "Lower Third – Rule",
            "Lower Thirds",
            "Name over a thin rule that always matches the name's width.",
            6,
            vec![name, rule, role],
            vec![
                ctl("name", "Name", 1, "text"),
                ctl("role", "Role", 3, "text"),
                ctl("rule_color", "Rule Color", 2, "fill_color"),
                ctl("font", "Font", 1, "font"),
                slider("name_size", "Name Size", 1, "size", 24.0, 140.0),
            ],
        ));
    }
    // Kadim Atlas lower third (CrStudio's channel look): amber tag, title in the channel face,
    // one body line, all on a dark plate that follows the title's width. Falls back to the default
    // font when the channel fonts are not installed.
    {
        let mut plate = shape(1, "Plate", 0, (420.0, 912.0), (640.0, 212.0), 0x0e0c0a);
        set(&mut plate, "opacity", ParamValue::Float(80.0));
        set(&mut plate, "corner_radius", ParamValue::Float(14.0));
        pin(&mut plate, 3, [true, false, true, false], [-44.0, 0.0, 56.0, 0.0]);
        let mut tag = shape(2, "Tag", 0, (210.0, 838.0), (200.0, 42.0), 0xf2a22e);
        pin(&mut tag, 4, [true, false, true, false], [-16.0, 0.0, 16.0, 0.0]);
        let mut kicker = text(4, "Kicker", "BÖLÜM", (132.0, 851.0), 27.0, "Bold");
        set(&mut kicker, "font", ParamValue::Text("Barlow Condensed".into()));
        set(&mut kicker, "fill_color", rgb(0x14110d));
        let mut title = text(3, "Title", "YEDİ UYURLAR", (130.0, 928.0), 68.0, "Regular");
        set(&mut title, "font", ParamValue::Text("Efsaneler Başlık Sülüs".into()));
        set(&mut title, "fill_color", rgb(0xf5e9d0));
        let mut body = text(5, "Body", "Efes'te yedi genç bir mağaraya sığındı.", (132.0, 980.0), 32.0, "SemiBold");
        set(&mut body, "font", ParamValue::Text("Barlow".into()));
        set(&mut body, "fill_color", rgb(0xf5e9d0));
        v.push(builtin(
            "atlas-lower-third",
            "Lower Third – Kadim Atlas",
            "Lower Thirds",
            "Amber tag, title in the channel face and one line on a dark plate (CrStudio channel look).",
            6,
            vec![plate, tag, kicker, title, body],
            vec![
                ctl("kicker", "Tag", 4, "text"),
                ctl("title", "Title", 3, "text"),
                ctl("body", "Body", 5, "text"),
                ctl("accent", "Accent Color", 2, "fill_color"),
            ],
        ));
    }
    // Centred title with a subtitle and a short rule between them.
    {
        let mut title = text(1, "Title", "THE LONG WAY HOME", (960.0, 520.0), 112.0, "Bold");
        set(&mut title, "align", ParamValue::Choice(1));
        set(&mut title, "tracking", ParamValue::Float(60.0));
        let mut rule = shape(2, "Rule", 0, (960.0, 556.0), (220.0, 4.0), 0xf97316);
        set(&mut rule, "opacity", ParamValue::Float(90.0));
        let mut sub = text(3, "Subtitle", "a story in three parts", (960.0, 628.0), 44.0, "Italic");
        set(&mut sub, "align", ParamValue::Choice(1));
        set(&mut sub, "fill_color", rgb(0xe5e7eb));
        v.push(builtin(
            "title-centered",
            "Title – Centered",
            "Titles",
            "Large centred title, accent rule and subtitle.",
            5,
            vec![title, rule, sub],
            vec![
                ctl("title", "Title", 1, "text"),
                ctl("subtitle", "Subtitle", 3, "text"),
                slider("title_size", "Title Size", 1, "size", 40.0, 240.0),
                ctl("title_color", "Title Color", 1, "fill_color"),
                ctl("accent", "Accent Color", 2, "fill_color"),
                ctl("font", "Font", 1, "font"),
            ],
        ));
    }
    // Title in a box (text background) with a soft shadow.
    {
        let mut title = text(1, "Title", "Chapter One", (960.0, 560.0), 96.0, "SemiBold");
        set(&mut title, "align", ParamValue::Choice(1));
        set(&mut title, "fill_color", rgb(0x111827));
        set(&mut title, "background", ParamValue::Bool(true));
        set(&mut title, "background_color", rgb(0xfef3c7));
        set(&mut title, "background_opacity", ParamValue::Float(100.0));
        set(&mut title, "background_size", ParamValue::Float(36.0));
        set(&mut title, "background_radius", ParamValue::Float(10.0));
        set(&mut title, "shadow", ParamValue::Bool(true));
        set(&mut title, "shadow_distance", ParamValue::Float(8.0));
        set(&mut title, "shadow_blur", ParamValue::Float(30.0));
        set(&mut title, "shadow_opacity", ParamValue::Float(45.0));
        v.push(builtin(
            "title-boxed",
            "Title – Boxed",
            "Titles",
            "A title on a rounded card with a soft shadow.",
            5,
            vec![title],
            vec![
                ctl("title", "Title", 1, "text"),
                ctl("text_color", "Text Color", 1, "fill_color"),
                ctl("box_color", "Box Color", 1, "background_color"),
                slider("box_opacity", "Box Opacity", 1, "background_opacity", 0.0, 100.0),
                ctl("position", "Position", 1, "position"),
            ],
        ));
    }
    // End credits that roll from below the frame to above it.
    {
        let mut heading = text(1, "Heading", "CREDITS", (960.0, 160.0), 72.0, "Bold");
        set(&mut heading, "align", ParamValue::Choice(1));
        set(&mut heading, "tracking", ParamValue::Float(120.0));
        let body = "Directed by\nJordan Lee\n\nWritten by\nPriya Natarajan\n\nEdited by\nMarco Bianchi\n\nMusic by\nHana Kobayashi\n\nMade with FilmCraft";
        let mut credits = text(2, "Credits", body, (960.0, 300.0), 46.0, "Regular");
        set(&mut credits, "align", ParamValue::Choice(1));
        set(&mut credits, "leading", ParamValue::Float(10.0));
        let mut t = builtin(
            "end-credits-roll",
            "End Credits – Roll",
            "End Credits",
            "Centred credits that roll up from below the frame and leave at the top.",
            12,
            vec![heading, credits],
            vec![
                ctl("heading", "Heading", 1, "text"),
                ctl("credits", "Credits", 2, "text"),
                ctl("font", "Font", 2, "font"),
                ctl("color", "Text Color", 2, "fill_color"),
            ],
        );
        t.graphic.roll = Roll { mode: RollMode::Roll, start_off_screen: true, end_off_screen: true, ..Default::default() };
        v.push(t);
    }
    // Callout: a label on a box with a pointer that follows it.
    {
        let mut label = text(1, "Label", "Look here", (1260.0, 380.0), 48.0, "SemiBold");
        set(&mut label, "fill_color", rgb(0xffffff));
        set(&mut label, "background", ParamValue::Bool(true));
        set(&mut label, "background_color", rgb(0xdc2626));
        set(&mut label, "background_opacity", ParamValue::Float(100.0));
        set(&mut label, "background_size", ParamValue::Float(18.0));
        set(&mut label, "background_radius", ParamValue::Float(6.0));
        let mut dot = shape(2, "Pointer", 1, (1200.0, 470.0), (34.0, 34.0), 0xdc2626);
        set(&mut dot, "stroke", ParamValue::Bool(true));
        set(&mut dot, "stroke_color", rgb(0xffffff));
        set(&mut dot, "stroke_width", ParamValue::Float(5.0));
        pin(&mut dot, 1, [true, true, false, false], [-60.0, 55.0, 0.0, 0.0]);
        v.push(builtin(
            "callout-pointer",
            "Callout – Pointer",
            "Callouts",
            "A boxed label with a round pointer; move the label and the pointer follows.",
            4,
            vec![dot, label],
            vec![
                ctl("label", "Label", 1, "text"),
                ctl("color", "Color", 1, "background_color"),
                ctl("pointer", "Pointer Color", 2, "fill_color"),
                ctl("position", "Position", 1, "position"),
            ],
        ));
    }
    // Callout: a pill-shaped tag around its text, with a status dot.
    {
        let mut tag = shape(1, "Tag", 0, (300.0, 140.0), (300.0, 70.0), 0x0f172a);
        set(&mut tag, "corner_radius", ParamValue::Float(35.0));
        set(&mut tag, "opacity", ParamValue::Float(88.0));
        pin(&mut tag, 2, [true; 4], [-70.0, -18.0, 30.0, 18.0]);
        let mut dot = shape(3, "Dot", 1, (190.0, 140.0), (22.0, 22.0), 0x22c55e);
        pin(&mut dot, 2, [true, false, false, false], [-44.0, 0.0, 0.0, 0.0]);
        let mut label = text(2, "Text", "LIVE", (220.0, 155.0), 40.0, "Bold");
        set(&mut label, "tracking", ParamValue::Float(80.0));
        v.push(builtin(
            "callout-tag",
            "Callout – Tag",
            "Callouts",
            "A rounded tag that fits its text, with a status dot.",
            4,
            vec![tag, dot, label],
            vec![
                ctl("text", "Text", 2, "text"),
                ctl("tag_color", "Tag Color", 1, "fill_color"),
                ctl("dot_color", "Dot Color", 3, "fill_color"),
                ctl("show_dot", "Show Dot", 3, "enabled"),
            ],
        ));
    }
    // Ticker: a crawl from right to left.
    {
        let mut line = text(
            1,
            "Ticker",
            "Breaking: the river festival moves to Saturday  •  Roads close at 6 pm  •  Shuttle buses run every 15 minutes",
            (40.0, 1010.0),
            44.0,
            "Medium",
        );
        set(&mut line, "background", ParamValue::Bool(true));
        set(&mut line, "background_color", rgb(0x111827));
        set(&mut line, "background_opacity", ParamValue::Float(85.0));
        set(&mut line, "background_size", ParamValue::Float(14.0));
        let mut t = builtin(
            "ticker-crawl",
            "Ticker – Crawl",
            "Lower Thirds",
            "A news-style line that crawls in from the right and leaves at the left.",
            15,
            vec![line],
            vec![ctl("text", "Text", 1, "text"), ctl("text_color", "Text Color", 1, "fill_color"), ctl("bar_color", "Bar Color", 1, "background_color")],
        );
        t.graphic.roll = Roll { mode: RollMode::CrawlLeft, start_off_screen: true, end_off_screen: true, ..Default::default() };
        v.push(t);
    }
    // Documentary: Map Location Pin + coordinate card (CrStudio).
    {
        let mut ring = shape(1, "Pin Ring", 1, (960.0, 470.0), (54.0, 54.0), 0xef4444);
        set(&mut ring, "opacity", ParamValue::Float(85.0));
        set(&mut ring, "stroke", ParamValue::Bool(true));
        set(&mut ring, "stroke_color", rgb(0xffffff));
        set(&mut ring, "stroke_width", ParamValue::Float(4.0));
        let core = shape(2, "Pin Core", 1, (960.0, 470.0), (20.0, 20.0), 0xffffff);
        let mut card = shape(3, "Card", 0, (960.0, 575.0), (520.0, 108.0), 0x0f172a);
        set(&mut card, "corner_radius", ParamValue::Float(12.0));
        set(&mut card, "opacity", ParamValue::Float(92.0));
        let mut loc = text(4, "Location", "ISTANBUL, TURKEY", (960.0, 562.0), 42.0, "Bold");
        set(&mut loc, "align", ParamValue::Choice(1));
        set(&mut loc, "fill_color", rgb(0xffffff));
        set(&mut loc, "tracking", ParamValue::Float(40.0));
        let mut coords = text(5, "Coordinates", "41.0082 N, 28.9784 E  •  LOCATION FOCUS", (960.0, 606.0), 24.0, "Medium");
        set(&mut coords, "align", ParamValue::Choice(1));
        set(&mut coords, "fill_color", rgb(0x38bdf8));
        v.push(builtin(
            "doc-map-pin",
            "Documentary – Map Location Pin",
            "Callouts",
            "Animated documentary map marker with location title and coordinates.",
            5,
            vec![card, ring, core, loc, coords],
            vec![
                ctl("location", "Location", 4, "text"),
                ctl("coordinates", "Coordinates", 5, "text"),
                ctl("pin_color", "Pin Color", 1, "fill_color"),
                ctl("card_color", "Card Color", 3, "fill_color"),
                ctl("position", "Pin Position", 1, "position"),
            ],
        ));
    }
    // Documentary: Evidence / Newspaper Highlight Quote (CrStudio).
    {
        let mut card = shape(1, "Evidence Card", 0, (960.0, 860.0), (1360.0, 185.0), 0x0f172a);
        set(&mut card, "corner_radius", ParamValue::Float(14.0));
        set(&mut card, "opacity", ParamValue::Float(92.0));
        let mut bar = shape(2, "Highlight Bar", 0, (960.0, 835.0), (1260.0, 72.0), 0xfacc15);
        set(&mut bar, "corner_radius", ParamValue::Float(6.0));
        set(&mut bar, "opacity", ParamValue::Float(95.0));
        let mut quote = text(3, "Quote", "\"The decisive turning point remained classified for decades.\"", (960.0, 848.0), 40.0, "Bold");
        set(&mut quote, "align", ParamValue::Choice(1));
        set(&mut quote, "fill_color", rgb(0x111827));
        let mut source = text(4, "Source", "SOURCE: HISTORICAL ARCHIVE DOCUMENT", (960.0, 916.0), 26.0, "SemiBold");
        set(&mut source, "align", ParamValue::Choice(1));
        set(&mut source, "fill_color", rgb(0xe2e8f0));
        set(&mut source, "tracking", ParamValue::Float(50.0));
        v.push(builtin(
            "doc-highlight-quote",
            "Documentary – Evidence Highlight",
            "Callouts",
            "Vox-style documentary evidence highlighter with source citation.",
            6,
            vec![card, bar, quote, source],
            vec![
                ctl("quote", "Quote", 3, "text"),
                ctl("source", "Source", 4, "text"),
                ctl("highlight_color", "Highlight Color", 2, "fill_color"),
                ctl("card_color", "Card Color", 1, "fill_color"),
            ],
        ));
    }
    // Documentary: Chapter Title Card (CrStudio).
    {
        let mut backdrop = shape(1, "Backdrop", 0, (960.0, 540.0), (1040.0, 330.0), 0x090d16);
        set(&mut backdrop, "corner_radius", ParamValue::Float(18.0));
        set(&mut backdrop, "opacity", ParamValue::Float(88.0));
        let mut chap = text(2, "Chapter", "BÖLÜM I", (960.0, 430.0), 34.0, "Bold");
        set(&mut chap, "align", ParamValue::Choice(1));
        set(&mut chap, "fill_color", rgb(0xf59e0b));
        set(&mut chap, "tracking", ParamValue::Float(120.0));
        let rule = shape(3, "Accent Line", 0, (960.0, 462.0), (160.0, 5.0), 0xef4444);
        let mut title = text(4, "Title", "GİZLİ DOSYA", (960.0, 560.0), 82.0, "Bold");
        set(&mut title, "align", ParamValue::Choice(1));
        set(&mut title, "fill_color", rgb(0xffffff));
        let mut sub = text(5, "Subtitle", "Tarihin akışını değiştiren kritik kırılma noktası", (960.0, 632.0), 34.0, "Regular");
        set(&mut sub, "align", ParamValue::Choice(1));
        set(&mut sub, "fill_color", rgb(0xcbd5e1));
        v.push(builtin(
            "doc-chapter-card",
            "Documentary – Chapter Title",
            "Titles",
            "Cinematic documentary chapter card with chapter number, accent rule and subtitle.",
            5,
            vec![backdrop, chap, rule, title, sub],
            vec![
                ctl("chapter", "Chapter", 2, "text"),
                ctl("title", "Title", 4, "text"),
                ctl("subtitle", "Subtitle", 5, "text"),
                ctl("accent_color", "Accent Color", 3, "fill_color"),
                ctl("title_color", "Title Color", 4, "fill_color"),
            ],
        ));
    }
    // Shorts / Reels: Viral Hook Banner (CrStudio).
    {
        let mut banner = shape(1, "Banner", 0, (960.0, 195.0), (960.0, 114.0), 0xdc2626);
        set(&mut banner, "corner_radius", ParamValue::Float(18.0));
        set(&mut banner, "opacity", ParamValue::Float(95.0));
        let mut badge = shape(2, "Badge", 0, (960.0, 128.0), (360.0, 44.0), 0xfacc15);
        set(&mut badge, "corner_radius", ParamValue::Float(10.0));
        let mut tag = text(3, "Tag", "BUNU BİLİYOR MUYDUNUZ?", (960.0, 138.0), 26.0, "Bold");
        set(&mut tag, "align", ParamValue::Choice(1));
        set(&mut tag, "fill_color", rgb(0x111827));
        let mut headline = text(4, "Headline", "HERKESİN GÖZDEN KAÇIRDIĞI DETAY", (960.0, 210.0), 48.0, "Bold");
        set(&mut headline, "align", ParamValue::Choice(1));
        set(&mut headline, "fill_color", rgb(0xffffff));
        v.push(builtin(
            "shorts-dynamic-hook",
            "Shorts – Viral Hook Banner",
            "Titles",
            "High-retention top hook banner for YouTube Shorts and Instagram Reels.",
            4,
            vec![banner, badge, tag, headline],
            vec![
                ctl("tag", "Tag", 3, "text"),
                ctl("headline", "Headline", 4, "text"),
                ctl("banner_color", "Banner Color", 1, "fill_color"),
                ctl("badge_color", "Badge Color", 2, "fill_color"),
            ],
        ));
    }
    // YouTube: Subscribe & Notification Bell Callout (CrStudio Brand Kit).
    {
        let mut pill = shape(1, "Subscribe Pill", 0, (1540.0, 930.0), (540.0, 96.0), 0x0f172a);
        set(&mut pill, "corner_radius", ParamValue::Float(48.0));
        set(&mut pill, "opacity", ParamValue::Float(94.0));
        let mut btn = shape(2, "Red Button", 0, (1395.0, 930.0), (210.0, 68.0), 0xdc2626);
        set(&mut btn, "corner_radius", ParamValue::Float(34.0));
        let mut cta = text(3, "CTA", "ABONE OL", (1395.0, 942.0), 30.0, "Bold");
        set(&mut cta, "align", ParamValue::Choice(1));
        set(&mut cta, "fill_color", rgb(0xffffff));
        let mut ch_name = text(4, "Channel", "CRSTUDIO BELGESEL", (1645.0, 924.0), 28.0, "Bold");
        set(&mut ch_name, "align", ParamValue::Choice(1));
        set(&mut ch_name, "fill_color", rgb(0xfacc15));
        let mut sub_note = text(5, "Note", "BİLDİRİMLERİ AÇ • YENİ BÖLÜMLER", (1645.0, 956.0), 19.0, "Medium");
        set(&mut sub_note, "align", ParamValue::Choice(1));
        set(&mut sub_note, "fill_color", rgb(0xe2e8f0));
        v.push(builtin(
            "youtube-subscribe-cta",
            "YouTube – Subscribe Callout",
            "Callouts",
            "Channel branding subscribe pill with notification reminder.",
            4,
            vec![pill, btn, cta, ch_name, sub_note],
            vec![
                ctl("channel", "Channel Name", 4, "text"),
                ctl("cta", "Button Text", 3, "text"),
                ctl("note", "Subtitle", 5, "text"),
                ctl("button_color", "Button Color", 2, "fill_color"),
            ],
        ));
    }
    // YouTube: Documentary End Screen Frame (Next Video + Subscribe Circle).
    {
        let mut backdrop = shape(1, "End Backdrop", 0, (960.0, 540.0), (1680.0, 820.0), 0x090d16);
        set(&mut backdrop, "corner_radius", ParamValue::Float(24.0));
        set(&mut backdrop, "opacity", ParamValue::Float(86.0));
        let mut next_box = shape(2, "Next Video Frame", 0, (620.0, 560.0), (720.0, 405.0), 0x1e293b);
        set(&mut next_box, "corner_radius", ParamValue::Float(16.0));
        set(&mut next_box, "stroke", ParamValue::Bool(true));
        set(&mut next_box, "stroke_color", rgb(0xfacc15));
        set(&mut next_box, "stroke_width", ParamValue::Float(5.0));
        let mut sub_ring = shape(3, "Subscribe Circle", 1, (1390.0, 560.0), (240.0, 240.0), 0xdc2626);
        set(&mut sub_ring, "stroke", ParamValue::Bool(true));
        set(&mut sub_ring, "stroke_color", rgb(0xffffff));
        set(&mut sub_ring, "stroke_width", ParamValue::Float(5.0));
        let mut heading = text(4, "Heading", "SIRADAKİ ARAŞTIRMA DOSYASI", (960.0, 245.0), 54.0, "Bold");
        set(&mut heading, "align", ParamValue::Choice(1));
        set(&mut heading, "fill_color", rgb(0xfacc15));
        let mut ch_label = text(5, "Channel", "ABONE OL & TAKİPTE KAL", (1390.0, 825.0), 32.0, "Bold");
        set(&mut ch_label, "align", ParamValue::Choice(1));
        set(&mut ch_label, "fill_color", rgb(0xffffff));
        v.push(builtin(
            "youtube-end-screen",
            "YouTube – End Screen Card",
            "End Credits",
            "YouTube documentary end screen with Next Video placeholder frame and Subscribe badge.",
            5,
            vec![backdrop, next_box, sub_ring, heading, ch_label],
            vec![ctl("heading", "Heading", 4, "text"), ctl("channel", "Subscribe Label", 5, "text"), ctl("frame_color", "Frame Border", 2, "stroke_color")],
        ));
    }
    // ---------------------------------------------------------------------------------------
    // CrStudio YouTuber pack (original designs; colours are controls so the channel kit can set them)
    // ---------------------------------------------------------------------------------------
    // Info box (top right): accent bar, title, wrapped body.
    {
        let mut card = shape(1, "Kart", 0, (1500.0, 205.0), (720.0, 190.0), 0x0b1220);
        set(&mut card, "corner_radius", ParamValue::Float(18.0));
        set(&mut card, "opacity", ParamValue::Float(93.0));
        let bar = shape(2, "Vurgu", 0, (1146.0, 205.0), (12.0, 190.0), 0x38bdf8);
        let mut title = text(3, "Başlık", "BİLİYOR MUYDUNUZ?", (1172.0, 150.0), 28.0, "Bold");
        set(&mut title, "fill_color", rgb(0x38bdf8));
        set(&mut title, "tracking", ParamValue::Float(60.0));
        let mut body = text(4, "Metin", "Bu efsane en az beş farklı kültürde anlatılır.", (1172.0, 172.0), 32.0, "Medium");
        set(&mut body, "fill_color", rgb(0xf1f5f9));
        set(&mut body, "box_width", ParamValue::Float(650.0));
        set(&mut body, "box_height", ParamValue::Float(110.0));
        v.push(builtin(
            "yt-info-box",
            "YouTuber – Bilgi Kutusu",
            "Callouts",
            "Ek bilgi kutusu: vurgu çubuğu, başlık ve kendiliğinden kırılan metin.",
            5,
            vec![card, bar, title, body],
            vec![
                ctl("title", "Başlık", 3, "text"),
                ctl("text", "Metin", 4, "text"),
                ctl("accent", "Vurgu Rengi", 2, "fill_color"),
                ctl("card", "Kart Rengi", 1, "fill_color"),
            ],
        ));
    }
    // Warning box: amber, for "commonly misunderstood" facts.
    {
        let mut card = shape(1, "Kart", 0, (1500.0, 205.0), (720.0, 190.0), 0x1c1204);
        set(&mut card, "corner_radius", ParamValue::Float(18.0));
        set(&mut card, "opacity", ParamValue::Float(94.0));
        set(&mut card, "stroke", ParamValue::Bool(true));
        set(&mut card, "stroke_color", rgb(0xf59e0b));
        set(&mut card, "stroke_width", ParamValue::Float(3.0));
        let mut title = text(3, "Başlık", "DİKKAT", (1172.0, 150.0), 30.0, "Bold");
        set(&mut title, "fill_color", rgb(0xfbbf24));
        let mut body = text(4, "Metin", "Bu bilgi sık sık yanlış aktarılır.", (1172.0, 172.0), 32.0, "Medium");
        set(&mut body, "fill_color", rgb(0xfef3c7));
        set(&mut body, "box_width", ParamValue::Float(650.0));
        set(&mut body, "box_height", ParamValue::Float(110.0));
        v.push(builtin(
            "yt-warning-box",
            "YouTuber – Uyarı Kutusu",
            "Callouts",
            "Yanlış bilinen bir şeyi düzelten uyarı kutusu.",
            5,
            vec![card, title, body],
            vec![ctl("title", "Başlık", 3, "text"), ctl("text", "Metin", 4, "text"), ctl("border", "Çerçeve Rengi", 1, "stroke_color")],
        ));
    }
    // Speech bubble: a rounded bubble with a tail.
    {
        let mut bubble = shape(1, "Balon", 0, (1300.0, 300.0), (760.0, 200.0), 0xffffff);
        set(&mut bubble, "corner_radius", ParamValue::Float(60.0));
        let tail = shape(2, "Kuyruk", 2, (1010.0, 420.0), (70.0, 70.0), 0xffffff);
        let mut words = text(3, "Söz", "“Rabbimiz göklerin ve yerin Rabbidir.”", (950.0, 240.0), 38.0, "SemiBold");
        set(&mut words, "fill_color", rgb(0x0f172a));
        set(&mut words, "box_width", ParamValue::Float(700.0));
        set(&mut words, "box_height", ParamValue::Float(150.0));
        v.push(builtin(
            "yt-speech-bubble",
            "YouTuber – Konuşma Balonu",
            "Callouts",
            "Bir karakterin sözü için konuşma balonu.",
            5,
            vec![bubble, tail, words],
            vec![ctl("text", "Söz", 3, "text"), ctl("bubble", "Balon Rengi", 1, "fill_color"), ctl("position", "Konum", 1, "position")],
        ));
    }
    // Comment prompt: a question to the audience with a comment icon.
    {
        let mut card = shape(1, "Kart", 0, (960.0, 880.0), (1300.0, 150.0), 0x111827);
        set(&mut card, "corner_radius", ParamValue::Float(75.0));
        set(&mut card, "opacity", ParamValue::Float(94.0));
        let icon = shape(2, "Simge", 0, (380.0, 880.0), (96.0, 96.0), 0xe11d48);
        let mut q = text(3, "Soru", "Sizce hangisi doğru? Yorumlarda yazın!", (460.0, 845.0), 40.0, "Bold");
        set(&mut q, "fill_color", rgb(0xffffff));
        set(&mut q, "box_width", ParamValue::Float(1120.0));
        set(&mut q, "box_height", ParamValue::Float(110.0));
        let mut ic = text(4, "İkon", "?", (380.0, 900.0), 52.0, "Regular");
        set(&mut ic, "align", ParamValue::Choice(1));
        v.push(builtin(
            "yt-comment-prompt",
            "YouTuber – Yorumda Belirt",
            "Callouts",
            "İzleyiciye soru: yorumlara davet kartı.",
            5,
            vec![card, icon, q, ic],
            vec![ctl("question", "Soru", 3, "text"), ctl("accent", "Simge Rengi", 2, "fill_color")],
        ));
    }
    // Like reminder: a pill with a thumbs-up badge.
    {
        let mut pill = shape(1, "Hap", 0, (300.0, 960.0), (440.0, 96.0), 0x0f172a);
        set(&mut pill, "corner_radius", ParamValue::Float(48.0));
        set(&mut pill, "opacity", ParamValue::Float(94.0));
        let badge = shape(2, "Rozet", 1, (135.0, 960.0), (78.0, 78.0), 0xe11d48);
        let mut thumb = text(3, "Simge", "+1", (135.0, 978.0), 44.0, "Regular");
        set(&mut thumb, "align", ParamValue::Choice(1));
        let mut t = text(4, "Metin", "Beğenmeyi unutma!", (190.0, 972.0), 34.0, "Bold");
        set(&mut t, "fill_color", rgb(0xffffff));
        v.push(builtin(
            "yt-like-reminder",
            "YouTuber – Beğen Hatırlatıcı",
            "Callouts",
            "Beğen hatırlatması: başparmak rozetli hap.",
            4,
            vec![pill, badge, thumb, t],
            vec![ctl("text", "Metin", 4, "text"), ctl("badge", "Rozet Rengi", 2, "fill_color")],
        ));
    }
    // Source credit (bottom left, small).
    {
        let mut t = text(1, "Kaynak", "Kaynak: —", (60.0, 1040.0), 24.0, "Medium");
        set(&mut t, "fill_color", rgb(0xe2e8f0));
        set(&mut t, "background", ParamValue::Bool(true));
        set(&mut t, "background_color", rgb(0x0b1220));
        set(&mut t, "background_opacity", ParamValue::Float(80.0));
        set(&mut t, "background_size", ParamValue::Float(10.0));
        set(&mut t, "background_radius", ParamValue::Float(6.0));
        v.push(builtin(
            "yt-source-credit",
            "YouTuber – Kaynak Gösterimi",
            "Lower Thirds",
            "Ekranın altında küçük kaynak satırı.",
            5,
            vec![t],
            vec![ctl("source", "Kaynak", 1, "text")],
        ));
    }
    // Timeline tag: a date stamp (top left).
    {
        let mut box_ = shape(1, "Kutu", 0, (210.0, 120.0), (300.0, 84.0), 0xe11d48);
        set(&mut box_, "corner_radius", ParamValue::Float(10.0));
        let mut d = text(2, "Tarih", "MS 250", (210.0, 135.0), 44.0, "Bold");
        set(&mut d, "align", ParamValue::Choice(1));
        set(&mut d, "fill_color", rgb(0xffffff));
        pin(&mut box_, 2, [true; 4], [-36.0, -14.0, 36.0, 14.0]);
        v.push(builtin(
            "yt-timeline-tag",
            "YouTuber – Tarih Etiketi",
            "Callouts",
            "Kronoloji için tarih damgası; kutu metne göre genişler.",
            4,
            vec![box_, d],
            vec![ctl("date", "Tarih", 2, "text"), ctl("color", "Renk", 1, "fill_color")],
        ));
    }
    // Countdown number (list videos).
    {
        let ring = shape(1, "Halka", 1, (960.0, 540.0), (300.0, 300.0), 0xe11d48);
        let mut n = text(2, "Sayı", "3", (960.0, 600.0), 180.0, "Bold");
        set(&mut n, "align", ParamValue::Choice(1));
        set(&mut n, "fill_color", rgb(0xffffff));
        v.push(builtin(
            "yt-countdown",
            "YouTuber – Geri Sayım",
            "Titles",
            "Sıralama videoları için büyük sayı.",
            2,
            vec![ring, n],
            vec![ctl("number", "Sayı", 2, "text"), ctl("color", "Renk", 1, "fill_color")],
        ));
    }
    // Shock badge: tilted bold sticker.
    {
        let mut t = text(1, "Metin", "İNANILMAZ!", (1500.0, 220.0), 64.0, "Bold");
        set(&mut t, "align", ParamValue::Choice(1));
        set(&mut t, "fill_color", rgb(0x111827));
        set(&mut t, "background", ParamValue::Bool(true));
        set(&mut t, "background_color", rgb(0xfacc15));
        set(&mut t, "background_opacity", ParamValue::Float(100.0));
        set(&mut t, "background_size", ParamValue::Float(22.0));
        set(&mut t, "background_radius", ParamValue::Float(12.0));
        set(&mut t, "rotation", ParamValue::Float(-6.0));
        v.push(builtin(
            "yt-shock-badge",
            "YouTuber – Vurgu Etiketi",
            "Callouts",
            "Eğik, parlak vurgu etiketi.",
            3,
            vec![t],
            vec![ctl("text", "Metin", 1, "text"), ctl("color", "Zemin Rengi", 1, "background_color")],
        ));
    }
    // Watch next end screen: two video slots + subscribe circle (YouTube end-screen friendly).
    {
        let mut back = shape(1, "Zemin", 0, (960.0, 540.0), (1920.0, 1080.0), 0x05070d);
        set(&mut back, "opacity", ParamValue::Float(70.0));
        let mut a = shape(2, "Video 1", 0, (560.0, 560.0), (640.0, 360.0), 0x111827);
        set(&mut a, "corner_radius", ParamValue::Float(14.0));
        set(&mut a, "stroke", ParamValue::Bool(true));
        set(&mut a, "stroke_color", rgb(0xffffff));
        set(&mut a, "stroke_width", ParamValue::Float(3.0));
        let mut b = shape(3, "Video 2", 0, (1260.0, 560.0), (640.0, 360.0), 0x111827);
        set(&mut b, "corner_radius", ParamValue::Float(14.0));
        set(&mut b, "stroke", ParamValue::Bool(true));
        set(&mut b, "stroke_color", rgb(0xffffff));
        set(&mut b, "stroke_width", ParamValue::Float(3.0));
        let sub = shape(4, "Abone Halkası", 1, (1700.0, 900.0), (170.0, 170.0), 0xe11d48);
        let mut h = text(5, "Başlık", "BUNU DA İZLE", (960.0, 250.0), 72.0, "Bold");
        set(&mut h, "align", ParamValue::Choice(1));
        set(&mut h, "fill_color", rgb(0xffffff));
        set(&mut h, "tracking", ParamValue::Float(40.0));
        let mut ch = text(6, "Kanal", "ABONE OL", (1700.0, 1015.0), 30.0, "Bold");
        set(&mut ch, "align", ParamValue::Choice(1));
        set(&mut ch, "fill_color", rgb(0xffffff));
        v.push(builtin(
            "yt-watch-next",
            "YouTuber – Bunu da İzle (Bitiş Ekranı)",
            "End Credits",
            "Son 15-20 saniye: iki video yeri ve abone halkası (YouTube bitiş ekranı öğeleriyle hizalı).",
            15,
            vec![back, a, b, sub, h, ch],
            vec![ctl("heading", "Başlık", 5, "text"), ctl("channel", "Abone Yazısı", 6, "text"), ctl("accent", "Abone Rengi", 4, "fill_color")],
        ));
    }
    // Sponsor / ad band: a slim full-width strip below the caption zone (y 1004–1080) — used by the Assistant.
    {
        let mut band = shape(1, "Kuşak", 0, (960.0, 1042.0), (1920.0, 76.0), 0x0b1220);
        set(&mut band, "opacity", ParamValue::Float(94.0));
        let mut tag = shape(2, "Etiket", 0, (150.0, 1042.0), (220.0, 76.0), 0xe11d48);
        set(&mut tag, "opacity", ParamValue::Float(100.0));
        let mut sp = text(3, "Etiket Yazısı", "SPONSOR", (150.0, 1052.0), 24.0, "Bold");
        set(&mut sp, "align", ParamValue::Choice(1));
        set(&mut sp, "fill_color", rgb(0xffffff));
        set(&mut sp, "tracking", ParamValue::Float(80.0));
        let mut brand = text(4, "Marka", "MARKA ADI", (300.0, 1031.0), 28.0, "Bold");
        set(&mut brand, "fill_color", rgb(0xffffff));
        let mut msg = text(5, "Mesaj", "Kısa reklam mesajı burada", (300.0, 1064.0), 21.0, "Regular");
        set(&mut msg, "fill_color", rgb(0xcbd5e1));
        let mut cta = text(6, "Çağrı", "Detaylar açıklamada", (1820.0, 1052.0), 26.0, "SemiBold");
        set(&mut cta, "align", ParamValue::Choice(2));
        set(&mut cta, "fill_color", rgb(0xfacc15));
        let mut t = builtin(
            "yt-ad-band",
            "YouTuber – Reklam / Sponsor Kuşağı",
            "Lower Thirds",
            "Altyazının altında, tam genişlikte ince kuşak: sponsor etiketi, marka, mesaj ve çağrı.",
            8,
            vec![band, tag, sp, brand, msg, cta],
            vec![
                ctl("label", "Etiket", 3, "text"),
                ctl("brand", "Marka", 4, "text"),
                ctl("text", "Mesaj", 5, "text"),
                ctl("cta", "Çağrı", 6, "text"),
                ctl("accent", "Etiket Rengi", 2, "fill_color"),
                ctl("band", "Kuşak Rengi", 1, "fill_color"),
            ],
        );
        t.tags.push("reklam".into());
        v.push(t);
    }
    // Thumbnail headline (used to render the 1280×720 cover; designed at 1920×1080).
    {
        // two lines of 168 pt fit between y≈300 and the channel strip at y≈930
        let mut h = text(1, "Başlık", "309 YIL UYKU", (110.0, 300.0), 168.0, "Bold");
        set(&mut h, "fill_color", rgb(0xffffff));
        set(&mut h, "stroke", ParamValue::Bool(true));
        set(&mut h, "stroke_color", rgb(0x000000));
        set(&mut h, "stroke_width", ParamValue::Float(10.0));
        set(&mut h, "shadow", ParamValue::Bool(true));
        set(&mut h, "shadow_distance", ParamValue::Float(8.0));
        set(&mut h, "shadow_blur", ParamValue::Float(24.0));
        set(&mut h, "shadow_opacity", ParamValue::Float(70.0));
        set(&mut h, "box_width", ParamValue::Float(1150.0));
        set(&mut h, "box_height", ParamValue::Float(520.0));
        let mut bar = shape(2, "Şerit", 0, (330.0, 960.0), (440.0, 80.0), 0xe11d48);
        set(&mut bar, "corner_radius", ParamValue::Float(10.0));
        let mut tag = text(3, "Kanal", "CRSTUDIO", (330.0, 975.0), 40.0, "Bold");
        set(&mut tag, "align", ParamValue::Choice(1));
        set(&mut tag, "fill_color", rgb(0xffffff));
        set(&mut tag, "tracking", ParamValue::Float(60.0));
        v.push(builtin(
            "yt-thumbnail-bold",
            "YouTuber – Kapak Başlığı",
            "Titles",
            "Kapak (thumbnail) için kalın, konturlu büyük başlık ve kanal şeridi.",
            5,
            vec![h, bar, tag],
            vec![ctl("headline", "Başlık", 1, "text"), ctl("tag", "Kanal", 3, "text"), ctl("accent", "Şerit Rengi", 2, "fill_color")],
        ));
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trip() {
        for n in 0..20 {
            let data: Vec<u8> = (0..n).map(|i| (i * 37 + 11) as u8).collect();
            assert_eq!(base64_decode(&base64_encode(&data)).unwrap(), data);
        }
        assert_eq!(base64_encode(b"Man"), "TWFu");
        assert_eq!(base64_encode(b"Ma"), "TWE=");
        assert!(base64_decode("@@@").is_none());
    }

    #[test]
    fn atlas_theme_recolours_accents_and_keeps_contrast() {
        let mut tag = builtin_templates().into_iter().find(|t| t.id == "builtin:yt-timeline-tag").unwrap();
        apply_theme(&mut tag, "atlas");
        let col = |t: &GraphicsTemplate, name: &str, key: &str| {
            t.layers
                .iter()
                .find(|e| matches!(e.params.get("name").map(|p| &p.value), Some(ParamValue::Text(n)) if n == name))
                .and_then(|e| match e.params.get(key).map(|p| &p.value) {
                    Some(ParamValue::Color(c)) => Some(*c),
                    _ => None,
                })
                .unwrap()
        };
        let boxc = col(&tag, "Kutu", "fill_color");
        assert!(boxc[0] > 0.9 && boxc[1] > 0.6 && boxc[2] < 0.3, "red box → amber: {boxc:?}");
        let ink = col(&tag, "Tarih", "fill_color");
        assert!(ink[0] < 0.1, "text on amber turns dark: {ink:?}");
        let mut before = tag.clone();
        apply_theme(&mut before, "nope");
        assert_eq!(before, tag, "unknown theme is a no-op");
    }

    #[test]
    fn builtins_are_valid_and_round_trip() {
        let all = builtin_templates();
        assert!(all.len() >= 8);
        let mut ids: Vec<&str> = all.iter().map(|t| t.id.as_str()).collect();
        ids.dedup();
        assert_eq!(ids.len(), all.len());
        for cat in ["Lower Thirds", "Titles", "End Credits", "Callouts"] {
            assert!(all.iter().any(|t| t.category == cat), "{cat}");
        }
        for t in all {
            t.validate().unwrap_or_else(|e| panic!("{}: {e}", t.name));
            let back = GraphicsTemplate::from_bytes(t.to_json().as_bytes()).unwrap();
            assert_eq!(back, t);
        }
    }

    #[test]
    fn refuses_foreign_and_newer_files() {
        assert_eq!(GraphicsTemplate::from_bytes(b"PK\x03\x04rest"), Err(TemplateError::ForeignPackage));
        assert!(matches!(GraphicsTemplate::from_bytes(b"{\"format\":\"x\"}"), Err(TemplateError::NotTemplate(_))));
        let mut t = builtin_templates().remove(0);
        t.version = 99;
        assert_eq!(GraphicsTemplate::from_bytes(t.to_json().as_bytes()), Err(TemplateError::TooNew(99)));
        let mut t = builtin_templates().remove(0);
        t.controls[0].layer = 999;
        assert!(matches!(t.validate(), Err(TemplateError::Invalid(_))));
    }

    #[test]
    fn instantiates_on_other_frame_sizes() {
        let t = builtin_templates().into_iter().find(|t| t.id == "builtin:title-centered").unwrap();
        let (same, meta) = t.instantiate((1920, 1080));
        assert_eq!(same, t.layers);
        assert_eq!(meta.template.as_ref().unwrap().id, t.id);
        let (half, _) = t.instantiate((960, 540));
        assert_eq!(half[0].params["position"].value, ParamValue::Vec2(Vec2::new(480.0, 260.0)));
        assert_eq!(half[0].params["scale"].value, ParamValue::Float(50.0));
        // a vertical frame: fit to width, centred vertically
        let (tall, _) = t.instantiate((1080, 1920));
        let ParamValue::Vec2(p) = tall[0].params["position"].value else { panic!() };
        assert!((p.x - 540.0).abs() < 1e-9 && (p.y - (520.0 * 0.5625 + (1920.0 - 1080.0 * 0.5625) / 2.0)).abs() < 1e-9);
        assert_eq!(slug("My Lower Third!"), "my-lower-third");
        assert_eq!(ControlKind::for_param("background_opacity"), ControlKind::Slider);
        assert_eq!(ControlKind::for_param("shadow"), ControlKind::Checkbox);
    }
}
