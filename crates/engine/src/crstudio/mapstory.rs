//! Map choreography from the narration: which places a scene talks about, in what order and
//! when. The Blender map shot uses these "beats" so the camera arrives at a place just as the
//! narrator names it and travels between places in the order the story moves.
//!
//! Places come from every map in the project (a small gazetteer), so a scene that says "the
//! bones taken from Ephesus were carried to Marseille" can start at Ephesus even though its own
//! map only pinned Marseille.

use serde::{Deserialize, Serialize};

use super::{MapSpec, SceneSpec};

/// One place the narration names, with the moment (seconds into the scene) it is named.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Beat {
    /// Marker name exactly as the terrain meta lists it.
    pub name: String,
    pub t: f64,
    /// The story's starting point, shown before the first named place (not named itself).
    #[serde(default)]
    pub origin: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Place {
    pub name: String,
    pub lat: f64,
    pub lon: f64,
}

/// Lowercase with Turkish casing and without circumflexes, for matching.
pub fn fold(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            'İ' => out.push('i'),
            'I' => out.push('ı'),
            'â' | 'Â' => out.push('a'),
            'î' | 'Î' => out.push('i'),
            'û' | 'Û' => out.push('u'),
            _ => out.extend(c.to_lowercase()),
        }
    }
    out
}

/// Words that name a kind of place, not a place (a part made only of these is no alias).
const GENERIC: &[&str] = &["mağara", "mağarası", "cami", "camii", "ve", "dağı", "dağ", "yakını", "kalıntısı", "antik", "kenti", "şehri", "bölgesi"];

/// The phrases a place may be called by: each comma / slash / bracket part of its name
/// ("Afşin (Arabissos/Efsûs), Kahramanmaraş" → afşin, arabissos, efsus, kahramanmaraş).
pub fn aliases(name: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for part in fold(name).split([',', '/', '(', ')', ';']) {
        let p = part.split_whitespace().collect::<Vec<_>>().join(" ");
        if p.chars().count() < 3 || p.split(' ').all(|w| GENERIC.contains(&w)) {
            continue;
        }
        if !out.contains(&p) {
            out.push(p);
        }
    }
    out
}

/// Every place pinned on any map in the project (first occurrence wins).
pub fn gazetteer(scenes: &[SceneSpec]) -> Vec<Place> {
    let mut out: Vec<Place> = Vec::new();
    let mut add = |name: &str, lat: f64, lon: f64| {
        if name.trim().is_empty() || !lat.is_finite() || !lon.is_finite() {
            return;
        }
        let key = fold(name);
        if !out.iter().any(|p| fold(&p.name) == key) {
            out.push(Place { name: name.trim().to_string(), lat, lon });
        }
    };
    for sc in scenes {
        if let Some(m) = &sc.map_spec {
            add(&m.location_name, m.lat, m.lon);
            for (n, la, lo) in &m.extra_pins {
                add(n, *la, *lo);
            }
        }
    }
    out
}

/// Char position of the first time `alias` is said as a word (Turkish suffixes after an
/// apostrophe or directly attached are allowed: "Efes'te", "Tarsus'ta", "Şam'da").
fn find_word(text: &[char], alias: &[char]) -> Option<usize> {
    if alias.is_empty() || alias.len() > text.len() {
        return None;
    }
    let letter = |c: char| c.is_alphanumeric();
    (0..=text.len() - alias.len()).find(|&i| {
        text.get(i..i + alias.len()) == Some(alias)
            && (i == 0 || text.get(i - 1).is_none_or(|c| !letter(*c)))
            && match text.get(i + alias.len()) {
                None => true,
                Some(c) if !letter(*c) => true,
                // short Turkish case endings glued on ("Efesli" is not a place mention, "Efes'e" is)
                Some(_) => {
                    let tail: String = text[i + alias.len()..].iter().take_while(|c| letter(**c)).collect();
                    matches!(tail.as_str(), "a" | "e" | "da" | "de" | "ta" | "te" | "dan" | "den" | "tan" | "ten" | "ı" | "i" | "u" | "ü")
                }
            }
    })
}

/// Places named in `narration`, in the order they are first said, with their char position.
pub fn mentions(narration: &str, places: &[Place]) -> Vec<(usize, usize)> {
    let text: Vec<char> = fold(narration).chars().collect();
    let mut hits: Vec<(usize, usize)> = Vec::new();
    for (pi, p) in places.iter().enumerate() {
        let best = aliases(&p.name).iter().filter_map(|a| find_word(&text, &a.chars().collect::<Vec<_>>())).min();
        if let Some(pos) = best {
            hits.push((pi, pos));
        }
    }
    hits.sort_by_key(|h| h.1);
    hits
}

/// The scene's map, reframed for its story, and the beats. `home` is the story's anchor (the first
/// map place of the video): when the narration names its first place only after a while, the shot
/// starts there, so "the tradition carried it elsewhere: Tarsus…" travels from Ephesus to Tarsus.
pub fn plan(scenes: &[SceneSpec], idx: usize, spec: &MapSpec) -> (MapSpec, Vec<Beat>) {
    let Some(sc) = scenes.get(idx) else { return (spec.clone(), Vec::new()) };
    let places = gazetteer(scenes);
    let text = sc.narration_text.trim();
    let total_chars = text.chars().count().max(1) as f64;
    let speech = sc.voiceover_duration_sec.filter(|d| d.is_finite() && *d > 0.5).unwrap_or_else(|| sc.estimated_seconds()).clamp(1.0, 300.0);
    // with the voice's own timing (TTS alignment) the beat lands exactly when the name is said;
    // otherwise it is estimated from the position in the text
    let timed = !sc.voice_char_times.is_empty() && sc.voice_spoken.chars().count() == sc.voice_char_times.len();
    let at = |pos: usize| {
        let t = if timed { sc.voice_char_times.get(pos).map(|t| f64::from(*t)).unwrap_or(0.0) } else { pos as f64 / total_chars * speech };
        (t * 100.0).round() / 100.0
    };
    let hits = if timed {
        // the voice said the respelled names (pronunciation dictionary: "Tours" → "Tur")
        let lex = super::voice::load_lexicon();
        let spoken_places: Vec<Place> = places.iter().map(|p| Place { name: super::turkish::apply_lexicon(&p.name, &lex), ..p.clone() }).collect();
        mentions(&sc.voice_spoken, &spoken_places)
    } else {
        mentions(text, &places)
    };
    let mut out = spec.clone();
    let main = fold(&spec.location_name);
    // beats name markers exactly as the map will list them (the scene's own place keeps its name)
    let marker_name = |p: &Place| if fold(&p.name) == main { spec.location_name.clone() } else { p.name.clone() };
    let mut beats: Vec<Beat> = Vec::new();
    let mut stops: Vec<&Place> = Vec::new();
    for (pi, pos) in &hits {
        if let Some(p) = places.get(*pi) {
            stops.push(p);
            beats.push(Beat { name: marker_name(p), t: at(*pos), origin: false });
        }
    }
    let home = scenes.iter().find_map(|s| s.map_spec.as_ref()).map(|m| Place { name: m.location_name.clone(), lat: m.lat, lon: m.lon });
    if let (Some(first), Some(home)) = (beats.first(), home.as_ref())
        && first.t > 1.6
        && !stops.iter().any(|p| fold(&p.name) == fold(&home.name))
        && scenes.iter().position(|s| s.map_spec.is_some()) != Some(idx)
    {
        beats.insert(0, Beat { name: marker_name(home), t: 0.0, origin: true });
        stops.insert(0, home);
    }
    if beats.is_empty() {
        // nothing named: the scene's own place
        beats.push(Beat { name: spec.location_name.clone(), t: 0.0, origin: false });
    }
    // the map shows the story's places only, framed close when there is just one
    out.extra_pins = stops.iter().filter(|p| fold(&p.name) != main).map(|p| (p.name.clone(), p.lat, p.lon)).collect();
    out.zoom = if out.extra_pins.is_empty() { spec.zoom.max(6.0) } else { spec.zoom.max(5.0) };
    out.from_lat = None;
    out.from_lon = None;
    (out, beats)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scene(text: &str, vo: f64, map: Option<MapSpec>) -> SceneSpec {
        SceneSpec { narration_text: text.into(), voiceover_duration_sec: Some(vo), map_spec: map, ..Default::default() }
    }

    fn map(name: &str, lat: f64, lon: f64, pins: &[(&str, f64, f64)]) -> MapSpec {
        MapSpec {
            location_name: name.into(),
            lat,
            lon,
            zoom: 2.5,
            extra_pins: pins.iter().map(|(n, a, b)| ((*n).to_string(), *a, *b)).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn spoken_timing_wins_over_the_estimate() {
        let tarsus = map("ASHÂB-I KEHF MAĞARASI VE CAMİİ, TARSUS, MERSİN", 36.92, 34.89, &[]);
        let mut sc = scene("Tarsus'ta bir mağara vardır.", 9.0, Some(tarsus.clone()));
        sc.voice_spoken = "[slowly] Tarsus. [pause] Bir mağara.".into();
        sc.voice_char_times = (0..sc.voice_spoken.chars().count()).map(|i| i as f32 * 0.1).collect();
        let (_, b) = plan(&[sc], 0, &tarsus);
        assert_eq!(b.len(), 1);
        assert!((b[0].t - 0.9).abs() < 1e-6, "Tarsus starts at char 9 → 0.9 s, got {}", b[0].t);
    }

    #[test]
    fn aliases_and_word_matching() {
        assert_eq!(aliases("Afşin (Arabissos/Efsûs), Kahramanmaraş"), vec!["afşin", "arabissos", "efsus", "kahramanmaraş"]);
        assert_eq!(aliases("ASHÂB-I KEHF MAĞARASI VE CAMİİ, TARSUS, MERSİN"), vec!["ashab-ı kehf mağarası ve camii", "tarsus", "mersin"]);
        let t: Vec<char> = fold("Efes'ten alınan kemikler; Efesli değil").chars().collect();
        assert_eq!(find_word(&t, &['e', 'f', 'e', 's']), Some(0));
        let t2: Vec<char> = fold("Efesli bir taş").chars().collect();
        assert_eq!(find_word(&t2, &['e', 'f', 'e', 's']), None);
        let t3: Vec<char> = fold("Şampiyon değil, Şam'da").chars().collect();
        assert_eq!(find_word(&t3, &['ş', 'a', 'm']), Some(16));
    }

    #[test]
    fn the_story_moves_from_ephesus_to_tarsus() {
        let efes =
            map("YEDİ UYURLAR MAĞARASI, PANAYIRDAĞI, EFES/SELÇUK, İZMİR", 37.94, 27.34, &[("Ashâb-ı Kehf Mağarası ve Camii, Tarsus, Mersin", 36.92, 34.89)]);
        let tarsus = map("ASHÂB-I KEHF MAĞARASI VE CAMİİ, TARSUS, MERSİN", 36.92, 34.89, &[("Afşin (Arabissos/Efsûs), Kahramanmaraş", 38.25, 36.92)]);
        let scenes = vec![
            scene("Efes'te Panayırdağı yamacındaki mağara.", 10.0, Some(efes)),
            scene("Müslüman gelenek olayı başka yerlere de taşıdı. Tarsus'ta Ashâb-ı Kehf adıyla anılan bir mağara vardır.", 7.5, Some(tarsus.clone())),
            scene("Anadolu'da Efsûs adıyla Arabissos, yani Afşin gösterilir. Tarsus da anılır.", 7.0, Some(tarsus.clone())),
        ];
        // scene 0: one place, framed close
        let (m0, b0) = plan(&scenes, 0, scenes[0].map_spec.as_ref().unwrap());
        assert_eq!(b0.len(), 1);
        assert!(m0.extra_pins.is_empty() && m0.zoom >= 6.0);
        // scene 1: Tarsus is named late → start at Ephesus (origin) and travel
        let (m1, b1) = plan(&scenes, 1, &tarsus);
        assert_eq!(b1.len(), 2);
        assert!(b1[0].origin && b1[0].name.contains("EFES"));
        assert!(b1[1].name.contains("TARSUS") && b1[1].t > 2.0 && b1[1].t < 4.0);
        assert_eq!(b1[1].name, tarsus.location_name, "the beat names the marker exactly");
        assert_eq!(m1.extra_pins.len(), 1, "only the story's places are pinned");
        // scene 2: Afşin first (named by its old name), then Tarsus
        let (_, b2) = plan(&scenes, 2, &tarsus);
        assert_eq!(b2.len(), 2);
        assert!(b2[0].name.starts_with("Afşin") && b2[0].t < 1.5 && !b2[0].origin);
        assert!(b2[1].name.contains("TARSUS"));
    }
}
