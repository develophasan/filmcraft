//! Narration recording clean-up and the voice-clone library.
//!
//! [`clean`] turns a raw microphone take into a narration-ready one, conservatively (heavy
//! processing sounds artificial, and a voice clone would learn the artefacts):
//! 1. 80 Hz high-pass (rumble, desk thumps, mains hum's lowest part)
//! 2. a soft downward expander: everything well below the speech level (room noise, breaths,
//!    mouth noise between phrases) is lowered by up to 12 dB, with a hang time so word endings
//!    and soft consonants are never cut
//! 3. pauses longer than 0.6 s shortened to 0.45 s, leading / trailing silence to 0.25 s
//! 4. level: speech at about −20 dBFS RMS, true peaks at or below −3 dBFS
//!
//! It also reports what it found (clipping, too quiet, noisy room) so the narrator can re-take.
//!
//! Every cleaned take is copied into the clone library (`~/.crstudio/klon`), which exports
//! ≤10-minute FLAC parts ready for an ElevenLabs Professional Voice Clone.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{persist, wav};

const FRAME_MS: usize = 10;
const MAX_PAUSE: f64 = 0.6;
const KEEP_PAUSE: f64 = 0.45;
const EDGE_SILENCE: f64 = 0.25;
const TARGET_RMS_DB: f64 = -20.0;
const PEAK_CEILING_DB: f64 = -3.0;
const QUIET_CUT_DB: f64 = 12.0;
/// Clone-library export: part length (seconds).
const PART_SECONDS: f64 = 600.0;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CleanReport {
    pub raw_seconds: f64,
    pub clean_seconds: f64,
    /// Speech level of the raw take (90th percentile of 10 ms frames, dBFS).
    pub raw_speech_db: f64,
    /// Room-noise floor of the raw take (10th percentile, dBFS).
    pub floor_db: f64,
    pub raw_peak_db: f64,
    /// After clean-up.
    pub speech_rms_db: f64,
    pub peak_db: f64,
    pub pauses_shortened: usize,
    pub clipped_samples: usize,
    /// `iyi` / `orta` / `zayıf`.
    pub grade: String,
    pub warnings: Vec<String>,
}

fn db(x: f64) -> f64 {
    if x > 1e-9 { 20.0 * x.log10() } else { -180.0 }
}

fn from_db(d: f64) -> f64 {
    10f64.powf(d / 20.0)
}

/// RBJ 2nd-order high-pass (Butterworth Q).
fn highpass(x: &[f32], rate: u32, fc: f64) -> Vec<f32> {
    let w0 = 2.0 * std::f64::consts::PI * fc / f64::from(rate.max(8000));
    let (s, c) = w0.sin_cos();
    let alpha = s / (2.0 * std::f64::consts::FRAC_1_SQRT_2);
    let a0 = 1.0 + alpha;
    let b0 = (1.0 + c) / 2.0 / a0;
    let b1 = -(1.0 + c) / a0;
    let b2 = b0;
    let a1 = -2.0 * c / a0;
    let a2 = (1.0 - alpha) / a0;
    let (mut x1, mut x2, mut y1, mut y2) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
    x.iter()
        .map(|&v| {
            let v = f64::from(v);
            let y = b0 * v + b1 * x1 + b2 * x2 - a1 * y1 - a2 * y2;
            x2 = x1;
            x1 = v;
            y2 = y1;
            y1 = y;
            y as f32
        })
        .collect()
}

fn frame_db(x: &[f32], frame: usize) -> Vec<f64> {
    x.chunks(frame.max(1))
        .map(|c| {
            let e: f64 = c.iter().map(|v| f64::from(*v) * f64::from(*v)).sum::<f64>() / c.len().max(1) as f64;
            db(e.sqrt())
        })
        .collect()
}

fn percentile(v: &[f64], p: f64) -> f64 {
    if v.is_empty() {
        return -180.0;
    }
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    let i = ((s.len() - 1) as f64 * p.clamp(0.0, 1.0)).round() as usize;
    s.get(i).copied().unwrap_or(-180.0)
}

/// Clean a mono take. Never fails: an empty or silent take comes back as it was, with warnings.
pub fn clean(raw: &[f32], rate: u32) -> (Vec<f32>, CleanReport) {
    let rate = rate.max(8000);
    let raw: Vec<f32> = raw.iter().map(|v| if v.is_finite() { *v } else { 0.0 }).collect();
    let raw = raw.as_slice();
    let mut r = CleanReport { raw_seconds: raw.len() as f64 / f64::from(rate), ..Default::default() };
    let frame = (rate as usize * FRAME_MS / 1000).max(1);
    r.clipped_samples = raw.iter().filter(|v| v.abs() >= 0.999).count();
    r.raw_peak_db = db(raw.iter().fold(0.0f32, |m, v| m.max(v.abs())).into());
    if raw.len() < frame * 20 {
        r.warnings.push("Kayıt çok kısa.".into());
        r.grade = "zayıf".into();
        r.clean_seconds = r.raw_seconds;
        return (raw.to_vec(), r);
    }

    // 1. high-pass
    let x = highpass(raw, rate, 80.0);
    let fdb = frame_db(&x, frame);
    r.raw_speech_db = percentile(&fdb, 0.90);
    r.floor_db = percentile(&fdb, 0.10);

    // 2. speech / quiet frames with hang time (60 ms before, 180 ms after speech)
    let gate = (r.raw_speech_db - 24.0).max(r.floor_db + 8.0);
    let n = fdb.len();
    let mut speech = vec![false; n];
    for (i, d) in fdb.iter().enumerate() {
        if *d >= gate {
            let a = i.saturating_sub(6);
            let b = (i + 18).min(n.saturating_sub(1));
            for s in speech.iter_mut().take(b + 1).skip(a) {
                *s = true;
            }
        }
    }
    // smoothed per-frame gain (attack 10 ms, release 60 ms), interpolated per sample
    let quiet_gain = from_db(-QUIET_CUT_DB);
    let mut g = Vec::with_capacity(n);
    let mut cur = 1.0f64;
    for s in &speech {
        let target = if *s { 1.0 } else { quiet_gain };
        let k = if target > cur { 0.7 } else { 0.18 };
        cur += (target - cur) * k;
        g.push(cur);
    }
    let mut y: Vec<f32> = x
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let fi = i / frame;
            let frac = (i % frame) as f64 / frame as f64;
            let g0 = g.get(fi).copied().unwrap_or(1.0);
            let g1 = g.get(fi + 1).copied().unwrap_or(g0);
            (f64::from(*v) * (g0 + (g1 - g0) * frac)) as f32
        })
        .collect();

    // 3. shorten long pauses (runs of non-speech frames), trim the ends
    let max_pause = (MAX_PAUSE * 1000.0) as usize / FRAME_MS;
    let keep_half = (KEEP_PAUSE * 1000.0) as usize / FRAME_MS / 2;
    let edge = (EDGE_SILENCE * 1000.0) as usize / FRAME_MS;
    let mut keep = vec![true; n];
    let mut i = 0usize;
    while i < n {
        if speech.get(i).copied().unwrap_or(true) {
            i += 1;
            continue;
        }
        let start = i;
        while i < n && !speech.get(i).copied().unwrap_or(true) {
            i += 1;
        }
        let len = i - start;
        let (lead, trail) = (start == 0, i == n);
        if lead || trail {
            if len > edge {
                let (a, b) = if lead { (start, i - edge) } else { (start + edge, i) };
                for k in keep.iter_mut().take(b).skip(a) {
                    *k = false;
                }
            }
        } else if len > max_pause {
            for k in keep.iter_mut().take(i - keep_half).skip(start + keep_half) {
                *k = false;
            }
            r.pauses_shortened += 1;
        }
    }
    if keep.iter().any(|k| !k) {
        let mut out: Vec<f32> = Vec::with_capacity(y.len());
        let fade = frame; // 10 ms crossfade at every joint
        let mut prev_kept = true;
        for (fi, chunk) in y.chunks(frame).enumerate() {
            let k = keep.get(fi).copied().unwrap_or(true);
            if k {
                if !prev_kept && !out.is_empty() {
                    // crossfade the joint
                    let m = fade.min(chunk.len()).min(out.len());
                    let base = out.len() - m;
                    for j in 0..m {
                        let w = (j as f32 + 0.5) / m as f32;
                        if let (Some(o), Some(c)) = (out.get_mut(base + j), chunk.get(j)) {
                            *o = *o * (1.0 - w) + *c * w;
                        }
                    }
                    out.extend(chunk.iter().skip(m));
                } else {
                    out.extend_from_slice(chunk);
                }
            }
            prev_kept = k;
        }
        y = out;
    }

    // 4. level: speech RMS to −20 dBFS, peaks ≤ −3 dBFS
    let speech_e: Vec<f64> = frame_db(&y, frame).into_iter().filter(|d| *d >= gate - 6.0).collect();
    let speech_rms = if speech_e.is_empty() { -180.0 } else { db((speech_e.iter().map(|d| from_db(*d).powi(2)).sum::<f64>() / speech_e.len() as f64).sqrt()) };
    let peak = y.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    if speech_rms > -150.0 && peak > 0.0 {
        let gain = from_db(TARGET_RMS_DB - speech_rms).min(from_db(PEAK_CEILING_DB) / f64::from(peak)).min(from_db(30.0));
        for v in &mut y {
            *v = (f64::from(*v) * gain) as f32;
        }
        r.speech_rms_db = speech_rms + db(gain);
    } else {
        r.speech_rms_db = speech_rms;
    }
    r.peak_db = db(y.iter().fold(0.0f32, |m, v| m.max(v.abs())).into());
    r.clean_seconds = y.len() as f64 / f64::from(rate);

    // findings
    if r.clipped_samples > 3 {
        r.warnings.push("Kayıt kırpılmış (çok yüksek): ses kartı / mikrofon kazancını düşürün.".into());
    }
    if r.raw_speech_db < -38.0 {
        r.warnings.push(format!("Kayıt kısık (konuşma {:.0} dB): kazancı artırın ya da mikrofona yaklaşın.", r.raw_speech_db));
    }
    if r.floor_db > -55.0 {
        r.warnings.push(format!("Ortam gürültülü (taban {:.0} dB): fan/klimayı kapatın, odayı yumuşak eşyalarla sönümleyin.", r.floor_db));
    }
    if r.raw_speech_db - r.floor_db < 30.0 && r.raw_speech_db > -150.0 {
        r.warnings.push(format!("Konuşma ile gürültü arası yalnızca {:.0} dB (en az 40 dB iyi olur).", r.raw_speech_db - r.floor_db));
    }
    if r.speech_rms_db < TARGET_RMS_DB - 4.0 && r.speech_rms_db > -150.0 {
        r.warnings.push("Ani yüksek sesler var; mikrofona sabit mesafede konuşun.".into());
    }
    r.grade = match r.warnings.len() {
        0 => "iyi",
        1 => "orta",
        _ => "zayıf",
    }
    .into();
    if r.clipped_samples > 3 {
        r.grade = "zayıf".into();
    }
    (y, r)
}

/// One-line Turkish summary of a report for the UI.
pub fn summary(r: &CleanReport) -> String {
    let mut s = format!("{} · {:.1} sn → {:.1} sn · konuşma {:.0} dB · taban {:.0} dB", r.grade, r.raw_seconds, r.clean_seconds, r.raw_speech_db, r.floor_db);
    if r.pauses_shortened > 0 {
        s.push_str(&format!(" · {} uzun bekleme kısaltıldı", r.pauses_shortened));
    }
    s
}

// ------------------------------------------------------------------------------------------------
// Clone library

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ClonePiece {
    pub path: String,
    pub seconds: f64,
    pub text: String,
    /// `sahne` (a scene take) or `okuma` (a clone reading).
    pub source: String,
    pub grade: String,
    pub created: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CloneLibrary {
    pub pieces: Vec<ClonePiece>,
    /// Next clone-reading paragraph.
    pub cursor: usize,
    /// Narrator's own reading text (paragraphs separated by blank lines).
    pub custom_text: String,
}

pub fn clone_dir() -> PathBuf {
    persist::config_dir().join("klon")
}

fn library_path() -> PathBuf {
    clone_dir().join("kutuphane.json")
}

pub fn load_library() -> CloneLibrary {
    let mut l: CloneLibrary = std::fs::read(library_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    l.pieces.retain(|p| Path::new(&p.path).is_file());
    l.pieces.truncate(5000);
    l
}

pub fn save_library(l: &CloneLibrary) -> Result<(), String> {
    let p = library_path();
    std::fs::create_dir_all(clone_dir()).map_err(|e| e.to_string())?;
    filmcraft_format::atomic_write(&p, &serde_json::to_vec_pretty(l).map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}

pub fn total_seconds(l: &CloneLibrary) -> f64 {
    l.pieces.iter().filter(|p| p.grade != "zayıf").map(|p| p.seconds).sum()
}

/// Copy a cleaned take into the library (weak takes are kept but not exported).
pub fn add_piece(samples: &[f32], rate: u32, text: &str, source: &str, report: &CleanReport) -> Result<ClonePiece, String> {
    if cfg!(test) {
        return Err("testlerde kütüphaneye yazılmaz".into());
    }
    std::fs::create_dir_all(clone_dir()).map_err(|e| e.to_string())?;
    let stamp = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
    let path = clone_dir().join(format!("parca_{stamp}.wav"));
    std::fs::write(&path, wav::encode(samples, rate)).map_err(|e| e.to_string())?;
    let piece = ClonePiece {
        path: path.to_string_lossy().into_owned(),
        seconds: samples.len() as f64 / f64::from(rate.max(8000)),
        text: text.chars().take(2000).collect(),
        source: source.into(),
        grade: report.grade.clone(),
        created: persist::today(),
    };
    let mut l = load_library();
    l.pieces.push(piece.clone());
    save_library(&l)?;
    Ok(piece)
}

/// Encode mono samples as a 16-bit FLAC file.
pub fn flac_bytes(samples: &[f32], rate: u32) -> Result<Vec<u8>, String> {
    let mut enc = filmcraft_flac::Encoder::new(filmcraft_flac::EncoderConfig::new(rate, 1, 16)).map_err(|e| e.to_string())?;
    let ints: Vec<i32> = samples.iter().map(|v| (f64::from(*v).clamp(-1.0, 1.0) * 32767.0).round() as i32).collect();
    let mut file = enc.header();
    file.extend(enc.encode(&[&ints]).map_err(|e| e.to_string())?);
    file.extend(enc.finish());
    let at = filmcraft_flac::STREAMINFO_OFFSET as usize;
    if let Some(slot) = file.get_mut(at..at + 34) {
        slot.copy_from_slice(&enc.streaminfo());
    }
    Ok(file)
}

/// Join the library's good takes (0.4 s apart) into ≤10-minute FLAC parts in
/// `~/.crstudio/klon/elevenlabs/`. Returns the files written.
pub fn export_parts() -> Result<Vec<PathBuf>, String> {
    let l = load_library();
    let out_dir = clone_dir().join("elevenlabs");
    std::fs::create_dir_all(&out_dir).map_err(|e| e.to_string())?;
    // old parts go, so the folder always matches the library
    if let Ok(rd) = std::fs::read_dir(&out_dir) {
        for e in rd.flatten() {
            if e.path().extension().is_some_and(|x| x == "flac") {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    let mut parts = Vec::new();
    let mut cur: Vec<f32> = Vec::new();
    let mut rate = 0u32;
    let flush = |cur: &mut Vec<f32>, rate: u32, parts: &mut Vec<PathBuf>| -> Result<(), String> {
        if cur.is_empty() || rate == 0 {
            return Ok(());
        }
        let p = out_dir.join(format!("klon_parca_{:02}.flac", parts.len() + 1));
        std::fs::write(&p, flac_bytes(cur, rate)?).map_err(|e| e.to_string())?;
        parts.push(p);
        cur.clear();
        Ok(())
    };
    for piece in l.pieces.iter().filter(|p| p.grade != "zayıf") {
        let Ok(wav::Mono { samples, rate: r }) = wav::read(Path::new(&piece.path)) else { continue };
        if rate != 0 && r != rate {
            flush(&mut cur, rate, &mut parts)?;
        }
        rate = r;
        if cur.len() as f64 / f64::from(rate.max(1)) + samples.len() as f64 / f64::from(rate.max(1)) > PART_SECONDS {
            flush(&mut cur, rate, &mut parts)?;
        }
        if !cur.is_empty() {
            cur.extend(std::iter::repeat_n(0.0, (rate as usize) * 2 / 5));
        }
        cur.extend(samples);
        if parts.len() >= 60 {
            break;
        }
    }
    flush(&mut cur, rate, &mut parts)?;
    if parts.is_empty() {
        return Err("Kütüphanede dışa aktarılacak (iyi/orta kalitede) kayıt yok".into());
    }
    Ok(parts)
}

/// Clone reading material: varied Turkish documentary prose (original text), so the clone hears
/// every sound of the language in the narrator's documentary delivery.
pub const READING: &[&str] = &[
    "Anadolu'nun en eski hikâyeleri, taşa kazınmış işaretlerle başlar. Göbeklitepe'nin dikilitaşlarına bakan biri, on iki bin yıl önce burada toplanan insanların ne düşündüğünü merak etmeden duramaz. Tilkiler, yaban domuzları, akbabalar... Her biri bir mesaj mı, yoksa yalnızca bir süs mü? Bu soruya bugün hâlâ kesin bir cevap veremiyoruz.",
    "Rivayete göre Efes yakınlarındaki bir mağarada yedi genç uykuya daldı. Uyandıklarında ceplerindeki gümüş paralar artık geçmiyordu, şehrin sokakları tanınmaz hâle gelmişti. Peki bu hikâye gerçekten Efes'te mi geçti, yoksa Tarsus'ta mı? Kaynaklar burada ayrılıyor ve biz de her iki iddiayı dikkatle tartacağız.",
    "Kaçkar Dağları'nın eteklerinde, sisin vadiye çöktüğü sabahlarda, köylüler eski bir masalı fısıldar. Derler ki yaylaya giden yolda yaşlı bir çoban, kaybolan yolculara ışık tutarmış. Kimse onun yüzünü görmemiş; yalnızca fenerinin titrek ışığını ve uzaktan gelen kaval sesini duymuşlar.",
    "Şimdi haritaya bakalım. Kuzeyde Karadeniz, batıda Ege, güneyde Akdeniz... Bu üç denizin arasında kalan topraklar, binlerce yıl boyunca göçlerin, savaşların ve ticaret kervanlarının geçtiği bir köprü oldu. Hititler, Frigler, Lidyalılar ve daha nicesi burada iz bıraktı.",
    "Belgelerle rivayeti birbirinden ayırmak zorundayız. Tarihçi Herodot, Lidya kralı Krezüs'ün servetini anlatırken abartılı hikâyeleri de kayda geçirir. Bugün arkeologlar Sardes'te bulunan altın atölyelerinin kalıntılarını inceliyor ve bazı anlatıların şaşırtıcı biçimde doğru çıktığını görüyor.",
    "Gece yarısı, köyün meydanındaki çınar ağacının altında, büyükanneler çocuklara Şahmeran'ı anlatırdı. Yarısı kadın, yarısı yılan olan bu bilge kraliçe, sırrını kimseye vermezdi. Ama bir gün güvendiği bir insan onu ele verdi ve hikâye, ihanetin bedelini anlatan acı bir derse dönüştü.",
    "Bu kadar eski bir efsanenin bugüne nasıl ulaştığını hiç düşündünüz mü? Ağızdan ağıza, kuşaktan kuşağa aktarılan her hikâye biraz değişir. Bir isim unutulur, bir yer adı başkalaşır, bir kahramanın yaşı uzar da uzar. İşte biz bu değişimin izini sürmeye çalışıyoruz.",
    "Üç yüz dokuz yıl. Kulağa inanılmaz geliyor, değil mi? Bazı yorumcular bu rakamı güneş ve ay takvimleri arasındaki farkla açıklıyor: üç yüz güneş yılı, yaklaşık üç yüz dokuz ay yılına denk geliyor. Bu hesap, metindeki sayının rastgele seçilmediğini düşündürüyor.",
    "Nemrut Dağı'nın zirvesinde, gün doğarken dev heykellerin başları yavaş yavaş aydınlanır. Kommagene Kralı Antiokhos, kendisini tanrılarla yan yana koydurmuştu. Yüzyıllar içinde depremler başları gövdelerden ayırdı; bugün onlar, dağın eteklerine bakan sessiz nöbetçiler gibi duruyor.",
    "Kapadokya'da yeraltı şehirlerine indiğinizde hava birden serinler. Dar koridorlar, taş kapılar, havalandırma bacaları... Burada yüzlerce, belki binlerce insan aylarca saklanabiliyordu. Kimden kaçıyorlardı ve bu karanlık tünelleri nasıl bu kadar ustaca kazdılar?",
    "Tarihî gerçek ile halk anlatısı çoğu zaman iç içe geçer. Dede Korkut hikâyelerinde Oğuz beylerinin maceraları anlatılırken, aynı satırlarda dağlara, sulara ve atlara seslenen şiirsel bir dil karşımıza çıkar. Bu dil, anlatıcının kendisini de hikâyenin bir parçası hâline getirir.",
    "Ve şimdi en zor soruya geliyoruz. Bu hikâyelerin hepsi bir yerde birleşiyor mu? Farklı kültürlerde, farklı dillerde aynı motifin karşımıza çıkması bir tesadüf olabilir mi? Siz ne düşünüyorsunuz? Yorumlarda bize yazın; bir sonraki bölümde en ilginç cevapları birlikte değerlendireceğiz.",
    "Truva'nın surları, yüzyıllarca yalnızca Homeros'un dizelerinde yaşadı. Ta ki on dokuzuncu yüzyılda Heinrich Schliemann, Çanakkale yakınlarındaki Hisarlık Tepesi'nde kazmaya başlayana kadar. Bulduğu katmanlar, efsanenin ardında gerçek bir şehrin, hatta üst üste kurulmuş dokuz şehrin yattığını gösterdi.",
    "Kış gecelerinde Erzurum'da anlatılan bir hikâye vardır: Karlar diz boyu olduğunda, dağdan inen bir kurt köyün etrafında üç kez döner ve sonra kaybolurmuş. Yaşlılar bunun bir uyarı olduğunu söyler. Ertesi gün mutlaka bir haber gelir; bazen iyi, bazen de kötü.",
    "Bu bölümde kullandığımız tüm görsellerin kaynaklarını açıklamaya ekledik. Belgelenmiş bilgileri rivayetten ayırdık; emin olmadığımız yerlerde bunu açıkça söyledik. Çünkü bir efsaneyi anlatmak, onu gerçekmiş gibi sunmak demek değildir.",
    "Haydi, son bir kez Efes'e dönelim. Mermer caddelerde yürürken Celsus Kütüphanesi'nin cephesi karşınıza çıkar. Bir zamanlar on iki bin parşömen burada saklanıyordu. Okuyan, tartışan, merak eden insanlar... Belki de anlattığımız hikâyelerin bir kısmı ilk kez burada yazıya geçirildi.",
];

/// The clone-reading paragraphs: the narrator's own text (blank-line separated) first, then the
/// built-in ones.
pub fn reading_list(l: &CloneLibrary) -> Vec<String> {
    let mut v: Vec<String> =
        l.custom_text.split("\n\n").map(|p| p.split_whitespace().collect::<Vec<_>>().join(" ")).filter(|p| p.chars().count() >= 20).take(2000).collect();
    v.extend(READING.iter().map(|s| (*s).to_string()));
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Speech-like bursts with a long pause, a quiet "breath" and room noise.
    fn take(rate: u32) -> Vec<f32> {
        let mut v = Vec::new();
        let mut seed = 1u32;
        let mut noise = move || {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
            ((seed >> 16) as f32 / 32768.0 - 1.0) * 0.002
        };
        let tone = |n: usize, amp: f32, f: f32| (0..n).map(move |i| (i as f32 * f / rate as f32 * std::f32::consts::TAU).sin() * amp);
        v.extend((0..rate as usize).map(|_| noise())); // 1 s lead silence
        v.extend(tone(rate as usize, 0.3, 220.0));
        v.extend((0..(rate as usize * 2)).map(|_| noise())); // 2 s pause
        v.extend(tone(rate as usize / 3, 0.01, 900.0)); // breath-ish
        v.extend((0..(rate as usize / 2)).map(|_| noise()));
        v.extend(tone(rate as usize, 0.25, 180.0));
        v.extend((0..rate as usize).map(|_| noise()));
        v
    }

    #[test]
    fn clean_shortens_pauses_levels_speech_and_reports() {
        let rate = 48_000;
        let raw = take(rate);
        let (y, r) = clean(&raw, rate);
        assert!(r.clean_seconds < r.raw_seconds - 1.5, "pauses and edges trimmed: {r:?}");
        assert!(r.pauses_shortened >= 1);
        assert!(r.peak_db <= PEAK_CEILING_DB + 0.01);
        assert!((r.speech_rms_db - TARGET_RMS_DB).abs() < 4.0 || r.peak_db >= PEAK_CEILING_DB - 0.1, "{r:?}");
        assert!(y.iter().all(|v| v.is_finite()));
        assert_eq!(r.grade, "iyi", "{:?}", r.warnings);
    }

    #[test]
    fn hostile_takes_never_panic_and_are_flagged() {
        let (y, r) = clean(&[], 48_000);
        assert!(y.is_empty());
        assert_eq!(r.grade, "zayıf");
        let clipped: Vec<f32> = (0..48_000).map(|i| if i % 2 == 0 { 1.0 } else { -1.0 }).collect();
        let (_, r) = clean(&clipped, 0);
        assert!(r.warnings.iter().any(|w| w.contains("kırpılmış")));
        let quiet: Vec<f32> = (0..96_000).map(|i| (i as f32 * 0.03).sin() * 0.002).collect();
        let (_, r) = clean(&quiet, 48_000);
        assert!(r.warnings.iter().any(|w| w.contains("kısık")), "{r:?}");
        let nan = vec![f32::NAN; 4800];
        let _ = clean(&nan, 48_000);
    }

    #[test]
    fn flac_and_reading_list() {
        let s: Vec<f32> = (0..48_000).map(|i| (i as f32 * 0.01).sin() * 0.5).collect();
        let f = flac_bytes(&s, 48_000).unwrap();
        assert_eq!(&f[..4], b"fLaC");
        assert!(f.len() < 48_000 * 2);
        let l = CloneLibrary { custom_text: "Kısa.\n\nBu benim kendi okuma metnim, yeterince uzun bir paragraf.".into(), ..Default::default() };
        let r = reading_list(&l);
        assert_eq!(r.len(), READING.len() + 1);
        assert!(r[0].starts_with("Bu benim"));
    }
}
