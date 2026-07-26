use std::cmp::Ordering;

use strsim::osa_distance;

use crate::model::{Candidate, ManagerKind, MatchKind};

const MAX_QUERY_CHARS: usize = 128;
const MAX_NAME_CHARS: usize = 256;
const MAX_DESCRIPTION_CHARS: usize = 1_024;
const MAX_TOKENS: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MatchAssessment {
    pub kind: MatchKind,
    pub score: u16,
    pub edit_distance: Option<usize>,
}

impl MatchAssessment {
    const fn new(kind: MatchKind, score: u16, edit_distance: Option<usize>) -> Self {
        Self {
            kind,
            score,
            edit_distance,
        }
    }
}

pub fn assess(query: &str, candidate: &Candidate) -> MatchAssessment {
    if exact_identity(query, &candidate.package)
        || (candidate.manager != ManagerKind::Pacman
            && exact_identity(query, &candidate.match_name))
    {
        let score = if query == candidate.package || query == candidate.match_name {
            1_000
        } else {
            995
        };
        return MatchAssessment::new(MatchKind::Exact, score, Some(0));
    }

    if canonical_identity_matches(query, candidate) {
        return MatchAssessment::new(MatchKind::CanonicalExact, 980, Some(0));
    }

    let query_name = Normalized::new(query, MAX_QUERY_CHARS);
    let package_name = Normalized::new(&candidate.match_name, MAX_NAME_CHARS);
    if query_name.compact.is_empty() || package_name.compact.is_empty() {
        return description_assessment(&query_name, candidate.description.as_deref());
    }

    if query_name.joined == package_name.joined {
        return MatchAssessment::new(MatchKind::NormalizedExact, 940, Some(0));
    }
    if query_name.compact == package_name.compact {
        return MatchAssessment::new(MatchKind::CompactExact, 920, Some(0));
    }

    if package_name.joined.starts_with(&query_name.joined)
        || package_name.compact.starts_with(&query_name.compact)
    {
        let score = 850 + coverage(&query_name.compact, &package_name.compact, 49);
        return MatchAssessment::new(MatchKind::Prefix, score, None);
    }

    if let Some((ordered, matched)) = token_match(&query_name.tokens, &package_name.tokens) {
        let base = if ordered { 800 } else { 770 };
        let span = if ordered { 39 } else { 29 };
        let score = base + ((matched * span) / query_name.tokens.len().max(1)) as u16;
        return MatchAssessment::new(MatchKind::Tokens, score, None);
    }

    if package_name.compact.contains(&query_name.compact) {
        let score = 700 + coverage(&query_name.compact, &package_name.compact, 59);
        return MatchAssessment::new(MatchKind::Contains, score, None);
    }

    if let Some(distance) = bounded_edit_distance(&query_name, &package_name) {
        let length_gap = query_name
            .compact
            .chars()
            .count()
            .abs_diff(package_name.compact.chars().count());
        let penalty = distance.saturating_mul(35) + length_gap.saturating_mul(2);
        let score = 679_u16.saturating_sub(penalty.min(119) as u16).max(560);
        return MatchAssessment::new(MatchKind::Edit, score, Some(distance));
    }

    description_assessment(&query_name, candidate.description.as_deref())
}

pub fn apply(query: &str, candidate: &mut Candidate) {
    let assessment = assess(query, candidate);
    candidate.match_kind = assessment.kind;
    candidate.score = assessment.score;
}

pub fn sort_candidates(candidates: &mut [Candidate]) {
    candidates.sort_by(compare_candidates);
}

pub fn compare_candidates(a: &Candidate, b: &Candidate) -> Ordering {
    a.query
        .cmp(&b.query)
        .then_with(|| b.match_kind.cmp(&a.match_kind))
        .then_with(|| b.score.cmp(&a.score))
        .then_with(|| b.verified.cmp(&a.verified))
        .then_with(|| a.manager.cmp(&b.manager))
        .then_with(|| a.manager_instance_id.cmp(&b.manager_instance_id))
        .then_with(|| a.package.cmp(&b.package))
        .then_with(|| a.version.cmp(&b.version))
        .then_with(|| a.source.cmp(&b.source))
}

pub fn is_unattended_exact(candidate: &Candidate) -> bool {
    candidate.verified && is_identity_exact(candidate)
}

pub fn is_identity_exact(candidate: &Candidate) -> bool {
    matches!(
        candidate.match_kind,
        MatchKind::Exact | MatchKind::CanonicalExact
    )
}

pub fn fallback_queries(query: &str) -> Vec<String> {
    let normalized = Normalized::new(query, MAX_QUERY_CHARS);
    let mut terms = Vec::new();

    if normalized.tokens.len() > 1 {
        let mut tokens = normalized
            .tokens
            .iter()
            .filter(|token| token.chars().count() >= 3)
            .cloned()
            .collect::<Vec<_>>();
        tokens.sort_by_key(|token| std::cmp::Reverse(token.chars().count()));
        for token in tokens {
            push_unique_term(&mut terms, token, &normalized.compact);
            if terms.len() == 3 {
                return terms;
            }
        }
    }

    let characters = normalized.compact.chars().collect::<Vec<_>>();
    let gram_length = if characters.len() >= 9 { 4 } else { 3 };
    if characters.len() > gram_length {
        let last = characters.len() - gram_length;
        for start in [0, last / 2, last] {
            let term = characters[start..start + gram_length]
                .iter()
                .collect::<String>();
            push_unique_term(&mut terms, term, &normalized.compact);
            if terms.len() == 3 {
                break;
            }
        }
    }
    terms
}

fn push_unique_term(terms: &mut Vec<String>, term: String, original: &str) {
    if term != original && term.chars().count() >= 3 && !terms.iter().any(|known| known == &term) {
        terms.push(term);
    }
}

fn exact_identity(a: &str, b: &str) -> bool {
    a == b || (a.is_ascii() && b.is_ascii() && a.eq_ignore_ascii_case(b))
}

fn canonical_identity_matches(query: &str, candidate: &Candidate) -> bool {
    match candidate.manager {
        ManagerKind::Pip | ManagerKind::Uv => {
            let query = pypi_identity(query);
            !query.is_empty() && query == pypi_identity(&candidate.match_name)
        }
        ManagerKind::Pacman => exact_identity(query, &candidate.match_name),
        _ => false,
    }
}

fn pypi_identity(value: &str) -> String {
    let mut result = String::new();
    let mut separator = false;
    for ch in value.chars().take(MAX_NAME_CHARS) {
        if matches!(ch, '-' | '_' | '.') {
            if !separator {
                result.push('-');
                separator = true;
            }
        } else if ch.is_ascii_alphanumeric() {
            result.push(ch.to_ascii_lowercase());
            separator = false;
        } else {
            return String::new();
        }
    }
    result
}

fn coverage(needle: &str, haystack: &str, span: u16) -> u16 {
    let needle = needle.chars().count();
    let haystack = haystack.chars().count().max(1);
    ((needle.min(haystack) * usize::from(span)) / haystack) as u16
}

fn token_match(query: &[String], package: &[String]) -> Option<(bool, usize)> {
    if query.is_empty() || package.is_empty() {
        return None;
    }

    let mut next = 0;
    let mut matched = 0;
    for query_token in query {
        let Some(offset) = package[next..]
            .iter()
            .position(|name| name.starts_with(query_token))
        else {
            break;
        };
        matched += 1;
        next += offset + 1;
    }
    if matched == query.len() {
        return Some((true, matched));
    }

    let mut used = vec![false; package.len()];
    for query_token in query {
        let index = package
            .iter()
            .enumerate()
            .position(|(index, name)| !used[index] && name.starts_with(query_token))?;
        used[index] = true;
    }
    Some((false, query.len()))
}

fn bounded_edit_distance(query: &Normalized, package: &Normalized) -> Option<usize> {
    let query_length = query.compact.chars().count();
    if query_length < 4 {
        return None;
    }
    let maximum = match query_length {
        4..=6 => 1,
        7..=12 => 2,
        _ => 3,
    };

    std::iter::once(package.compact.as_str())
        .chain(package.tokens.iter().map(String::as_str))
        .filter(|name| query_length.abs_diff(name.chars().count()) <= maximum)
        .map(|name| osa_distance(&query.compact, name))
        .filter(|distance| *distance <= maximum)
        .min()
}

fn description_assessment(query: &Normalized, description: Option<&str>) -> MatchAssessment {
    let Some(description) = description else {
        return MatchAssessment::new(MatchKind::None, 0, None);
    };
    if query.tokens.is_empty() {
        return MatchAssessment::new(MatchKind::None, 0, None);
    }

    let description = Normalized::new(description, MAX_DESCRIPTION_CHARS);
    if description.joined.contains(&query.joined) || description.compact.contains(&query.compact) {
        return MatchAssessment::new(MatchKind::Description, 260, None);
    }

    let matched = query
        .tokens
        .iter()
        .filter(|query_token| {
            description
                .tokens
                .iter()
                .any(|token| token.starts_with(query_token.as_str()))
        })
        .count();
    if matched == 0 {
        return MatchAssessment::new(MatchKind::None, 0, None);
    }
    let score = if matched == query.tokens.len() {
        220 + ((matched * 29) / query.tokens.len()) as u16
    } else {
        100 + ((matched * 99) / query.tokens.len()) as u16
    };
    MatchAssessment::new(MatchKind::Description, score.min(299), None)
}

#[derive(Debug)]
struct Normalized {
    tokens: Vec<String>,
    joined: String,
    compact: String,
}

impl Normalized {
    fn new(value: &str, limit: usize) -> Self {
        let mut tokens = Vec::new();
        let mut current = String::new();
        let mut previous_class = CharacterClass::Boundary;

        for original in value.chars().take(limit) {
            let ch = fold_fullwidth(original);
            let class = CharacterClass::of(ch);
            let camel_boundary = matches!(
                (previous_class, class),
                (CharacterClass::Lower, CharacterClass::Upper)
                    | (CharacterClass::Letter, CharacterClass::Digit)
                    | (CharacterClass::Lower, CharacterClass::Digit)
                    | (CharacterClass::Upper, CharacterClass::Digit)
                    | (CharacterClass::Digit, CharacterClass::Letter)
                    | (CharacterClass::Digit, CharacterClass::Lower)
                    | (CharacterClass::Digit, CharacterClass::Upper)
            );
            if class == CharacterClass::Boundary || camel_boundary {
                finish_token(&mut tokens, &mut current);
            }
            if class != CharacterClass::Boundary {
                current.extend(ch.to_lowercase());
            }
            previous_class = class;
            if tokens.len() == MAX_TOKENS {
                break;
            }
        }
        finish_token(&mut tokens, &mut current);
        tokens.truncate(MAX_TOKENS);
        let joined = tokens.join("-");
        let compact = tokens.concat();
        Self {
            tokens,
            joined,
            compact,
        }
    }
}

fn finish_token(tokens: &mut Vec<String>, current: &mut String) {
    if !current.is_empty() && tokens.len() < MAX_TOKENS {
        tokens.push(std::mem::take(current));
    } else {
        current.clear();
    }
}

fn fold_fullwidth(ch: char) -> char {
    match ch {
        '\u{3000}' => ' ',
        '\u{ff01}'..='\u{ff5e}' => char::from_u32(ch as u32 - 0xfee0).unwrap_or(ch),
        _ => ch,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CharacterClass {
    Boundary,
    Letter,
    Lower,
    Upper,
    Digit,
}

impl CharacterClass {
    fn of(ch: char) -> Self {
        if ch.is_numeric() {
            Self::Digit
        } else if ch.is_lowercase() {
            Self::Lower
        } else if ch.is_uppercase() {
            Self::Upper
        } else if ch.is_alphabetic() {
            Self::Letter
        } else {
            Self::Boundary
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{apply, assess, fallback_queries, sort_candidates};
    use crate::model::{Candidate, ManagerKind, MatchKind};

    fn candidate(manager: ManagerKind, package: &str, match_name: &str) -> Candidate {
        Candidate {
            query: String::new(),
            package: package.into(),
            match_name: match_name.into(),
            manager_instance_id: format!("{manager}:test"),
            manager,
            source: "test".into(),
            version: Some("1.0.0".into()),
            description: None,
            score: 0,
            match_kind: MatchKind::None,
            verified: true,
        }
    }

    #[test]
    fn distinguishes_exact_and_case_folded_exact_matches() {
        let package = candidate(ManagerKind::Apt, "ripgrep", "ripgrep");
        assert_eq!(assess("ripgrep", &package).score, 1_000);
        let folded = assess("RIPGREP", &package);
        assert_eq!(folded.kind, MatchKind::Exact);
        assert_eq!(folded.score, 995);
    }

    #[test]
    fn recognizes_manager_specific_canonical_identities() {
        let pypi = candidate(ManagerKind::Pip, "my-pkg-name", "my-pkg-name");
        assert_eq!(assess("My_Pkg.Name", &pypi).kind, MatchKind::CanonicalExact);

        let pacman = candidate(ManagerKind::Pacman, "extra/ripgrep", "ripgrep");
        assert_eq!(assess("ripgrep", &pacman).kind, MatchKind::CanonicalExact);
    }

    #[test]
    fn does_not_treat_an_npm_scope_leaf_as_exact() {
        let package = candidate(ManagerKind::Npm, "@types/node", "@types/node");
        assert!(!matches!(
            assess("node", &package).kind,
            MatchKind::Exact | MatchKind::CanonicalExact
        ));
    }

    #[test]
    fn ranks_separators_tokens_and_transpositions() {
        let separated = candidate(ManagerKind::Cargo, "serde_json", "serde_json");
        assert_eq!(
            assess("serde json", &separated).kind,
            MatchKind::NormalizedExact
        );

        let typo = candidate(ManagerKind::Apt, "ripgrep", "ripgrep");
        let assessment = assess("ripgrpe", &typo);
        assert_eq!(assessment.kind, MatchKind::Edit);
        assert_eq!(assessment.edit_distance, Some(1));
    }

    #[test]
    fn short_names_do_not_use_edit_distance() {
        let package = candidate(ManagerKind::Apt, "gr", "gr");
        assert_ne!(assess("rg", &package).kind, MatchKind::Edit);
    }

    #[test]
    fn unicode_confusables_are_not_exact() {
        let package = candidate(ManagerKind::Npm, "package", "package");
        assert!(!matches!(
            assess("p\u{430}ckage", &package).kind,
            MatchKind::Exact | MatchKind::CanonicalExact
        ));

        let kelvin = candidate(ManagerKind::Npm, "k", "k");
        assert!(!matches!(
            assess("\u{212a}", &kelvin).kind,
            MatchKind::Exact | MatchKind::CanonicalExact
        ));
    }

    #[test]
    fn description_evidence_stays_below_name_evidence() {
        let mut by_description = candidate(ManagerKind::Pacman, "other", "other");
        by_description.description = Some("recursive ripgrep-compatible search tool".into());
        let description = assess("ripgrep", &by_description);
        assert_eq!(description.kind, MatchKind::Description);

        let prefix = candidate(ManagerKind::Pacman, "extra/ripgrep-all", "ripgrep-all");
        assert!(assess("ripgrep", &prefix).kind > description.kind);
    }

    #[test]
    fn fallback_queries_are_bounded_and_deterministic() {
        assert_eq!(fallback_queries("ripgrpe"), ["rip", "pgr", "rpe"]);
        assert!(fallback_queries("json parser").len() <= 3);
        assert!(fallback_queries("rg").is_empty());
    }

    #[test]
    fn sorting_is_deterministic_after_relevance() {
        let mut first = candidate(ManagerKind::Apt, "ripgrep-all", "ripgrep-all");
        first.query = "ripgrep".into();
        apply("ripgrep", &mut first);
        let mut exact = candidate(ManagerKind::Apt, "ripgrep", "ripgrep");
        exact.query = "ripgrep".into();
        apply("ripgrep", &mut exact);

        let mut candidates = vec![first.clone(), exact.clone()];
        sort_candidates(&mut candidates);
        assert_eq!(candidates[0].package, "ripgrep");

        candidates.reverse();
        sort_candidates(&mut candidates);
        assert_eq!(candidates[0].package, "ripgrep");
    }

    #[test]
    fn matching_caps_oversized_names_and_descriptions() {
        let mut package = candidate(ManagerKind::Apt, &"x".repeat(10_000), &"x".repeat(10_000));
        package.description = Some("search ".repeat(10_000));
        let _ = assess(&"y".repeat(1_000), &package);
    }
}
