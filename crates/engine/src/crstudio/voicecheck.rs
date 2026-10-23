//! Narration check: the synthesized voice is transcribed (ElevenLabs speech-to-text) and compared
//! word by word with the script, so a misread word ("Gençler" heard as "Gesar") is caught before
//! the video is cut.
//!
//! Proper names are expected to differ in spelling (the pronunciation dictionary respells them
//! and a transcriber spells foreign names its own way); they are reported, not retried. An
//! ordinary Turkish word that comes back wrong means the voice slurred or misread it: the scene is
//! synthesized again.

use super::turkish;

/// One stretch of the script that the transcript does not match.
#[derive(Clone, Debug, PartialEq)]
pub struct Miss {
    /// What the script says.
    pub expected: String,
    /// What the transcriber heard there (may be empty: dropped).
    pub heard: String,
    /// An ordinary word (not a proper name): worth a retry.
    pub common: bool,
}

impl Miss {
    pub fn label(&self) -> String {
        if self.heard.is_empty() { format!("{} → (duyulmadı)", self.expected) } else { format!("{} → {}", self.expected, self.heard) }
    }
}

const MAX_WORDS: usize = 800;

struct Tok {
    /// Sound-alike key: foreign spellings and a transcriber's Turkish spelling of the same name
    /// meet here (`Tours'lu` ~ `Turlu`, `Malchus` ~ `Malkus`).
    key: String,
    show: String,
    common: bool,
    /// Key of the word before an apostrophe (`Camasb'a` → `camasb`): where a name's suffixes go.
    stem: String,
    /// Capitalized inside a sentence: a proper name.
    name: bool,
}

/// Drop v3 audio tags (`[pause]`, `[low voice]` …).
fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut depth = 0usize;
    for c in s.chars() {
        match c {
            '[' => depth += 1,
            ']' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

fn tokens(text: &str) -> Vec<Tok> {
    let spoken = turkish::normalize_for_speech(&strip_tags(text));
    let mut out = Vec::new();
    let mut sentence_start = true;
    for raw in spoken.split_whitespace().take(MAX_WORDS) {
        let word: String = raw.trim_matches(|c: char| !c.is_alphanumeric()).to_string();
        let ends = raw.ends_with(['.', '!', '?', ':', '…']);
        if word.is_empty() {
            sentence_start |= ends;
            continue;
        }
        let key = phonetic(&turkish::ascii_fold(&turkish::lower(&word)));
        if key.is_empty() {
            continue;
        }
        let first_upper = word.chars().next().is_some_and(char::is_uppercase);
        let roman = word.chars().all(|c| matches!(c, 'I' | 'V' | 'X' | 'L' | 'C' | 'M'));
        let common = (!first_upper || sentence_start) && !roman && word.chars().any(char::is_lowercase);
        let stem_word = word.split(['\'', '’']).next().unwrap_or(&word);
        let stem = phonetic(&turkish::ascii_fold(&turkish::lower(stem_word)));
        out.push(Tok { key, show: word.clone(), common, stem, name: first_upper && !sentence_start });
        sentence_start = ends;
    }
    out
}

/// A rough sound key over ASCII-folded lower-case text: common foreign digraphs to their Turkish
/// sound, `y` as `i`, doubled letters once, no spaces.
fn phonetic(folded: &str) -> String {
    let s: String = folded.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    let s = s
        .replace("ou", "u")
        .replace("ch", "k")
        .replace("ph", "f")
        .replace("th", "t")
        .replace("qu", "k")
        .replace('x', "ks")
        .replace('w', "v")
        .replace('q', "k")
        .replace('y', "i")
        .replace('h', "");
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if out.ends_with(c) {
            continue;
        }
        out.push(c);
    }
    out
}

/// Edit distance, bounded (returns `limit + 1` once it is exceeded).
fn distance(a: &str, b: &str, limit: usize) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.len().abs_diff(b.len()) > limit {
        return limit + 1;
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            let sub = prev.get(j).copied().unwrap_or(0) + usize::from(ca != cb);
            let del = prev.get(j + 1).copied().unwrap_or(0) + 1;
            let ins = cur.get(j).copied().unwrap_or(0) + 1;
            if let Some(c) = cur.get_mut(j + 1) {
                *c = sub.min(del).min(ins);
            }
        }
        if cur.iter().min().copied().unwrap_or(0) > limit {
            return limit + 1;
        }
        prev = cur;
    }
    prev.last().copied().unwrap_or(0)
}

fn same(a: &str, b: &str) -> bool {
    let tol = a.chars().count().max(b.chars().count()) / 4;
    a == b || (tol > 0 && distance(a, b, tol) <= tol)
}

/// Compare the script (`expected`, may contain audio tags) with a transcript (`heard`).
pub fn compare(expected: &str, heard: &str) -> Vec<Miss> {
    compare_with_names(expected, heard, &std::collections::HashSet::new())
}

/// The proper names of a whole script: words written capitalized inside a sentence somewhere
/// (so "Camasb" opening a sentence is known as a name, not an ordinary word).
pub fn proper_names<'a>(texts: impl IntoIterator<Item = &'a str>) -> std::collections::HashSet<String> {
    texts.into_iter().flat_map(tokens).filter(|t| t.name && !t.stem.is_empty()).map(|t| t.stem).collect()
}

/// [`compare`], with the script's proper names: a name at a sentence start is not an ordinary word.
pub fn compare_with_names(expected: &str, heard: &str, names: &std::collections::HashSet<String>) -> Vec<Miss> {
    let mut e = tokens(expected);
    for t in &mut e {
        if names.contains(&t.stem) {
            t.common = false;
        }
    }
    let h = tokens(heard);
    let (n, m) = (e.len(), h.len());
    // longest common subsequence over the word keys
    let w = m + 1;
    let mut dp = vec![0u32; (n + 1) * w];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            let v = match (e.get(i), h.get(j)) {
                (Some(a), Some(b)) if same(&a.key, &b.key) => dp.get((i + 1) * w + j + 1).copied().unwrap_or(0) + 1,
                _ => dp.get((i + 1) * w + j).copied().unwrap_or(0).max(dp.get(i * w + j + 1).copied().unwrap_or(0)),
            };
            if let Some(c) = dp.get_mut(i * w + j) {
                *c = v;
            }
        }
    }
    let mut out = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    let mut miss_e: Vec<&Tok> = Vec::new();
    let mut miss_h: Vec<&Tok> = Vec::new();
    let flush = |me: &mut Vec<&Tok>, mh: &mut Vec<&Tok>, out: &mut Vec<Miss>| {
        let joined = |v: &Vec<&Tok>| v.iter().map(|t| t.key.as_str()).collect::<String>();
        if !me.is_empty() && (mh.is_empty() || !same(&joined(me), &joined(mh))) {
            out.push(Miss {
                expected: me.iter().map(|t| t.show.as_str()).collect::<Vec<_>>().join(" "),
                heard: mh.iter().map(|t| t.show.as_str()).collect::<Vec<_>>().join(" "),
                common: me.iter().any(|t| t.common && t.key.chars().count() >= 3),
            });
        }
        me.clear();
        mh.clear();
    };
    while i < n || j < m {
        match (e.get(i), h.get(j)) {
            (Some(a), Some(b)) if same(&a.key, &b.key) => {
                flush(&mut miss_e, &mut miss_h, &mut out);
                i += 1;
                j += 1;
            }
            (Some(a), Some(b)) => {
                let down = dp.get((i + 1) * w + j).copied().unwrap_or(0);
                let right = dp.get(i * w + j + 1).copied().unwrap_or(0);
                if down >= right {
                    miss_e.push(a);
                    i += 1;
                } else {
                    miss_h.push(b);
                    j += 1;
                }
            }
            (Some(a), None) => {
                miss_e.push(a);
                i += 1;
            }
            (None, Some(b)) => {
                miss_h.push(b);
                j += 1;
            }
            (None, None) => break,
        }
    }
    flush(&mut miss_e, &mut miss_h, &mut out);
    out
}

/// How bad a take is: misread ordinary words count most.
pub fn score(misses: &[Miss]) -> usize {
    misses.iter().map(|m| if m.common { 10 } else { 1 }).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catches_a_misread_common_word_and_tolerates_names() {
        let script = "[low voice] Hristiyan anlatısında... sahne Efes'tir. [pause] İmparator Decius'un zulmü başlamıştır. [pause] Gençler kaçar. [long pause] Decius, 249 ile 251 yılları arasında hüküm sürer.";
        let heard = "Hristiyan anlatısında sahne Efes'tir. İmparator Decius'un zulmü başlamıştır. Gesar kaçar. Decius 249 ile 251 yılları arasında hüküm sürer";
        let m = compare(script, heard);
        assert_eq!(m.len(), 1, "{m:?}");
        assert_eq!(m.first().map(|x| (x.expected.as_str(), x.heard.as_str(), x.common)), Some(("Gençler", "Gesar", true)));
    }

    #[test]
    fn spelling_variants_and_names_are_not_retried() {
        let script =
            "[curious] Peki ya hikâyenin kendisi? [pause] Thurston ve Attwater'a göre... pagan ya da Yahudi bir efsanenin Hristiyanlaştırılmış hâlidir.";
        let heard = "Peki ya hikayenin kendisi? Thurston ve Etteilla'a göre Pagan ya da Yahudi bir efsanenin Hristiyanlaştırılmış halidir";
        let m = compare(script, heard);
        assert!(m.iter().all(|x| !x.common), "{m:?}");
        assert!(m.len() <= 1, "{m:?}");
        // a foreign name at a sentence start, spelled the Turkish way by the transcriber
        let names = compare(
            "6. yüzyılın sonu. Tours'lu Gregorius taşır. Hristiyan listesi: Maximian, Malchus ve Constantine.",
            "Altıncı yüzyılın sonu. Turlu Gregorius taşır. Hristiyan listesi: Maksimiyan, Malkus ve Konstantin",
        );
        assert!(names.is_empty(), "{names:?}");
        let joined = compare("Panayırdağı'nın yamacında. Belge ile rivayet.", "Panayır Dağı'nın yamacında. Belgeyle rivayet");
        assert!(joined.is_empty(), "{joined:?}");
        let ok = compare("Baronius, 1607'de itiraz etmişti. Üç yüz dokuz yıl.", "Baronius 1607'de itiraz etmiştir. 309 yıl");
        assert!(ok.is_empty(), "{ok:?}");
    }

    #[test]
    fn a_name_opening_a_sentence_is_known_from_the_rest_of_the_script() {
        let names = proper_names(["Bir genç vardı, adı Camasb'dı.", "Şahmeran ona tıp öğretir."]);
        let m = compare_with_names("Camasb sır tutamaz.", "Jamasp sır tutamaz", &names);
        assert!(m.iter().all(|x| !x.common), "{m:?}");
        assert!(compare("Camasb sır tutamaz.", "Jamasp sır tutamaz").iter().any(|x| x.common));
    }

    #[test]
    fn dropped_words_and_hostile_input() {
        let m = compare("Bir sabah uyandığını düşün.", "Bir uyandığını düşün");
        assert_eq!(m.first().map(|x| (x.expected.as_str(), x.heard.as_str(), x.common)), Some(("sabah", "", true)));
        assert!(compare("", "").is_empty());
        assert_eq!(compare("[[[", "]]] ...").len(), 0);
        let long = "kelime ".repeat(5000);
        assert!(compare(&long, &long).is_empty());
    }
}
