use std::collections::{BTreeSet, HashMap, HashSet};

use serde::Serialize;
use url::Url;

use super::domain::SearchHit;

const MIN_CANDIDATE_SCORE: f64 = 24.0;
const MIN_CONTENT_SCORE: f64 = 28.0;
const PASSAGE_CHARS: usize = 1_600;
const PASSAGE_STEP_CHARS: usize = 1_200;
const MAX_EXCERPT_CHARS: usize = 4_000;

#[derive(Debug, Clone)]
pub(crate) struct RankedCandidate {
    pub hit: SearchHit,
    pub normalized_url: String,
    pub domain: String,
    pub matched_queries: Vec<String>,
    pub search_engines: Vec<String>,
    pub score: f64,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct CandidateRejection {
    pub url: String,
    pub stage: &'static str,
    pub reason: String,
    pub score: f64,
}

#[derive(Debug, Clone)]
pub(crate) struct CandidateRanking {
    pub candidates: Vec<RankedCandidate>,
    pub rejected: Vec<CandidateRejection>,
}

#[derive(Debug, Clone)]
pub(crate) struct ContentAssessment {
    pub accepted: bool,
    pub reason: Option<&'static str>,
    pub relevance_score: f64,
    pub excerpt: String,
    pub verification_quote: String,
    pub matched_query: String,
}

#[derive(Debug)]
struct CandidateAccumulator {
    hit: SearchHit,
    normalized_url: String,
    domain: String,
    query_indices: BTreeSet<usize>,
    engines: BTreeSet<String>,
    retrieval_score: f64,
    best_preview_score: f64,
    best_preview_matches: usize,
    best_query_terms: usize,
}

pub(crate) fn categories_for_query(query: &str) -> Option<&'static str> {
    let tokens = tokenize(query).into_iter().collect::<HashSet<_>>();
    let academic = [
        "academic",
        "arxiv",
        "citation",
        "doi",
        "journal",
        "paper",
        "research",
        "study",
        "systematic",
    ]
    .iter()
    .any(|token| tokens.contains(*token));
    let technical = [
        "api",
        "code",
        "crate",
        "developer",
        "documentation",
        "github",
        "protocol",
        "rust",
        "sdk",
        "software",
        "specification",
    ]
    .iter()
    .any(|token| tokens.contains(*token));
    match (academic, technical) {
        (true, true) => Some("general,science,it"),
        (true, false) => Some("general,science"),
        (false, true) => Some("general,it"),
        (false, false) => Some("general"),
    }
}

pub(crate) fn rank_candidates(queries: &[String], searches: &[Vec<SearchHit>]) -> CandidateRanking {
    let mut accumulated: HashMap<String, CandidateAccumulator> = HashMap::new();
    let mut rejected = Vec::new();

    for (query_index, hits) in searches.iter().enumerate() {
        let Some(query) = queries.get(query_index) else {
            continue;
        };
        for hit in hits {
            let (normalized_url, domain) = match normalize_url(&hit.url) {
                Ok(value) => value,
                Err(reason) => {
                    rejected.push(CandidateRejection {
                        url: hit.url.clone(),
                        stage: "candidate",
                        reason,
                        score: 0.0,
                    });
                    continue;
                }
            };
            if let Some(reason) = unsafe_candidate_reason(&hit.url) {
                rejected.push(CandidateRejection {
                    url: hit.url.clone(),
                    stage: "candidate",
                    reason: reason.to_string(),
                    score: 0.0,
                });
                continue;
            }
            let (preview_score, preview_matches, query_terms) = preview_relevance(query, hit);
            let rank_score = 18.0 / (hit.rank.max(1) as f64).sqrt();
            let upstream_score = hit.score.unwrap_or_default().max(0.0).ln_1p().min(3.0) * 2.0;
            let entry = accumulated
                .entry(normalized_url.clone())
                .or_insert_with(|| CandidateAccumulator {
                    hit: hit.clone(),
                    normalized_url,
                    domain,
                    query_indices: BTreeSet::new(),
                    engines: BTreeSet::new(),
                    retrieval_score: 0.0,
                    best_preview_score: 0.0,
                    best_preview_matches: 0,
                    best_query_terms: 0,
                });
            entry.query_indices.insert(query_index);
            entry.retrieval_score += rank_score + upstream_score;
            if preview_score > entry.best_preview_score {
                entry.best_preview_score = preview_score;
                entry.best_preview_matches = preview_matches;
                entry.best_query_terms = query_terms;
            }
            if hit.snippet.len() > entry.hit.snippet.len() {
                entry.hit = hit.clone();
            }
            if hit.engines.is_empty() {
                if !hit.engine.is_empty() {
                    entry.engines.insert(hit.engine.clone());
                }
            } else {
                entry.engines.extend(hit.engines.iter().cloned());
            }
        }
    }

    let mut candidates = Vec::new();
    for entry in accumulated.into_values() {
        if entry.best_query_terms >= 4 && entry.best_preview_matches < 2 {
            rejected.push(CandidateRejection {
                url: entry.hit.url,
                stage: "candidate",
                reason: "insufficient_preview_concepts".to_string(),
                score: rounded(entry.best_preview_score),
            });
            continue;
        }
        let mut score = entry.best_preview_score
            + entry.retrieval_score.min(28.0)
            + ((entry.query_indices.len().saturating_sub(1)) as f64 * 5.0)
            + ((entry.engines.len().saturating_sub(1)) as f64 * 2.0);
        if is_homepage(&entry.hit.url) {
            score -= 10.0;
        }
        if is_low_signal_domain(&entry.domain) {
            score -= 12.0;
        }
        if score < MIN_CANDIDATE_SCORE {
            rejected.push(CandidateRejection {
                url: entry.hit.url,
                stage: "candidate",
                reason: "low_preview_relevance".to_string(),
                score: rounded(score),
            });
            continue;
        }
        candidates.push(RankedCandidate {
            hit: entry.hit,
            normalized_url: entry.normalized_url,
            domain: entry.domain,
            matched_queries: entry
                .query_indices
                .into_iter()
                .filter_map(|index| queries.get(index).cloned())
                .collect(),
            search_engines: entry.engines.into_iter().collect(),
            score: rounded(score),
        });
    }
    candidates.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.normalized_url.cmp(&right.normalized_url))
    });

    let mut accepted_titles: Vec<(String, HashSet<String>)> = Vec::new();
    candidates.retain(|candidate| {
        let title = tokenize(&candidate.hit.title)
            .into_iter()
            .collect::<HashSet<_>>();
        if title.len() >= 3
            && accepted_titles.iter().any(|(domain, accepted)| {
                let similarity = title_similarity(&title, accepted);
                (domain == &candidate.domain || domains_share_brand(domain, &candidate.domain))
                    && similarity >= 0.8
            })
        {
            rejected.push(CandidateRejection {
                url: candidate.hit.url.clone(),
                stage: "candidate",
                reason: "near_duplicate_title".to_string(),
                score: candidate.score,
            });
            false
        } else {
            accepted_titles.push((candidate.domain.clone(), title));
            true
        }
    });

    // One host cannot consume the entire bounded scrape budget. Two pages from
    // a host are still allowed because an official overview and a detailed
    // technical page are often independently useful.
    let mut domain_counts: HashMap<String, usize> = HashMap::new();
    candidates.retain(|candidate| {
        let count = domain_counts.entry(candidate.domain.clone()).or_default();
        if *count >= 2 {
            rejected.push(CandidateRejection {
                url: candidate.hit.url.clone(),
                stage: "candidate",
                reason: "domain_diversity_cap".to_string(),
                score: candidate.score,
            });
            false
        } else {
            *count += 1;
            true
        }
    });
    let candidates = diversify_by_query(queries, candidates);

    CandidateRanking {
        candidates,
        rejected,
    }
}

fn diversify_by_query(
    queries: &[String],
    mut candidates: Vec<RankedCandidate>,
) -> Vec<RankedCandidate> {
    let mut ordered = Vec::with_capacity(candidates.len());
    loop {
        let before = ordered.len();
        for query in queries {
            let Some(position) = candidates
                .iter()
                .position(|candidate| candidate.matched_queries.contains(query))
            else {
                continue;
            };
            ordered.push(candidates.remove(position));
        }
        if ordered.len() == before {
            break;
        }
    }
    // Defensive fallback for future candidate sources without query
    // attribution. Keep their existing score order instead of dropping them.
    ordered.extend(candidates);
    ordered
}

pub(crate) fn assess_content(
    markdown: &str,
    final_url: &str,
    title: &str,
    queries: &[String],
) -> ContentAssessment {
    let body = unwrap_untrusted(markdown).trim();
    if body.chars().count() < 300 || tokenize(body).len() < 60 {
        return rejected_content("content_too_short");
    }
    if unsafe_candidate_reason(final_url).is_some() || looks_like_login(body) {
        return rejected_content("authentication_or_interstitial");
    }

    let windows = passage_windows(body);
    let mut scored = Vec::new();
    for query in queries {
        for (start, end) in &windows {
            let passage = &body[*start..*end];
            let (score, matched_terms) = text_relevance(query, passage);
            scored.push((score, matched_terms, query.as_str(), *start, *end));
        }
    }
    scored.sort_by(|left, right| right.0.total_cmp(&left.0));
    let Some((best_score, best_matches, best_query, _, _)) = scored.first().copied() else {
        return rejected_content("no_relevant_passage");
    };
    let required_matches = if tokenize(best_query).len() >= 4 {
        2
    } else {
        1
    };
    if best_score < MIN_CONTENT_SCORE || best_matches < required_matches {
        return ContentAssessment {
            accepted: false,
            reason: Some("low_content_relevance"),
            relevance_score: rounded(best_score),
            excerpt: String::new(),
            verification_quote: String::new(),
            matched_query: best_query.to_string(),
        };
    }

    let title_terms = tokenize(title).into_iter().collect::<HashSet<_>>();
    let body_terms = tokenize(body).into_iter().collect::<HashSet<_>>();
    let title_match =
        title_terms.is_empty() || title_terms.iter().any(|term| body_terms.contains(term));
    if !title_match && best_score < 55.0 {
        return ContentAssessment {
            accepted: false,
            reason: Some("title_content_mismatch"),
            relevance_score: rounded(best_score),
            excerpt: String::new(),
            verification_quote: String::new(),
            matched_query: best_query.to_string(),
        };
    }

    let mut selected_ranges = Vec::new();
    let mut excerpt_parts = Vec::new();
    let mut excerpt_chars = 0usize;
    for (_, _, query, start, end) in scored {
        if query != best_query
            || selected_ranges
                .iter()
                .any(|(selected_start, selected_end)| {
                    start < *selected_end && end > *selected_start
                })
        {
            continue;
        }
        let passage = body[start..end].trim();
        if passage.is_empty() {
            continue;
        }
        let remaining = MAX_EXCERPT_CHARS.saturating_sub(excerpt_chars);
        if remaining == 0 {
            break;
        }
        let part = take_chars(passage, remaining);
        excerpt_chars += part.chars().count();
        excerpt_parts.push(part);
        selected_ranges.push((start, end));
        if excerpt_parts.len() >= 3 {
            break;
        }
    }
    let excerpt = excerpt_parts.join("\n\n---\n\n");
    let verification_quote = verification_quote(&excerpt);
    ContentAssessment {
        accepted: !excerpt.is_empty() && !verification_quote.is_empty(),
        reason: None,
        relevance_score: rounded(best_score),
        excerpt,
        verification_quote,
        matched_query: best_query.to_string(),
    }
}

fn rejected_content(reason: &'static str) -> ContentAssessment {
    ContentAssessment {
        accepted: false,
        reason: Some(reason),
        relevance_score: 0.0,
        excerpt: String::new(),
        verification_quote: String::new(),
        matched_query: String::new(),
    }
}

fn preview_relevance(query: &str, hit: &SearchHit) -> (f64, usize, usize) {
    let query_terms = tokenize(query).into_iter().collect::<HashSet<_>>();
    if query_terms.is_empty() {
        return (0.0, 0, 0);
    }
    let title = tokenize(&hit.title).into_iter().collect::<HashSet<_>>();
    let snippet = tokenize(&hit.snippet).into_iter().collect::<HashSet<_>>();
    let url = tokenize(&hit.url).into_iter().collect::<HashSet<_>>();
    let preview_terms = title
        .union(&snippet)
        .cloned()
        .collect::<HashSet<_>>()
        .union(&url)
        .cloned()
        .collect::<HashSet<_>>();
    let matched = query_terms.intersection(&preview_terms).count();
    let score = coverage(&query_terms, &title) * 42.0
        + coverage(&query_terms, &snippet) * 30.0
        + coverage(&query_terms, &url) * 12.0;
    (score, matched, query_terms.len())
}

fn text_relevance(query: &str, text: &str) -> (f64, usize) {
    let query_terms = tokenize(query).into_iter().collect::<HashSet<_>>();
    let text_tokens = tokenize(text);
    if query_terms.is_empty() || text_tokens.is_empty() {
        return (0.0, 0);
    }
    let text_terms = text_tokens.iter().cloned().collect::<HashSet<_>>();
    let matched = query_terms
        .iter()
        .filter(|term| text_terms.contains(*term))
        .count();
    let coverage = matched as f64 / query_terms.len() as f64;
    let density = (text_tokens
        .iter()
        .filter(|term| query_terms.contains(*term))
        .count() as f64
        / text_tokens.len() as f64)
        .min(0.08)
        / 0.08;
    (coverage * 78.0 + density * 22.0, matched)
}

fn coverage(query: &HashSet<String>, document: &HashSet<String>) -> f64 {
    query.iter().filter(|term| document.contains(*term)).count() as f64 / query.len() as f64
}

fn jaccard(left: &HashSet<String>, right: &HashSet<String>) -> f64 {
    let union = left.union(right).count();
    if union == 0 {
        return 0.0;
    }
    left.intersection(right).count() as f64 / union as f64
}

fn title_similarity(left: &HashSet<String>, right: &HashSet<String>) -> f64 {
    let smaller = left.len().min(right.len());
    if smaller == 0 {
        return 0.0;
    }
    // Publishers commonly append a journal, site name, or format marker to the
    // same article title. The overlap coefficient catches that containment
    // relationship while the minimum title length above avoids generic titles.
    let containment = left.intersection(right).count() as f64 / smaller as f64;
    containment.max(jaccard(left, right))
}

fn domains_share_brand(left: &str, right: &str) -> bool {
    let brand_terms = |domain: &str| {
        domain
            .split('.')
            .filter(|label| {
                !matches!(
                    *label,
                    "blog"
                        | "co"
                        | "com"
                        | "docs"
                        | "edu"
                        | "info"
                        | "io"
                        | "net"
                        | "org"
                        | "research"
                        | "www"
                )
            })
            .map(str::to_string)
            .collect::<HashSet<_>>()
    };
    let left = brand_terms(left);
    let right = brand_terms(right);
    !left.is_disjoint(&right)
}

fn normalize_url(raw: &str) -> Result<(String, String), String> {
    let mut url = Url::parse(raw).map_err(|_| "invalid_url".to_string())?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("unsupported_url_scheme".to_string());
    }
    let host = url
        .host_str()
        .ok_or_else(|| "missing_url_host".to_string())?
        .to_ascii_lowercase();
    let normalized_host = host
        .strip_prefix("www.")
        .or_else(|| host.strip_prefix("m."))
        .or_else(|| host.strip_prefix("amp."))
        .unwrap_or(&host)
        .to_string();
    url.set_fragment(None);
    let mut pairs = url
        .query_pairs()
        .filter(|(key, _)| !is_tracking_parameter(key))
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect::<Vec<_>>();
    pairs.sort();
    url.set_query(None);
    if !pairs.is_empty() {
        url.query_pairs_mut().extend_pairs(pairs);
    }
    if url.path() != "/" {
        let trimmed = url.path().trim_end_matches('/').to_string();
        url.set_path(&trimmed);
    }
    let port = url
        .port()
        .map(|value| format!(":{value}"))
        .unwrap_or_default();
    let query = url
        .query()
        .map(|value| format!("?{value}"))
        .unwrap_or_default();
    Ok((
        format!(
            "{}://{}{}{}{}",
            url.scheme(),
            normalized_host,
            port,
            url.path(),
            query
        ),
        normalized_host,
    ))
}

fn is_tracking_parameter(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    key.starts_with("utm_")
        || matches!(
            key.as_str(),
            "fbclid" | "gclid" | "dclid" | "msclkid" | "mc_cid" | "mc_eid" | "ref_src"
        )
}

fn unsafe_candidate_reason(raw: &str) -> Option<&'static str> {
    let url = Url::parse(raw).ok()?;
    let path = url.path().to_ascii_lowercase();
    let segments = path.split('/').filter(|segment| !segment.is_empty());
    if segments.clone().any(|segment| {
        matches!(
            segment,
            "auth" | "authorize" | "login" | "oauth" | "signin" | "sign-in" | "signup" | "sign-up"
        )
    }) {
        return Some("authentication_url");
    }
    if url
        .query_pairs()
        .any(|(key, _)| matches!(key.as_ref(), "redirect_uri" | "return_to" | "continue"))
    {
        return Some("redirect_url");
    }
    None
}

fn is_homepage(raw: &str) -> bool {
    Url::parse(raw)
        .map(|url| url.path().trim_matches('/').is_empty() && url.query().is_none())
        .unwrap_or(false)
}

fn is_low_signal_domain(domain: &str) -> bool {
    [
        "coinmarketcap.com",
        "facebook.com",
        "instagram.com",
        "pinterest.com",
        "tiktok.com",
        "youtube.com",
    ]
    .iter()
    .any(|blocked| domain == *blocked || domain.ends_with(&format!(".{blocked}")))
}

fn looks_like_login(body: &str) -> bool {
    let prefix = body
        .chars()
        .take(2_000)
        .collect::<String>()
        .to_ascii_lowercase();
    (prefix.contains("sign in") || prefix.contains("log in"))
        && (prefix.contains("password")
            || prefix.contains("forgot password")
            || prefix.contains("create account"))
}

fn unwrap_untrusted(markdown: &str) -> &str {
    if !markdown.starts_with("<<<UNTRUSTED_WEB_CONTENT:") {
        return markdown;
    }
    let Some((_, body)) = markdown.split_once('\n') else {
        return markdown;
    };
    body.rsplit_once("\n<<<END_UNTRUSTED_WEB_CONTENT:")
        .map(|(content, _)| content)
        .unwrap_or(body)
}

fn passage_windows(body: &str) -> Vec<(usize, usize)> {
    let mut boundaries = body
        .char_indices()
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    boundaries.push(body.len());
    let char_count = boundaries.len().saturating_sub(1);
    if char_count <= PASSAGE_CHARS {
        return vec![(0, body.len())];
    }
    let mut windows = Vec::new();
    let mut start_char = 0usize;
    while start_char < char_count {
        let end_char = (start_char + PASSAGE_CHARS).min(char_count);
        windows.push((boundaries[start_char], boundaries[end_char]));
        if end_char == char_count {
            break;
        }
        start_char += PASSAGE_STEP_CHARS;
    }
    windows
}

fn verification_quote(excerpt: &str) -> String {
    let source = excerpt
        .lines()
        .map(str::trim)
        .find(|line| line.chars().count() >= 80)
        .unwrap_or_else(|| excerpt.trim());
    let mut character_count = 0usize;
    for (index, character) in source.char_indices() {
        character_count += 1;
        if character_count > 320 {
            break;
        }
        if character_count < 80 || !matches!(character, '.' | '!' | '?') {
            continue;
        }
        let end = index + character.len_utf8();
        let is_sentence_boundary = source[end..]
            .chars()
            .next()
            .is_none_or(|next| next.is_whitespace() || next == '[');
        if is_sentence_boundary {
            return source[..end].trim_end().to_string();
        }
    }
    take_chars(source, 320).trim_end().to_string()
}

fn take_chars(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

fn tokenize(value: &str) -> Vec<String> {
    value
        .split(|character: char| !character.is_alphanumeric())
        .filter_map(|raw| {
            let token = raw.to_lowercase();
            if token.chars().count() < 2 || is_stopword(&token) {
                return None;
            }
            Some(stem(token))
        })
        .collect()
}

fn stem(mut token: String) -> String {
    if token.len() > 5 && token.ends_with("ies") {
        token.truncate(token.len() - 3);
        token.push('y');
    } else if token.len() > 4 && token.ends_with('s') && !token.ends_with("ss") {
        token.pop();
    }
    token
}

fn is_stopword(token: &str) -> bool {
    matches!(
        token,
        "about"
            | "after"
            | "also"
            | "and"
            | "are"
            | "been"
            | "before"
            | "best"
            | "but"
            | "can"
            | "does"
            | "for"
            | "from"
            | "have"
            | "how"
            | "into"
            | "its"
            | "more"
            | "most"
            | "official"
            | "primary"
            | "source"
            | "sources"
            | "than"
            | "that"
            | "the"
            | "their"
            | "this"
            | "using"
            | "what"
            | "when"
            | "where"
            | "which"
            | "with"
    )
}

fn rounded(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::*;

    #[derive(Deserialize)]
    struct RetrievalFixture {
        name: String,
        query: String,
        hits: Vec<FixtureHit>,
        required_top_domains: Vec<String>,
        rejected_urls: Vec<String>,
    }

    #[derive(Deserialize)]
    struct FixtureHit {
        rank: usize,
        url: String,
        title: String,
        snippet: String,
        engine: String,
    }

    fn hit(rank: usize, url: &str, title: &str, snippet: &str, engine: &str) -> SearchHit {
        SearchHit {
            rank,
            url: url.into(),
            title: title.into(),
            snippet: snippet.into(),
            engine: engine.into(),
            engines: vec![engine.into()],
            score: Some(1.0 / rank as f64),
            published_at: None,
        }
    }

    #[test]
    fn observed_search_quality_fixture_stays_clean() {
        let fixtures: Vec<RetrievalFixture> = serde_json::from_str(include_str!(
            "../../tests/fixtures/search_quality_observed.json"
        ))
        .unwrap();
        for fixture in fixtures {
            let hits = fixture
                .hits
                .into_iter()
                .map(|item| {
                    hit(
                        item.rank,
                        &item.url,
                        &item.title,
                        &item.snippet,
                        &item.engine,
                    )
                })
                .collect::<Vec<_>>();
            let ranked = rank_candidates(std::slice::from_ref(&fixture.query), &[hits]);
            let top_domains = ranked
                .candidates
                .iter()
                .take(fixture.required_top_domains.len())
                .map(|candidate| candidate.domain.as_str())
                .collect::<HashSet<_>>();
            for required in &fixture.required_top_domains {
                assert!(
                    top_domains.contains(required.as_str()),
                    "{}: missing {required} from top candidates",
                    fixture.name
                );
            }
            for rejected_url in &fixture.rejected_urls {
                assert!(
                    ranked
                        .candidates
                        .iter()
                        .all(|candidate| &candidate.hit.url != rejected_url),
                    "{}: accepted observed junk URL {rejected_url}",
                    fixture.name
                );
            }
        }
    }

    #[test]
    fn normalizes_tracking_and_mobile_urls_for_deduplication() {
        let (left, domain) =
            normalize_url("https://www.Example.com/article/?utm_source=newsletter&id=3#section")
                .unwrap();
        let (right, _) = normalize_url("https://m.example.com/article?id=3").unwrap();
        assert_eq!(left, "https://example.com/article?id=3");
        assert_eq!(left, right);
        assert_eq!(domain, "example.com");
    }

    #[test]
    fn ranks_relevant_papers_above_observed_junk() {
        let query = "time series momentum cryptocurrency academic study".to_string();
        let searches = vec![vec![
            hit(
                1,
                "https://time.is/",
                "Time.is - exact time, any time zone",
                "Current time worldwide",
                "bing",
            ),
            hit(
                2,
                "https://link.springer.com/article/crypto-momentum",
                "Time-series momentum and market timing in Bitcoin",
                "An empirical academic study of cryptocurrency momentum strategies",
                "privacywall",
            ),
            hit(
                3,
                "https://coinmarketcap.com/",
                "Cryptocurrency Prices and Market Capitalizations",
                "Live cryptocurrency prices",
                "bing",
            ),
            hit(
                4,
                "https://research.example.edu/bitcoin-momentum",
                "Bitcoin intraday time series momentum",
                "Research paper studying momentum effects in cryptocurrency markets",
                "fynd",
            ),
        ]];
        let ranked = rank_candidates(&[query], &searches);
        assert!(ranked.candidates.len() >= 2);
        assert!(ranked.candidates[0].hit.url.contains("springer.com"));
        assert!(ranked.candidates[1]
            .hit
            .url
            .contains("research.example.edu"));
        assert!(ranked
            .rejected
            .iter()
            .any(|item| item.url == "https://time.is/"));
    }

    #[test]
    fn rejects_login_redirects_before_fetch() {
        assert_eq!(
            unsafe_candidate_reason(
                "https://login.microsoftonline.com/oauth/authorize?redirect_uri=x"
            ),
            Some("authentication_url")
        );
    }

    #[test]
    fn rejects_near_duplicate_article_and_pdf_titles() {
        let query = "cryptocurrency momentum strategy risks counterevidence".to_string();
        let searches = vec![vec![
            hit(
                1,
                "https://link.springer.com/content/pdf/paper.pdf",
                "Cryptocurrency momentum has (not) its moments - Springer",
                "A study of cryptocurrency momentum strategy risks and counterevidence",
                "privacywall",
            ),
            hit(
                2,
                "https://link.springer.com/article/paper",
                "Cryptocurrency momentum has (not) its moments | Financial Markets",
                "Research on cryptocurrency momentum strategy risk",
                "fynd",
            ),
        ]];
        let ranked = rank_candidates(&[query], &searches);
        assert_eq!(ranked.candidates.len(), 1);
        assert!(ranked
            .rejected
            .iter()
            .any(|item| item.reason == "near_duplicate_title"));
    }

    #[test]
    fn rejects_near_identical_titles_across_mirror_domains() {
        let query = "Model Context Protocol authorization specification guidance".to_string();
        let searches = vec![vec![
            hit(
                1,
                "https://modelcontextprotocol.io/specification/authorization",
                "Authorization - Model Context Protocol",
                "Model Context Protocol authorization specification and guidance",
                "privacywall",
            ),
            hit(
                2,
                "https://modelcontextprotocol.info/specification/authorization",
                "Authorization - Model Context Protocol (MCP)",
                "A mirror of the Model Context Protocol authorization specification",
                "fynd",
            ),
        ]];
        let ranked = rank_candidates(&[query], &searches);
        assert_eq!(ranked.candidates.len(), 1);
        assert!(ranked
            .rejected
            .iter()
            .any(|item| item.reason == "near_duplicate_title"));
    }

    #[test]
    fn covers_each_planned_query_before_filling_more_slots() {
        let queries = vec![
            "Model Context Protocol authorization specification requirements".to_string(),
            "Model Context Protocol prompt injection independent analysis".to_string(),
        ];
        let searches = vec![
            vec![
                hit(
                    1,
                    "https://spec.example.org/authorization",
                    "Model Context Protocol authorization specification",
                    "Normative authorization requirements for protocol deployments",
                    "privacywall",
                ),
                hit(
                    2,
                    "https://deployment.guide.org/authorization",
                    "Model Context Protocol authorization deployment guide",
                    "Implementation guidance for authorization requirements",
                    "fynd",
                ),
            ],
            vec![hit(
                1,
                "https://security.example.org/mcp-injection",
                "Model Context Protocol prompt injection analysis",
                "Independent security analysis of prompt injection risks",
                "bing",
            )],
        ];
        let ranked = rank_candidates(&queries, &searches);
        assert_eq!(ranked.candidates.len(), 3);
        assert!(ranked.candidates[0].matched_queries.contains(&queries[0]));
        assert!(ranked.candidates[1].matched_queries.contains(&queries[1]));
        assert!(ranked.candidates[2].matched_queries.contains(&queries[0]));
    }

    #[test]
    fn extracts_query_focused_passages_and_exact_quote() {
        let noise = "Navigation products pricing contact. ".repeat(40);
        let relevant = "The Model Context Protocol specification defines tools as schema-described functions that a server exposes to a client. Tool results are returned through the protocol with explicit correlation to the originating request.";
        let body = format!("<<<UNTRUSTED_WEB_CONTENT:n source=\"x\">>>\n{noise}\n\n{relevant}\n\n{noise}\n<<<END_UNTRUSTED_WEB_CONTENT:n>>>");
        let assessment = assess_content(
            &body,
            "https://modelcontextprotocol.io/specification/tools",
            "Model Context Protocol tools specification",
            &["Model Context Protocol official specification tooling".into()],
        );
        assert!(assessment.accepted, "{:?}", assessment.reason);
        assert!(assessment.excerpt.contains("schema-described functions"));
        assert!(body.contains(&assessment.verification_quote));
        assert!(assessment.relevance_score >= MIN_CONTENT_SCORE);
    }

    #[test]
    fn exact_quote_stops_before_markdown_permalink() {
        let sentence = "This document specifies an extension to the OAuth 2.0 Authorization Framework defining request parameters that let a client identify the protected resource.";
        let line = format!(
            "{sentence}[\u{00b6}](https://www.rfc-editor.org/rfc/rfc8707.html#section-abstract)"
        );
        let quote = verification_quote(&line);
        assert_eq!(quote, sentence);
        assert!(line.contains(&quote));
    }

    #[test]
    fn rejects_content_that_only_matches_one_ambiguous_word() {
        let body = format!(
            "<<<UNTRUSTED_WEB_CONTENT:n source=\"x\">>>\n{}\n<<<END_UNTRUSTED_WEB_CONTENT:n>>>",
            "Explore popular three-dimensional fashion models, animations, and downloadable art assets. ".repeat(20)
        );
        let assessment = assess_content(
            &body,
            "https://sketchfab.com/3d-models/popular",
            "Popular 3D models",
            &["Model Context Protocol official specification tooling".into()],
        );
        assert!(!assessment.accepted);
        assert_eq!(assessment.reason, Some("low_content_relevance"));
    }

    #[test]
    fn routes_academic_and_technical_queries_to_specialized_categories() {
        assert_eq!(
            categories_for_query("academic study of a Rust protocol implementation"),
            Some("general,science,it")
        );
        assert_eq!(categories_for_query("today's weather"), Some("general"));
    }
}
