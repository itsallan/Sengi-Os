//! Matching a typed query against names and ranking the results.
//!
//! Text is compared in a folded form: lower case, common Latin accents
//! removed. Each folded character remembers which bytes of the original it
//! came from, so matches can be reported as ranges to highlight.

use std::cmp::Reverse;
use std::ops::Range;

/// How often and how recently something was used; larger sorts first.
pub type Weight = (u32, u64);

/// How well a query matched, best first. Derived ordering is the ranking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    NameExact,
    NamePrefix,
    NameWordPrefix,
    NameSubstring,
    GenericPrefix,
    GenericSubstring,
    KeywordPrefix,
    KeywordSubstring,
    CommentPrefix,
    CommentSubstring,
    /// The characters appear in the name in order, with gaps.
    NameFuzzy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TieBreak {
    /// By name, for applications.
    Alphabetical,
    /// As given, for menus.
    InputOrder,
}

#[derive(Debug, Clone, Default)]
struct Folded {
    chars: Vec<char>,
    /// Byte range in the original string each folded character came from.
    spans: Vec<(usize, usize)>,
    /// Whether a word starts at this character.
    word_start: Vec<bool>,
}

fn unaccent(c: char) -> Option<&'static str> {
    Some(match c {
        'à'..='å' | 'ā' | 'ă' | 'ą' => "a",
        'æ' => "ae",
        'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => "c",
        'ď' | 'đ' => "d",
        'è'..='ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => "e",
        'ĝ' | 'ğ' | 'ġ' | 'ģ' => "g",
        'ĥ' | 'ħ' => "h",
        'ì'..='ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' | 'ı' => "i",
        'ĵ' => "j",
        'ķ' => "k",
        'ĺ' | 'ļ' | 'ľ' | 'ŀ' | 'ł' => "l",
        'ñ' | 'ń' | 'ņ' | 'ň' => "n",
        'ò'..='ö' | 'ø' | 'ō' | 'ŏ' | 'ő' => "o",
        'œ' => "oe",
        'ŕ' | 'ŗ' | 'ř' => "r",
        'ś' | 'ŝ' | 'ş' | 'š' => "s",
        'ß' => "ss",
        'ţ' | 'ť' | 'ŧ' => "t",
        'ù'..='ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' => "u",
        'ŵ' => "w",
        'ý' | 'ÿ' | 'ŷ' => "y",
        'ź' | 'ż' | 'ž' => "z",
        _ => return None,
    })
}

fn is_combining_mark(c: char) -> bool {
    ('\u{0300}'..='\u{036f}').contains(&c)
}

/// Zero-width non-joiner and joiner, and the emoji variation selector.
/// They shape the text around them and are not typed as letters.
fn is_joiner(c: char) -> bool {
    matches!(c, '\u{200c}' | '\u{200d}' | '\u{fe0f}')
}

impl Folded {
    fn new(text: &str) -> Self {
        let mut folded = Folded::default();
        let mut previous: Option<char> = None;
        for (start, c) in text.char_indices() {
            if is_combining_mark(c) || is_joiner(c) {
                // Keep a mark with the letter it sits on, so a highlight
                // never splits the two. Joiners go the same way: a name
                // is found whether or not the query was typed with them.
                let end = start + c.len_utf8();
                for span in folded.spans.iter_mut().rev() {
                    if span.1 != start {
                        break;
                    }
                    span.1 = end;
                }
                continue;
            }
            let span = (start, start + c.len_utf8());
            let mut word_start = c.is_alphanumeric()
                && previous
                    .is_none_or(|p| !p.is_alphanumeric() || (p.is_lowercase() && c.is_uppercase()));
            let mut push = |ch: char| {
                folded.chars.push(ch);
                folded.spans.push(span);
                folded.word_start.push(word_start);
                word_start = false;
            };
            for lower in c.to_lowercase() {
                if is_combining_mark(lower) {
                    continue;
                }
                match unaccent(lower) {
                    Some(plain) => plain.chars().for_each(&mut push),
                    None => push(lower),
                }
            }
            previous = Some(c);
        }
        folded
    }

    /// The best place `needle` occurs as a run: 0 for the very start, 1 for
    /// the start of a later word, 2 for anywhere else.
    fn find(&self, needle: &[char]) -> Option<(u8, usize)> {
        if needle.is_empty() || needle.len() > self.chars.len() {
            return None;
        }
        let mut anywhere = None;
        for at in 0..=self.chars.len() - needle.len() {
            if self.chars[at..at + needle.len()] != *needle {
                continue;
            }
            if at == 0 {
                return Some((0, 0));
            }
            if self.word_start[at] {
                return Some((1, at));
            }
            anywhere.get_or_insert(at);
        }
        anywhere.map(|at| (2, at))
    }

    /// Find `needle` as a subsequence, preferring word starts and unbroken
    /// runs. Returns a score (higher is tighter) and the matched positions.
    fn subsequence(&self, needle: &[char]) -> Option<(i32, Vec<usize>)> {
        // Long names are cut off; nobody fuzzy-types their way to column 80.
        const LIMIT: usize = 64;
        const WORD_START: i32 = 6;
        const ADJACENT: i32 = 4;
        const MAX_GAP: i32 = 3;
        const NONE: i32 = i32::MIN / 2;

        let n = self.chars.len().min(LIMIT);
        let m = needle.len();
        if m < 2 || m > n {
            return None;
        }
        let bonus = |i: usize| if self.word_start[i] { WORD_START } else { 0 };

        // score[j * n + i]: best score with needle[j] placed at position i.
        let mut score = vec![NONE; m * n];
        let mut from = vec![0usize; m * n];
        for (i, first) in score.iter_mut().enumerate().take(n) {
            if self.chars[i] == needle[0] {
                *first = bonus(i) - (i as i32).min(MAX_GAP);
            }
        }
        for j in 1..m {
            for i in j..n {
                if self.chars[i] != needle[j] {
                    continue;
                }
                for k in j - 1..i {
                    let before = score[(j - 1) * n + k];
                    if before == NONE {
                        continue;
                    }
                    let gap = (i - k - 1) as i32;
                    let step = if gap == 0 {
                        ADJACENT
                    } else {
                        -gap.min(MAX_GAP)
                    };
                    let total = before + bonus(i) + step;
                    if total > score[j * n + i] {
                        score[j * n + i] = total;
                        from[j * n + i] = k;
                    }
                }
            }
        }

        let last = (m - 1) * n;
        let (mut at, best) = (0..n)
            .map(|i| (i, score[last + i]))
            .filter(|&(_, s)| s != NONE)
            .max_by_key(|&(i, s)| (s, Reverse(i)))?;
        let mut positions = vec![0; m];
        for j in (0..m).rev() {
            positions[j] = at;
            at = from[j * n + at];
        }
        Some((best, positions))
    }

    fn byte_ranges(&self, positions: impl IntoIterator<Item = usize>) -> Vec<Range<usize>> {
        let mut spans: Vec<(usize, usize)> = positions.into_iter().map(|p| self.spans[p]).collect();
        spans.sort_unstable();
        let mut ranges: Vec<Range<usize>> = Vec::new();
        for (start, end) in spans {
            match ranges.last_mut() {
                Some(last) if start <= last.end => last.end = last.end.max(end),
                _ => ranges.push(start..end),
            }
        }
        ranges
    }
}

/// A query, folded and split into words once for all candidates.
#[derive(Debug, Clone, Default)]
pub struct Query {
    whole: Vec<char>,
    words: Vec<Vec<char>>,
}

impl Query {
    pub fn new(text: &str) -> Self {
        let folded: String = Folded::new(text).chars.into_iter().collect();
        let words: Vec<Vec<char>> = folded
            .split_whitespace()
            .map(|w| w.chars().collect())
            .collect();
        let whole = folded
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .collect();
        Query { whole, words }
    }

    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }
}

/// Something that can be searched for: an application or a menu line.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub name: String,
    /// Shown next to the name; also searched.
    pub detail: String,
    pub weight: Weight,
    name_folded: Folded,
    sort_name: String,
    detail_folded: Folded,
    keywords: Vec<Folded>,
    comment: Folded,
}

impl Candidate {
    pub fn new(name: impl Into<String>) -> Self {
        let name = name.into();
        let name_folded = Folded::new(&name);
        let sort_name = name_folded.chars.iter().collect();
        Candidate {
            name,
            detail: String::new(),
            weight: (0, 0),
            name_folded,
            sort_name,
            detail_folded: Folded::default(),
            keywords: Vec::new(),
            comment: Folded::default(),
        }
    }

    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self.detail_folded = Folded::new(&self.detail);
        self
    }

    pub fn keywords<S: AsRef<str>>(mut self, keywords: impl IntoIterator<Item = S>) -> Self {
        self.keywords = keywords
            .into_iter()
            .map(|k| Folded::new(k.as_ref()))
            .collect();
        self
    }

    pub fn comment(mut self, comment: &str) -> Self {
        self.comment = Folded::new(comment);
        self
    }

    pub fn weight(mut self, weight: Weight) -> Self {
        self.weight = weight;
        self
    }
}

/// One ranked result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    /// Index into the candidate list that was searched.
    pub index: usize,
    pub tier: Tier,
    /// Byte ranges of the name that matched, ascending and not touching.
    pub ranges: Vec<Range<usize>>,
}

struct WordMatch {
    tier: Tier,
    fuzzy_score: i32,
    name_positions: Vec<usize>,
}

fn match_word(candidate: &Candidate, word: &[char], allow_fuzzy: bool) -> Option<WordMatch> {
    let name = &candidate.name_folded;
    if let Some((place, at)) = name.find(word) {
        let tier = match place {
            0 if word.len() == name.chars.len() => Tier::NameExact,
            0 => Tier::NamePrefix,
            1 => Tier::NameWordPrefix,
            _ => Tier::NameSubstring,
        };
        return Some(WordMatch {
            tier,
            fuzzy_score: 0,
            name_positions: (at..at + word.len()).collect(),
        });
    }

    let place_in = |fields: &[&Folded]| {
        fields
            .iter()
            .filter_map(|f| f.find(word))
            .map(|(p, _)| p)
            .min()
    };
    let keywords: Vec<&Folded> = candidate.keywords.iter().collect();
    let secondary = [
        (
            place_in(&[&candidate.detail_folded]),
            Tier::GenericPrefix,
            Tier::GenericSubstring,
        ),
        (
            place_in(&keywords),
            Tier::KeywordPrefix,
            Tier::KeywordSubstring,
        ),
        (
            place_in(&[&candidate.comment]),
            Tier::CommentPrefix,
            Tier::CommentSubstring,
        ),
    ];
    let tier = secondary
        .iter()
        .filter_map(|&(place, prefix, substring)| {
            place.map(|p| if p < 2 { prefix } else { substring })
        })
        .min();
    if let Some(tier) = tier {
        return Some(WordMatch {
            tier,
            fuzzy_score: 0,
            name_positions: Vec::new(),
        });
    }

    if !allow_fuzzy {
        return None;
    }
    let (fuzzy_score, name_positions) = name.subsequence(word)?;
    Some(WordMatch {
        tier: Tier::NameFuzzy,
        fuzzy_score,
        name_positions,
    })
}

/// Match one candidate. With several words each must match somewhere; the
/// result is as good as its weakest word.
fn match_candidate(candidate: &Candidate, query: &Query) -> Option<(Tier, i32, Vec<Range<usize>>)> {
    let name = &candidate.name_folded;
    if query.words.len() > 1 {
        if let Some(m) = match_word(candidate, &query.whole, false) {
            return Some((m.tier, 0, name.byte_ranges(m.name_positions)));
        }
    }
    let mut tier = Tier::NameExact;
    let mut fuzzy_score = 0;
    let mut positions = Vec::new();
    for word in &query.words {
        let m = match_word(candidate, word, true)?;
        tier = tier.max(m.tier);
        fuzzy_score += m.fuzzy_score;
        positions.extend(m.name_positions);
    }
    if query.words.len() > 1 && tier == Tier::NameExact {
        tier = Tier::NamePrefix;
    }
    Some((tier, fuzzy_score, name.byte_ranges(positions)))
}

/// Filter and order `candidates` for `query`. An empty query keeps everything,
/// most used first.
pub fn rank(candidates: &[Candidate], query: &Query, tie_break: TieBreak) -> Vec<Hit> {
    let mut hits: Vec<(Hit, i32)> = candidates
        .iter()
        .enumerate()
        .filter_map(|(index, candidate)| {
            if query.is_empty() {
                let tier = Tier::NameExact;
                let ranges = Vec::new();
                return Some((
                    Hit {
                        index,
                        tier,
                        ranges,
                    },
                    0,
                ));
            }
            let (tier, fuzzy_score, ranges) = match_candidate(candidate, query)?;
            Some((
                Hit {
                    index,
                    tier,
                    ranges,
                },
                fuzzy_score,
            ))
        })
        .collect();

    hits.sort_by(|(a, a_score), (b, b_score)| {
        let (ca, cb) = (&candidates[a.index], &candidates[b.index]);
        a.tier
            .cmp(&b.tier)
            .then(b_score.cmp(a_score))
            .then(cb.weight.cmp(&ca.weight))
            .then_with(|| match tie_break {
                TieBreak::Alphabetical => ca.sort_name.cmp(&cb.sort_name),
                TieBreak::InputOrder => std::cmp::Ordering::Equal,
            })
            .then(a.index.cmp(&b.index))
    });
    hits.into_iter().map(|(hit, _)| hit).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<Candidate> {
        list.iter().map(|n| Candidate::new(*n)).collect()
    }

    fn ranked<'a>(candidates: &'a [Candidate], query: &str) -> Vec<&'a str> {
        rank(candidates, &Query::new(query), TieBreak::Alphabetical)
            .iter()
            .map(|hit| candidates[hit.index].name.as_str())
            .collect()
    }

    fn highlight(name: &str, query: &str) -> String {
        let candidates = [Candidate::new(name)];
        let hits = rank(&candidates, &Query::new(query), TieBreak::Alphabetical);
        let Some(hit) = hits.first() else {
            return "(no match)".into();
        };
        let mut out = String::new();
        let mut at = 0;
        for range in &hit.ranges {
            out.push_str(&name[at..range.start]);
            out.push('[');
            out.push_str(&name[range.clone()]);
            out.push(']');
            at = range.end;
        }
        out.push_str(&name[at..]);
        out
    }

    #[test]
    fn joiners_do_not_stand_between_a_query_and_a_name() {
        let fold = |s: &str| Folded::new(s).chars.into_iter().collect::<String>();
        let with = "پرونده\u{200c}ها";
        let without = "پروندهها";
        assert_eq!(fold(with), fold(without));
        // The joiner stays with the letter before it, so a highlight of
        // the whole name covers the whole name.
        let folded = Folded::new(with);
        assert_eq!(folded.spans.last().map(|span| span.1), Some(with.len()));
        assert_eq!(fold("\u{200c}a\u{200d}b\u{fe0f}"), "ab");
    }

    #[test]
    fn folding_lowers_case_and_strips_accents() {
        let fold = |s: &str| Folded::new(s).chars.into_iter().collect::<String>();
        assert_eq!(fold("Café"), "cafe");
        assert_eq!(fold("ÉCOLE Zoë"), "ecole zoe");
        assert_eq!(fold("Straße"), "strasse");
        assert_eq!(fold("Cafe\u{301}"), "cafe");
        assert_eq!(fold("İstanbul Łódź"), "istanbul lodz");
        assert_eq!(fold("日本語 Ж"), "日本語 ж");
    }

    #[test]
    fn prefix_beats_word_prefix_beats_substring_beats_fuzzy() {
        let candidates = names(&[
            "Software Terminal Emulator",
            "Determinator",
            "Terminal",
            "Text Editor Mini",
            "Term",
        ]);
        assert_eq!(
            ranked(&candidates, "term"),
            vec![
                "Term",
                "Terminal",
                "Software Terminal Emulator",
                "Determinator",
                "Text Editor Mini",
            ]
        );
        let tiers: Vec<Tier> = rank(&candidates, &Query::new("term"), TieBreak::Alphabetical)
            .iter()
            .map(|h| h.tier)
            .collect();
        assert_eq!(
            tiers,
            vec![
                Tier::NameExact,
                Tier::NamePrefix,
                Tier::NameWordPrefix,
                Tier::NameSubstring,
                Tier::NameFuzzy,
            ]
        );
    }

    #[test]
    fn name_beats_generic_beats_keyword_beats_comment() {
        let candidates = vec![
            Candidate::new("Delta").comment("Opens web pages"),
            Candidate::new("Charlie").keywords(["internet", "web"]),
            Candidate::new("Bravo").detail("Web Browser"),
            Candidate::new("Alpha Web"),
            Candidate::new("Echo").detail("Cobweb viewer"),
            Candidate::new("Unrelated").comment("Nothing to see"),
        ];
        assert_eq!(
            ranked(&candidates, "web"),
            vec!["Alpha Web", "Bravo", "Echo", "Charlie", "Delta"]
        );
    }

    #[test]
    fn literal_matches_elsewhere_beat_fuzzy_name_matches() {
        let candidates = vec![
            Candidate::new("Wine Explorer Beta"),
            Candidate::new("Firefox").keywords(["web"]),
        ];
        assert_eq!(
            ranked(&candidates, "web"),
            vec!["Firefox", "Wine Explorer Beta"]
        );
    }

    #[test]
    fn matching_ignores_case_and_accents_both_ways() {
        let candidates = names(&["Café Écran", "Cafe Plain", "Other"]);
        assert_eq!(
            ranked(&candidates, "CAFE"),
            vec!["Café Écran", "Cafe Plain"]
        );
        assert_eq!(
            ranked(&candidates, "café"),
            vec!["Café Écran", "Cafe Plain"]
        );
        assert_eq!(ranked(&candidates, "ecran"), vec!["Café Écran"]);
    }

    #[test]
    fn no_match_and_blank_queries() {
        let candidates = names(&["Bravo", "alpha", "Charlie"]);
        assert!(ranked(&candidates, "zzz").is_empty());
        assert!(ranked(&candidates, "ab").is_empty());
        assert_eq!(ranked(&candidates, ""), vec!["alpha", "Bravo", "Charlie"]);
        assert_eq!(
            ranked(&candidates, "   "),
            vec!["alpha", "Bravo", "Charlie"]
        );
        assert!(ranked(&[], "x").is_empty());
    }

    #[test]
    fn single_letters_do_not_fuzzy_match() {
        let candidates = names(&["Xylophone", "Box", "Alpha"]);
        assert_eq!(ranked(&candidates, "x"), vec!["Xylophone", "Box"]);
    }

    #[test]
    fn ties_go_to_usage_then_alphabet() {
        let candidates = vec![
            Candidate::new("Files"),
            Candidate::new("Firefox").weight((3, 100)),
            Candidate::new("Fig").weight((3, 200)),
            Candidate::new("Finder").weight((9, 50)),
            Candidate::new("Fiddle"),
        ];
        assert_eq!(
            ranked(&candidates, "fi"),
            vec!["Finder", "Fig", "Firefox", "Fiddle", "Files"]
        );
        assert_eq!(
            ranked(&candidates, ""),
            vec!["Finder", "Fig", "Firefox", "Fiddle", "Files"]
        );
    }

    #[test]
    fn usage_does_not_outrank_a_better_match() {
        let candidates = vec![
            Candidate::new("Disk Terminal").weight((50, 500)),
            Candidate::new("Terminal"),
        ];
        assert_eq!(
            ranked(&candidates, "ter"),
            vec!["Terminal", "Disk Terminal"]
        );
    }

    #[test]
    fn input_order_is_kept_for_menus() {
        let candidates = names(&["Lock", "Log out", "Restart", "Shut down"]);
        let order = |query: &str| -> Vec<&str> {
            rank(&candidates, &Query::new(query), TieBreak::InputOrder)
                .iter()
                .map(|hit| candidates[hit.index].name.as_str())
                .collect()
        };
        assert_eq!(order(""), vec!["Lock", "Log out", "Restart", "Shut down"]);
        assert_eq!(order("lo"), vec!["Lock", "Log out"]);
        assert_eq!(order("s"), vec!["Shut down", "Restart"]);
        assert_eq!(order("out"), vec!["Log out"]);
    }

    #[test]
    fn several_words_must_all_match() {
        let candidates = vec![
            Candidate::new("LibreOffice Writer").detail("Word Processor"),
            Candidate::new("LibreOffice Calc").detail("Spreadsheet"),
            Candidate::new("Writer's Block"),
        ];
        assert_eq!(
            ranked(&candidates, "libre writer"),
            vec!["LibreOffice Writer"]
        );
        assert_eq!(
            ranked(&candidates, "writer libre"),
            vec!["LibreOffice Writer"]
        );
        assert_eq!(
            ranked(&candidates, "office spread"),
            vec!["LibreOffice Calc"]
        );
        assert_eq!(
            ranked(&candidates, "  LibreOffice   Writer "),
            vec!["LibreOffice Writer"]
        );
    }

    #[test]
    fn highlights_cover_the_matched_characters() {
        assert_eq!(highlight("Terminal", "term"), "[Term]inal");
        assert_eq!(highlight("Web Browser", "bro"), "Web [Bro]wser");
        assert_eq!(highlight("Determinator", "term"), "De[term]inator");
        assert_eq!(highlight("Text Editor", "te"), "[Te]xt Editor");
        assert_eq!(highlight("Files", ""), "Files");
        assert_eq!(highlight("Files", "xyz"), "(no match)");
    }

    #[test]
    fn highlights_follow_multibyte_and_folded_characters() {
        assert_eq!(highlight("Café Écran", "cafe e"), "[Café É]cran");
        assert_eq!(highlight("Écran", "ec"), "[Éc]ran");
        assert_eq!(highlight("Straße", "strass"), "[Straß]e");
        assert_eq!(highlight("Straße", "ss"), "Stra[ß]e");
        assert_eq!(highlight("Cafe\u{301} Bar", "cafe"), "[Cafe\u{301}] Bar");
        assert_eq!(highlight("日本語入力", "語入"), "日本[語入]力");
    }

    #[test]
    fn fuzzy_highlights_prefer_word_starts_and_runs() {
        assert_eq!(
            highlight("LibreOffice Writer", "low"),
            "[L]ibre[O]ffice [W]riter"
        );
        assert_eq!(
            highlight("Visual Studio Code", "vsc"),
            "[V]isual [S]tudio [C]ode"
        );
        assert_eq!(highlight("Firefox", "ffx"), "[F]ire[f]o[x]");
        assert_eq!(highlight("Text Editor", "txed"), "[T]e[x]t [Ed]itor");
    }

    #[test]
    fn several_words_highlight_each_and_merge_overlaps() {
        assert_eq!(
            highlight("LibreOffice Writer", "writer libre"),
            "[Libre]Office [Writer]"
        );
        assert_eq!(highlight("Terminal", "term ter"), "[Term]inal");
        assert_eq!(highlight("Web Browser", "web browser"), "[Web Browser]");
    }

    #[test]
    fn matches_in_other_fields_highlight_nothing() {
        let candidates = [Candidate::new("Firefox").keywords(["browser"])];
        let hits = rank(&candidates, &Query::new("brow"), TieBreak::Alphabetical);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].tier, Tier::KeywordPrefix);
        assert!(hits[0].ranges.is_empty());
    }

    #[test]
    fn tighter_fuzzy_matches_rank_first() {
        let candidates = names(&["Game Map", "Gimp"]);
        assert_eq!(ranked(&candidates, "gmp"), vec!["Gimp", "Game Map"]);
    }

    #[test]
    fn very_long_and_strange_input_is_safe() {
        let long = "a".repeat(5000);
        let candidates = vec![
            Candidate::new(long.clone()),
            Candidate::new(""),
            Candidate::new("\u{301}\u{302}"),
            Candidate::new("ok"),
        ];
        assert_eq!(
            rank(&candidates, &Query::new(&long), TieBreak::Alphabetical).len(),
            1
        );
        assert_eq!(
            rank(&candidates, &Query::new("aXa"), TieBreak::Alphabetical).len(),
            0
        );
        assert_eq!(
            rank(&candidates, &Query::new("\u{301}"), TieBreak::Alphabetical).len(),
            4
        );
    }
}
