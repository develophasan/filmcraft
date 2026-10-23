//! Turkish text helpers for CrStudio: locale-correct case mapping (`i` ↔ `İ`, `ı` ↔ `I`), ASCII
//! folding for search queries and file names, and a small normalizer that spells out numbers so a
//! text-to-speech voice reads them naturally.

/// Upper-case `s` with Turkish rules (`i` → `İ`, `ı` → `I`); other characters use Unicode rules.
pub fn upper(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            'i' => out.push('İ'),
            'ı' => out.push('I'),
            _ => out.extend(c.to_uppercase()),
        }
    }
    out
}

/// Lower-case `s` with Turkish rules (`I` → `ı`, `İ` → `i`).
pub fn lower(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            'I' => out.push('ı'),
            'İ' => out.push('i'),
            _ => out.extend(c.to_lowercase()),
        }
    }
    out
}

/// Fold Turkish letters to ASCII (`ç→c`, `ğ→g`, `ı/İ→i`, `ö→o`, `ş→s`, `ü→u`) and keep only
/// letters, digits and single spaces.
pub fn ascii_fold(s: &str) -> String {
    let mapped: String = s
        .chars()
        .map(|c| match c {
            'ç' | 'Ç' => 'c',
            'ğ' | 'Ğ' => 'g',
            'ı' | 'İ' | 'î' | 'Î' => 'i',
            'ö' | 'Ö' => 'o',
            'ş' | 'Ş' => 's',
            'ü' | 'Ü' | 'û' | 'Û' => 'u',
            'â' | 'Â' => 'a',
            c if c.is_ascii_alphanumeric() => c,
            _ => ' ',
        })
        .collect();
    mapped.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A file-name-safe slug: ASCII-folded, lower case, words joined with `-`, at most 48 characters.
pub fn slug(s: &str) -> String {
    let folded = ascii_fold(s).to_ascii_lowercase();
    let mut out: String = folded.split(' ').filter(|w| !w.is_empty()).collect::<Vec<_>>().join("-");
    if out.len() > 48 {
        out.truncate(48);
        while out.ends_with('-') {
            out.pop();
        }
    }
    if out.is_empty() { "proje".into() } else { out }
}

const ONES: [&str; 10] = ["", "bir", "iki", "üç", "dört", "beş", "altı", "yedi", "sekiz", "dokuz"];
const TENS: [&str; 10] = ["", "on", "yirmi", "otuz", "kırk", "elli", "altmış", "yetmiş", "seksen", "doksan"];

fn below_thousand(n: u64) -> String {
    let mut parts: Vec<&str> = Vec::new();
    let h = (n / 100) % 10;
    let t = (n / 10) % 10;
    let o = n % 10;
    if h > 0 {
        if h > 1 {
            parts.push(ONES[h as usize]);
        }
        parts.push("yüz");
    }
    if t > 0 {
        parts.push(TENS[t as usize]);
    }
    if o > 0 {
        parts.push(ONES[o as usize]);
    }
    parts.join(" ")
}

/// Spell a non-negative integer in Turkish words (`1923` → `bin dokuz yüz yirmi üç`). Numbers of
/// 10^15 and above are returned as digits.
pub fn number_words(n: u64) -> String {
    if n == 0 {
        return "sıfır".into();
    }
    if n >= 1_000_000_000_000_000 {
        return n.to_string();
    }
    let scales = [(1_000_000_000_000u64, "trilyon"), (1_000_000_000, "milyar"), (1_000_000, "milyon"), (1_000, "bin")];
    let mut rest = n;
    let mut parts: Vec<String> = Vec::new();
    for (value, name) in scales {
        let q = rest / value;
        if q > 0 {
            if !(q == 1 && name == "bin") {
                parts.push(below_thousand(q));
            }
            parts.push(name.to_string());
            rest %= value;
        }
    }
    if rest > 0 {
        parts.push(below_thousand(rest));
    }
    parts.join(" ")
}

fn is_vowel(c: char) -> bool {
    matches!(c, 'a' | 'e' | 'ı' | 'i' | 'o' | 'ö' | 'u' | 'ü' | 'â' | 'î' | 'û')
}

fn last_vowel(w: &str) -> char {
    w.chars().rev().find(|c| is_vowel(*c)).unwrap_or('e')
}

/// Two-way vowel harmony (`a` / `e`).
fn harmony2(w: &str) -> char {
    if matches!(last_vowel(w), 'a' | 'ı' | 'o' | 'u' | 'â' | 'û') { 'a' } else { 'e' }
}

/// Four-way vowel harmony (`ı` / `i` / `u` / `ü`).
fn harmony4(w: &str) -> char {
    match last_vowel(w) {
        'a' | 'ı' | 'â' => 'ı',
        'o' | 'u' | 'û' => 'u',
        'ö' | 'ü' => 'ü',
        _ => 'i',
    }
}

/// Split a spelled number into (everything before the last word, last word).
fn split_last(phrase: &str) -> (&str, &str) {
    match phrase.rsplit_once(' ') {
        Some((a, b)) => (a, b),
        None => ("", phrase),
    }
}

fn join_last(head: &str, last: String) -> String {
    if head.is_empty() { last } else { format!("{head} {last}") }
}

/// Ordinal of a spelled number: `yirmi altı` → `yirmi altıncı`, `dört` → `dördüncü`.
pub fn ordinal_words(phrase: &str) -> String {
    let (head, w) = split_last(phrase);
    let w = if w == "dört" { "dörd" } else { w };
    let v = harmony4(w);
    let ends_vowel = w.chars().last().is_some_and(is_vowel);
    join_last(head, if ends_vowel { format!("{w}nc{v}") } else { format!("{w}{v}nc{v}") })
}

/// Ablative (`-dan/-den/-tan/-ten`) of a spelled number: `dokuz` → `dokuzdan`, `üç` → `üçten`.
pub fn ablative_words(phrase: &str) -> String {
    let (head, w) = split_last(phrase);
    let hard = w.chars().last().is_some_and(|c| "fstkçşhp".contains(c));
    join_last(head, format!("{w}{}{}n", if hard { 't' } else { 'd' }, harmony2(w)))
}

/// Dative (`-a/-e`, `-ya/-ye`) of a spelled word: `altıncı` → `altıncıya`, `dört` → `dörde`.
pub fn dative_words(phrase: &str) -> String {
    let (head, w) = split_last(phrase);
    let v = harmony2(w);
    let last = if w == "dört" {
        "dörde".to_string()
    } else if w.chars().last().is_some_and(is_vowel) {
        format!("{w}y{v}")
    } else {
        format!("{w}{v}")
    };
    join_last(head, last)
}

fn roman(s: &str) -> Option<u64> {
    const R: [&str; 20] = ["I", "II", "III", "IV", "V", "VI", "VII", "VIII", "IX", "X", "XI", "XII", "XIII", "XIV", "XV", "XVI", "XVII", "XVIII", "XIX", "XX"];
    R.iter().position(|r| *r == s).map(|i| i as u64 + 1)
}

/// Read one integer starting at `i` (Turkish thousands dots allowed: `1.500`); returns the value
/// and the index after it. Digits beyond u64 range are kept as text by the caller.
fn read_int(chars: &[char], mut i: usize) -> (String, usize) {
    let mut digits = String::new();
    while let Some(c) = chars.get(i).copied() {
        if c.is_ascii_digit() {
            digits.push(c);
            i += 1;
        } else if c == '.' && (1..=3).all(|k| chars.get(i + k).is_some_and(char::is_ascii_digit)) && !chars.get(i + 4).is_some_and(char::is_ascii_digit) {
            // a thousands separator is followed by exactly three digits
            i += 1;
        } else {
            break;
        }
    }
    (digits, i)
}

fn spell(digits: &str) -> String {
    digits.parse::<u64>().map(number_words).unwrap_or_else(|_| digits.to_string())
}

/// After a number at `i`: `.` + space + lower-case word means an ordinal (`26. ayet`).
fn ordinal_dot(chars: &[char], i: usize) -> bool {
    chars.get(i) == Some(&'.') && chars.get(i + 1).is_some_and(|c| c.is_whitespace()) && chars.get(i + 2).is_some_and(|c| c.is_lowercase())
}

/// Prepare narration text for a Turkish voice:
/// - integers spelled out (`1.500` → `bin beş yüz`; `3,5` → `üç virgül beş`)
/// - ordinals (`26. ayet` → `yirmi altıncı ayet`; `II. Theodosius` → `ikinci Theodosius`)
/// - ranges (`249-251` → `iki yüz kırk dokuz ile iki yüz elli bir`; `9-26. ayetler` →
///   `dokuzdan yirmi altıncıya ayetler`)
/// - suffix apostrophes after numbers dropped (`1923'te` → `bin dokuz yüz yirmi üçte`)
/// - common abbreviations expanded (`MS`, `MÖ`, `Hz.`, `vb.`, `yy.`, `bkz.`, `%`…)
pub fn normalize_for_speech(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len() + 16);
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars.get(i).copied().unwrap_or(' ');
        let starts_word = i == 0 || chars.get(i - 1).is_some_and(|p| !p.is_alphanumeric());
        if c.is_ascii_digit() && starts_word {
            let (a, j) = read_int(&chars, i);
            i = j;
            let mut spoken = spell(&a);
            // decimal comma
            if chars.get(i) == Some(&',') && chars.get(i + 1).is_some_and(char::is_ascii_digit) {
                let (d, j) = read_int(&chars, i + 1);
                i = j;
                spoken = format!("{spoken} virgül {}", spell(&d));
            } else if matches!(chars.get(i), Some('-' | '–')) && chars.get(i + 1).is_some_and(char::is_ascii_digit) {
                // range
                let (b, j) = read_int(&chars, i + 1);
                i = j;
                if ordinal_dot(&chars, i) {
                    i += 1;
                    spoken = format!("{} {}", ablative_words(&spoken), dative_words(&ordinal_words(&spell(&b))));
                } else {
                    spoken = format!("{spoken} ile {}", spell(&b));
                }
            } else if ordinal_dot(&chars, i) {
                i += 1;
                spoken = ordinal_words(&spoken);
            }
            // `1923'te` → `…üçte`
            if matches!(chars.get(i), Some('\'' | '’')) && chars.get(i + 1).is_some_and(|c| c.is_alphabetic()) {
                i += 1;
            }
            out.push_str(&spoken);
            continue;
        }
        if c == '%' && chars.get(i + 1).is_some_and(char::is_ascii_digit) {
            out.push_str(" yüzde ");
            i += 1;
            continue;
        }
        out.push(c);
        i += 1;
    }
    // word-level pass: abbreviations and Roman-numeral ordinals
    let words: Vec<&str> = out.split_whitespace().collect();
    let mut res: Vec<String> = Vec::with_capacity(words.len());
    for (k, w) in words.iter().enumerate() {
        let lead: String = w.chars().take_while(|c| "\"“«(".contains(*c)).collect();
        let rest = &w[lead.len()..];
        let core = rest.trim_end_matches([',', ';', ':', '!', '?', ')', '"', '”', '»']);
        let tail = &rest[core.len()..];
        // a sentence-final dot on MS/MÖ stays a full stop
        let (core, tail) = match core.strip_suffix('.') {
            Some(b) if matches!(b, "MS" | "MÖ") => (b, format!(".{tail}")),
            _ => (core, tail.to_string()),
        };
        let next_upper = words.get(k + 1).and_then(|n| n.chars().next()).is_some_and(char::is_uppercase);
        let expanded: Option<String> = match core {
            "MS" | "M.S." => Some("milattan sonra".into()),
            "MÖ" | "M.Ö." => Some("milattan önce".into()),
            "Hz." => Some("Hazreti".into()),
            "vb." => Some("ve benzeri".into()),
            "vs." => Some("vesaire".into()),
            "yy." => Some("yüzyıl".into()),
            "bkz." => Some("bakınız".into()),
            "örn." => Some("örneğin".into()),
            "Dr." => Some("Doktor".into()),
            "Prof." => Some("Profesör".into()),
            "Doç." => Some("Doçent".into()),
            "km" => Some("kilometre".into()),
            _ => core.strip_suffix('.').and_then(roman).filter(|_| next_upper).map(|n| ordinal_words(&number_words(n))),
        };
        match expanded {
            Some(e) => res.push(format!("{lead}{e}{tail}")),
            None => res.push((*w).to_string()),
        }
    }
    res.join(" ")
}

/// Whole-word replacements from the narrator's pronunciation dictionary (`Decius` → `Desiyus`),
/// applied before [`normalize_for_speech`]. Longer entries win; a following Turkish suffix after an
/// apostrophe is kept (`Decius'un` → `Desiyus'un`). Matching is case-sensitive, plus the all-caps form.
pub fn apply_lexicon(text: &str, entries: &[(String, String)]) -> String {
    let mut list: Vec<(String, String)> = Vec::new();
    for (from, to) in entries {
        let from = from.trim();
        if from.is_empty() || from.chars().count() > 80 {
            continue;
        }
        list.push((from.to_string(), to.trim().to_string()));
        // all-caps forms, Turkish (`İ`) and plain (`I`, for foreign names)
        for (f, t) in [(upper(from), upper(to.trim())), (from.to_uppercase(), to.trim().to_uppercase())] {
            if f != from && !list.iter().any(|(x, _)| *x == f) {
                list.push((f, t));
            }
        }
    }
    list.sort_by_key(|(f, _)| std::cmp::Reverse(f.chars().count()));
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0usize;
    'outer: while i < chars.len() {
        let boundary_before = i == 0 || chars.get(i - 1).is_some_and(|c| !c.is_alphanumeric());
        if boundary_before {
            for (from, to) in &list {
                let f: Vec<char> = from.chars().collect();
                if chars.get(i..i + f.len()).is_some_and(|s| s == f.as_slice()) && chars.get(i + f.len()).is_none_or(|c| !c.is_alphanumeric()) {
                    out.push_str(to);
                    i += f.len();
                    continue 'outer;
                }
            }
        }
        if let Some(c) = chars.get(i) {
            out.push(*c);
        }
        i += 1;
    }
    out
}

/// Parse a pronunciation dictionary: one `yazılış<TAB>okunuş` (or `yazılış = okunuş`) per line;
/// `#` starts a comment. At most 2000 entries.
pub fn parse_lexicon(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| {
            let l = l.trim();
            if l.is_empty() || l.starts_with('#') {
                return None;
            }
            let (a, b) = l.split_once('\t').or_else(|| l.split_once('='))?;
            let (a, b) = (a.trim(), b.trim());
            (!a.is_empty() && !b.is_empty()).then(|| (a.to_string(), b.to_string()))
        })
        .take(2000)
        .collect()
}

/// Average Turkish narration pace used to budget words against a duration (words per second).
pub const WORDS_PER_SECOND: f64 = 2.3;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turkish_case_mapping() {
        assert_eq!(upper("istanbul ılık"), "İSTANBUL ILIK");
        assert_eq!(lower("IŞIK İSTANBUL"), "ışık istanbul");
    }

    #[test]
    fn folding_and_slug() {
        assert_eq!(ascii_fold("Göbeklitepe'nin Sırrı!"), "Gobeklitepe nin Sirri");
        assert_eq!(slug("Yedi Uyurlar: Efes Mağarası"), "yedi-uyurlar-efes-magarasi");
        assert_eq!(slug("???"), "proje");
    }

    #[test]
    fn numbers_are_spelled() {
        assert_eq!(number_words(1923), "bin dokuz yüz yirmi üç");
        assert_eq!(number_words(7), "yedi");
        assert_eq!(number_words(100), "yüz");
        assert_eq!(number_words(2_000_000), "iki milyon");
        assert_eq!(normalize_for_speech("Yedi kişi 309 yıl uyudu."), "Yedi kişi üç yüz dokuz yıl uyudu.");
        assert_eq!(normalize_for_speech("oran 3,5 oldu"), "oran üç virgül beş oldu");
    }

    #[test]
    fn ordinals_ranges_and_abbreviations() {
        assert_eq!(normalize_for_speech("MS 249-251 yılları"), "milattan sonra iki yüz kırk dokuz ile iki yüz elli bir yılları");
        assert_eq!(normalize_for_speech("Kehf Suresi, 9-26. ayetler"), "Kehf Suresi, dokuzdan yirmi altıncıya ayetler");
        assert_eq!(normalize_for_speech("II. Theodosius döneminde"), "ikinci Theodosius döneminde");
        assert_eq!(normalize_for_speech("1923'te kuruldu."), "bin dokuz yüz yirmi üçte kuruldu.");
        assert_eq!(normalize_for_speech("4. yüzyılda"), "dördüncü yüzyılda");
        assert_eq!(normalize_for_speech("Nüfus 1.500 kişi."), "Nüfus bin beş yüz kişi.");
        assert_eq!(normalize_for_speech("Olay MS."), "Olay milattan sonra.");
        assert_eq!(normalize_for_speech("Hz. Musa ve %40 oranı"), "Hazreti Musa ve yüzde kırk oranı");
        assert_eq!(normalize_for_speech("Ve 309 yıl uyudular. Sonra"), "Ve üç yüz dokuz yıl uyudular. Sonra");
        assert_eq!(normalize_for_speech("H2O"), "H2O", "digits inside a word are left alone");
        assert_eq!(ordinal_words("on"), "onuncu");
        assert_eq!(ordinal_words("altı"), "altıncı");
        assert_eq!(ablative_words("üç"), "üçten");
        assert_eq!(dative_words("dört"), "dörde");
        // hostile input never panics
        let _ = normalize_for_speech("99999999999999999999999999. a 1-");
        let _ = normalize_for_speech("'%-.,");
    }

    #[test]
    fn lexicon_replaces_whole_words_and_keeps_suffixes() {
        let lex = parse_lexicon("# yorum\nDecius\tDesiyus\nAshab-ı Kehf = Ashabıkehf\nbozuk satır\n");
        assert_eq!(lex.len(), 2);
        assert_eq!(apply_lexicon("Decius'un zulmü, Ashab-ı Kehf ve Deciusçu", &lex), "Desiyus'un zulmü, Ashabıkehf ve Deciusçu");
        assert_eq!(apply_lexicon("DECIUS", &lex), "DESIYUS");
    }
}
