//! UI language. Korean is the default; English can be selected in preferences.
//! Strings are written in both languages at the call site: `tr!(korean, english)`, and `trf!(korean, english, args)` for formatted text.
//! Korean names left in constant tables (also used as keys) are translated for display with `t()`.

use std::sync::atomic::{AtomicBool, Ordering};

static EN: AtomicBool = AtomicBool::new(false);

pub fn set_en(on: bool) {
    EN.store(on, Ordering::Relaxed);
}

#[inline]
pub fn en() -> bool {
    EN.load(Ordering::Relaxed)
}

/// Fixed string: `&'static str` in the current language
macro_rules! tr {
    ($ko:literal, $en:literal) => {
        if $crate::i18n::en() { $en } else { $ko }
    };
}

/// Formatted string: takes the same arguments as `format!` (both templates must use the same placeholders)
macro_rules! trf {
    ($ko:literal, $en:literal $($rest:tt)*) => {
        if $crate::i18n::en() { $crate::i18n::singular(format!($en $($rest)*)) } else { format!($ko $($rest)*) }
    };
}

/// English singular: "1 photos" -> "1 photo" (only a plural noun right after the number 1)
pub fn singular(s: String) -> String {
    const WORDS: &[(&str, &str)] = &[
        ("photos", "photo"), ("faces", "face"), ("people", "person"), ("regions", "region"), ("stacks", "stack"),
        ("presets", "preset"), ("files", "file"), ("collections", "collection"), ("watermarks", "watermark"),
        ("stars", "star"), ("spots", "spot"), ("pages", "page"), ("notes", "note"), ("masks", "mask"), ("eyes", "eye"),
        ("days", "day"), ("conflicts", "conflict"), ("virtual copies", "virtual copy"), ("new people", "new person"),
        ("RAW photos", "RAW photo"), ("AI masks", "AI mask"), ("XMP sidecars", "XMP sidecar"),
    ];
    if !s.contains("1 ") {
        return s;
    }
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        let one = b[i] == b'1' && i + 1 < b.len() && b[i + 1] == b' ' && (i == 0 || !(b[i - 1].is_ascii_digit() || b[i - 1] == b'.' || b[i - 1] == b','));
        if one {
            let rest = &s[i + 2..];
            if let Some((pl, sg)) = WORDS.iter().find(|(pl, _)| rest.starts_with(pl) && !rest[pl.len()..].starts_with(|c: char| c.is_ascii_alphabetic())) {
                out.push_str("1 ");
                out.push_str(sg);
                i += 2 + pl.len();
                continue;
            }
        }
        let ch = s[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn singular_only_for_one() {
        assert_eq!(super::singular("Export 1 photos".into()), "Export 1 photo");
        assert_eq!(super::singular("11 photos · 1 faces".into()), "11 photos · 1 face");
        assert_eq!(super::singular("2.1 photos, 1 photosets".into()), "2.1 photos, 1 photosets");
        assert_eq!(super::singular("1 new people · 1 RAW photos".into()), "1 new person · 1 RAW photo");
    }
}

/// Korean name from a constant table -> current language (unchanged if not in the table)
pub fn t(ko: &str) -> &str {
    if !en() {
        return ko;
    }
    match TABLE.binary_search_by(|(k, _)| (*k).cmp(ko)) {
        Ok(i) => TABLE[i].1,
        Err(_) => ko,
    }
}

static TABLE: &[(&str, &str)] = &[
    ("100% 확대 전환", "Toggle 100% zoom"),
    ("Alt + 슬라이더", "Alt + slider"),
    ("HSL 믹서 · 컬러 그레이딩 · 보정", "HSL mixer · Color grading · Calibration"),
    ("XMP 사이드카에 저장", "Save to XMP sidecar"),
    ("가리기 (얼굴 모자이크)", "Privacy (face mosaic)"),
    ("가상 사본", "Virtual copy"),
    ("가져오기", "Import"),
    ("가져오기 / 내보내기", "Import / Export"),
    ("공통", "Common"),
    ("광학", "Optics"),
    ("그늘", "Shade"),
    ("기본 · 톤 커브", "Basic · Tone curve"),
    ("끄는 동안 클리핑 표시", "Show clipping while dragging"),
    ("내보내기 프리셋", "Export presets"),
    ("노랑", "Yellow"),
    ("노출", "Exposure"),
    ("단축키", "Shortcuts"),
    ("단축키 도움말", "Shortcut help"),
    ("디테일", "Detail"),
    ("라이브러리", "Library"),
    ("라이브러리 그리드 / 현상", "Library grid / Develop"),
    ("렌즈", "Lens"),
    ("렌즈 교정 · 변형", "Lens corrections · Transform"),
    ("루페 / 비교 / 서베이", "Loupe / Compare / Survey"),
    ("마스크", "Masks"),
    ("마젠타", "Magenta"),
    ("만들기 · 편집", "Create · Edit"),
    ("명령 찾기", "Command palette"),
    ("모든 섹션 한 번에", "All sections at once"),
    ("미리보기 줄 위치 (왼쪽·아래·숨김)", "Filmstrip position (left·bottom·hidden)"),
    ("방사형", "Radial"),
    ("별점", "Rating"),
    ("보라", "Purple"),
    ("브러시", "Brush"),
    ("브러시·스팟 크기", "Brush·spot size"),
    ("블랙", "Blacks"),
    ("비네팅 · 그레인", "Vignette · Grain"),
    ("빛", "Light"),
    ("빠른 컬렉션", "Quick Collection"),
    ("빨강", "Red"),
    ("사용자 텍스트", "Custom text"),
    ("색", "Color"),
    ("색상 라벨", "Color label"),
    ("색상 범위", "Color Range"),
    ("샤프닝 · 노이즈 감소", "Sharpening · Noise reduction"),
    ("섀도", "Shadows"),
    ("선택 / 거부 / 깃발 없음", "Pick / Reject / Unflag"),
    ("선형", "Linear"),
    ("설정 복사 / 붙여넣기", "Copy / paste settings"),
    ("소프트 교정 (인쇄 미리 보기)", "Soft proofing (print preview)"),
    ("스택 펼치기 / 접기", "Expand / collapse stack"),
    ("스택 해제", "Unstack"),
    ("스택으로 묶기", "Group into stack"),
    ("스팟 제거", "Spot Removal"),
    ("슬라이드쇼", "Slideshow"),
    ("실행 취소 / 다시 실행", "Undo / Redo"),
    ("아쿠아", "Aqua"),
    ("옆 패널 / 모든 패널 숨기기", "Hide side panels / all panels"),
    ("워터마크", "Watermark"),
    ("원본 파일명", "Original file name"),
    ("이름 일괄 변경", "Batch Rename"),
    ("이전 보기 / 나란히 / 분할", "Before / side by side / split"),
    ("이전 사진 설정 적용", "Apply previous photo's settings"),
    ("인쇄 · 레이아웃 (PDF)", "Print · Layout (PDF)"),
    ("일련번호", "Sequence number"),
    ("일련번호 (3자리)", "Sequence (3 digits)"),
    ("일반", "General"),
    ("자동 적용", "Auto-apply"),
    ("자동 톤", "Auto Tone"),
    ("자르기·회전", "Crop·Rotate"),
    ("작가", "Artist"),
    ("작가 정보", "Artist info"),
    ("저작권", "Copyright"),
    ("저장소", "Storage"),
    ("저장한 설정", "Saved settings"),
    ("전체", "All"),
    ("정보", "About"),
    ("제목", "Title"),
    ("주광", "Daylight"),
    ("주황", "Orange"),
    ("참조 보기 (다른 사진을 왼쪽에 고정)", "Reference view (pin another photo on the left)"),
    ("초록", "Green"),
    ("총 노출 맞추기", "Match Total Exposures"),
    ("촬영 날짜", "Capture date"),
    ("촬영 연도", "Capture year"),
    ("촬영 정보", "Shooting info"),
    ("촬영시각 HHMMSS", "Capture time HHMMSS"),
    ("촬영일 YYYYMMDD", "Capture date YYYYMMDD"),
    ("카메라", "Camera"),
    ("카메라 모델", "Camera model"),
    ("카탈로그 · 캐시", "Catalog · Cache"),
    ("클리핑 / 마스크 오버레이", "Clipping / mask overlay"),
    ("키보드", "Keyboard"),
    ("텅스텐", "Tungsten"),
    ("테마 · 배율 · 배치", "Theme · Scale · Layout"),
    ("파노라마 병합", "Panorama merge"),
    ("파랑", "Blue"),
    ("파일명", "File name"),
    ("평가", "Rating"),
    ("플래시", "Flash"),
    ("하이라이트", "Highlights"),
    ("현상", "Develop"),
    ("형광등", "Fluorescent"),
    ("화면", "Display"),
    ("화이트", "Whites"),
    ("화이트 밸런스 스포이드", "White balance eyedropper"),
    ("효과", "Effects"),
    ("휘도 범위", "Luminance Range"),
    ("흐림", "Cloudy"),
];

// Translating stored strings to the current language

mod pairs {
    include!(concat!(env!("OUT_DIR"), "/i18n_pairs.rs"));
}

/// Show a stored string (history/snapshot names etc., saved in the language active when recorded) in the current language.
/// Looks it up among the source's tr!/trf! pairs (collected at build time) and the table above: fixed strings are swapped, "Name +0.65" (slider) swaps only the name,
/// and formatted strings are re-filled into the other language's template. Unmatched strings are returned as is (e.g. imported names).
pub fn label(s: &str) -> String {
    static CACHE: std::sync::OnceLock<parking_lot::Mutex<std::collections::HashMap<(bool, String), String>>> = std::sync::OnceLock::new();
    let key = (en(), s.to_string());
    let cache = CACHE.get_or_init(Default::default);
    if let Some(v) = cache.lock().get(&key) {
        return v.clone();
    }
    let v = translate(s, key.0).unwrap_or_else(|| s.to_string());
    cache.lock().insert(key, v.clone());
    v
}

fn translate(s: &str, to_en: bool) -> Option<String> {
    exact(s, to_en).or_else(|| slider(s, to_en)).or_else(|| template(s, to_en))
}

/// Fixed strings
fn exact(s: &str, to_en: bool) -> Option<String> {
    let pick = |(ko, en): &(&str, &str)| if to_en { (*ko == s).then(|| en.to_string()) } else { (*en == s || singular(en.to_string()) == s).then(|| ko.to_string()) };
    pairs::TR_PAIRS
        .iter()
        .find_map(pick)
        .or_else(|| TABLE.iter().find_map(pick))
        .or_else(|| pairs::TRF_PAIRS.iter().filter(|(k, _)| !k.contains('{')).find_map(pick))
}

/// Slider history entry "Name +0.65": translate only the name
fn slider(s: &str, to_en: bool) -> Option<String> {
    let (name, v) = s.rsplit_once(' ')?;
    (v.starts_with(['+', '-']) && v[1..].parse::<f64>().is_ok()).then_some(())?;
    Some(format!("{} {v}", exact(name, to_en)?))
}

/// Formatted strings: if it matches a template in the original language ("Exposure: {}"), extract the values and fill the other template
fn template(s: &str, to_en: bool) -> Option<String> {
    let mut best: Option<(usize, String)> = None;
    for (ko, en) in pairs::TRF_PAIRS {
        let (from, to) = if to_en { (*ko, *en) } else { (*en, *ko) };
        let (lits, names) = split_fmt(from);
        // Templates without placeholders are fixed strings (`exact`); skip templates with no literal text ("{}") or adjacent placeholders as ambiguous
        if names.is_empty() {
            continue;
        }
        let lit_len: usize = lits.iter().map(|l| l.chars().count()).sum();
        if lit_len < 2 || lits[1..lits.len() - 1].iter().any(|l| l.is_empty()) {
            continue;
        }
        let Some(caps) = match_fmt(s, &lits) else { continue };
        // Also translate fixed strings inside values (e.g. "Auto sync: Auto tone")
        let caps: Vec<String> = caps.iter().map(|c| exact(c, to_en).unwrap_or_else(|| c.to_string())).collect();
        let (tl, tn) = split_fmt(to);
        let mut out = String::new();
        for (i, l) in tl.iter().enumerate() {
            out.push_str(l);
            if i < tn.len() {
                // Named placeholders match by name, otherwise by position
                let j = if tn[i].is_empty() { i } else { names.iter().position(|n| *n == tn[i]).unwrap_or(i) };
                out.push_str(caps.get(j).map(|c| c.as_str()).unwrap_or(""));
            }
        }
        let out = if to_en { singular(out) } else { out };
        if best.as_ref().is_none_or(|(b, _)| lit_len > *b) {
            best = Some((lit_len, out));
        }
    }
    best.map(|b| b.1)
}

/// Format string -> (literal pieces, placeholder names); pieces = names + 1. "{{" and "}}" are literal text
fn split_fmt(f: &str) -> (Vec<String>, Vec<String>) {
    let (mut lits, mut names, mut cur) = (Vec::new(), Vec::new(), String::new());
    let mut it = f.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '{' if it.peek() == Some(&'{') => {
                it.next();
                cur.push('{');
            }
            '}' if it.peek() == Some(&'}') => {
                it.next();
                cur.push('}');
            }
            '{' => {
                let mut spec = String::new();
                for c in it.by_ref() {
                    if c == '}' {
                        break;
                    }
                    spec.push(c);
                }
                lits.push(std::mem::take(&mut cur));
                names.push(spec.split(':').next().unwrap_or("").to_string());
            }
            c => cur.push(c),
        }
    }
    lits.push(cur);
    (lits, names)
}

/// Match literal pieces in order and extract placeholder values (shortest match between pieces)
fn match_fmt<'a>(s: &'a str, lits: &[String]) -> Option<Vec<&'a str>> {
    let rest = s.strip_prefix(lits[0].as_str())?;
    let n = lits.len();
    if n == 1 {
        return rest.is_empty().then(Vec::new);
    }
    let mut caps = Vec::new();
    let mut pos = 0;
    for (k, l) in lits[1..].iter().enumerate() {
        let last = k == n - 2;
        let at = if last {
            if !rest[pos..].ends_with(l.as_str()) || rest.len() - l.len() < pos {
                return None;
            }
            rest.len() - l.len()
        } else {
            pos + rest[pos..].find(l.as_str())?
        };
        if at == pos {
            return None; // Empty value
        }
        caps.push(&rest[pos..at]);
        pos = at + l.len();
    }
    Some(caps)
}

#[cfg(test)]
mod label_tests {
    #[test]
    fn saved_labels_follow_language() {
        let t = |s: &str, en: bool| super::translate(s, en).unwrap_or_else(|| s.to_string());
        assert_eq!(t("노출 +0.65", true), "Exposure +0.65");
        assert_eq!(t("렌즈 프로파일 자동", true), "Lens profile auto");
        assert_eq!(t("스냅샷 3", true), "Snapshot 3");
        assert_eq!(t("Lightroom에서 온 이름", true), "Lightroom에서 온 이름");
        assert_eq!(t("Exposure +0.65", false), "노출 +0.65");
        assert_eq!(t("Snapshot 3", false), "스냅샷 3");
        assert_eq!(t("Lens profile auto", false), "렌즈 프로파일 자동");
    }
}
