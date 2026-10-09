use scraper::{Html, Selector};
use slot_store::{Cart, Platform};
use unicode_normalization::UnicodeNormalization;

use crate::{Error, Transport};
const BASE: &str = "https://gamesdb.launchbox-app.com";
const IMAGE: &str = "https://images.launchbox-app.com/";

pub(crate) fn allowed_url(url: &str) -> bool {
    url.starts_with(&format!("{BASE}/")) || url.starts_with(IMAGE)
}

fn unavailable(message: &str) -> Error {
    Error::Unavailable(message.into())
}

fn selector(s: &str) -> Selector {
    Selector::parse(s).expect("static CSS selector")
}

pub(crate) fn title(stem: &str) -> String {
    normalize(&clean_title(stem))
}

fn clean_title(value: &str) -> String {
    let mut depth = 0u32;
    let clean: String = value
        .chars()
        .filter(|c| match c {
            '(' | '[' => {
                depth += 1;
                false
            }
            ')' | ']' => {
                depth = depth.saturating_sub(1);
                false
            }
            _ => depth == 0,
        })
        .collect();
    clean.replace(", The", "")
}

pub(crate) fn query(value: &str) -> String {
    clean_title(value)
        .chars()
        .map(|c| match c {
            c if c.is_alphanumeric() || matches!(c, '\'' | '’' | '.' | '-' | '+' | '&') => c,
            _ => ' ',
        })
        .collect::<String>()
        .split_whitespace()
        .filter(|token| *token == "&" || token.chars().any(char::is_alphanumeric))
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn normalize(value: &str) -> String {
    let mut text = String::new();
    for c in value
        .nfkd()
        .filter(|c| !unicode_normalization::char::is_combining_mark(*c))
    {
        match c {
            '&' => text.push_str(" and "),
            '$' => text.push('s'),
            '\'' | '’' => {}
            c if c.is_alphanumeric() => text.extend(c.to_lowercase()),
            _ => text.push(' '),
        }
    }
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    text.strip_prefix("the ")
        .or_else(|| text.strip_prefix("disneys "))
        .or_else(|| text.strip_prefix("lara croft "))
        .unwrap_or(&text)
        .to_owned()
}

fn encode(query: &str) -> String {
    let mut result = String::new();
    for b in query.bytes() {
        if b.is_ascii_alphanumeric() {
            result.push(b as char);
        } else {
            result.push_str(&format!("%{b:02X}"));
        }
    }
    result
}

/// The source's names for a cart's platform, its own first. Game Boy and Game Boy Color share
/// ROM extensions, so a cart may sit on either shelf.
fn platforms(platform: Platform) -> &'static [&'static str] {
    match platform {
        Platform::Gba => &["Nintendo Game Boy Advance"],
        Platform::Gb => &["Nintendo Game Boy", "Nintendo Game Boy Color"],
        Platform::Gbc => &["Nintendo Game Boy Color", "Nintendo Game Boy"],
    }
}

pub(crate) fn game_id(page: &str, wanted: &str, platform: Platform) -> Result<Vec<u64>, Error> {
    let doc = Html::parse_document(page);
    for name in platforms(platform) {
        let found = ids(&doc, wanted, name);
        if !found.is_empty() {
            return Ok(found);
        }
    }
    Err(unavailable("no title match"))
}

fn compare_key(value: &str) -> String {
    normalize(value)
        .replace(' ', "")
        .replace("ae", "a")
        .replace("oe", "o")
        .replace("ue", "u")
}

fn ids(doc: &Html, wanted: &str, platform: &str) -> Vec<u64> {
    let mut exact = Vec::new();
    let mut folded = Vec::new();
    for link in doc.select(&selector("a[href^='/games/details/']")) {
        let Some(heading) = link.select(&selector("h3")).next() else {
            continue;
        };
        let name = heading.text().collect::<String>();
        let listed = link
            .select(&selector("p"))
            .any(|p| p.text().collect::<String>().trim() == platform);
        if !listed {
            continue;
        }
        let href = link.value().attr("href").unwrap_or_default();
        if let Some(id) = href
            .strip_prefix("/games/details/")
            .and_then(|s| s.split('-').next())
            .and_then(|s| s.parse::<u64>().ok())
        {
            if normalize(&name) == wanted {
                if !exact.contains(&id) {
                    exact.push(id);
                }
            } else if compare_key(&name) == compare_key(wanted) && !folded.contains(&id) {
                folded.push(id);
            }
        }
    }
    if exact.is_empty() {
        folded
    } else {
        exact
    }
}

pub(crate) fn known(title: &str) -> Option<u64> {
    Some(match title {
        "advance wars" => 2367,
        "aladdin" => 3215,
        "dexters laboratory deesaster strikes" => 26190,
        "lady sia" => 14182,
        "legend of zelda a link to the past and four swords" => 8376,
        "metal slug advance" => 10743,
        "metroid zero mission" => 3552,
        "super mario advance" => 2225,
        "tetris worlds" => 3827,
        "tom clancys rainbow six rogue spear" => 3412,
        "warioware inc mega microgames" => 3917,
        "invincible iron man" => 10770,
        "rayman 10th anniversary" => 3325,
        "three in one pack connect four perfection trouble" => 21686,
        "three in one pack risk battleship clue" => 18148,
        "three in one pack sorry aggravation scrabble junior" => 91944,
        "crash and spyro superpack spyro season of ice crash bandicoot the huge adventure" => 18386,
        "pokemon ruby version" => 2241,
        "oriental blue ao no tengai" => 30529,
        "mega man battle network 6 cybeast gregar" => 6641,
        "tron 2 0 killer app" => 3907,
        "yggdra union well never fight alone" => 3817,
        _ => return None,
    })
}

fn own_regions(cart: &Cart) -> &'static [&'static str] {
    let stem = cart.stem.to_lowercase();
    // Filename regions take precedence over a possibly shared header code.
    let tags: Vec<_> = stem
        .split('(')
        .skip(1)
        .filter_map(|s| s.split_once(')').map(|p| p.0))
        .collect();
    for (token, regions) in [
        ("usa", &["North America", "United States"][..]),
        ("europe", &["Europe"][..]),
        ("japan", &["Japan"][..]),
        ("australia", &["Australia", "Oceania"][..]),
    ] {
        if tags
            .iter()
            .any(|tag| tag.split(',').any(|s| s.trim() == token))
        {
            return regions;
        }
    }
    match cart.code.as_bytes().get(3) {
        Some(b'J') => &["Japan"],
        Some(b'P') | Some(b'D') | Some(b'F') | Some(b'I') | Some(b'S') => &["Europe"],
        _ => &["North America", "United States"],
    }
}

pub(crate) fn image_url(page: &str, cart: &Cart) -> Result<String, Error> {
    let doc = Html::parse_document(page);
    let mut regions = own_regions(cart).to_vec();
    for region in ["North America", "United States", "Europe", "Japan"] {
        if !regions.contains(&region) {
            regions.push(region);
        }
    }
    for region in regions {
        for link in doc.select(&selector("a[data-title][href]")) {
            let description = link.value().attr("data-title").unwrap_or_default();
            let url = link.value().attr("href").unwrap_or_default();
            if description.contains(" - Cart - Front Image")
                && !description.contains("Fanart")
                && description.ends_with(&format!("({region})"))
                && url.starts_with(IMAGE)
            {
                return Ok(url.to_owned());
            }
        }
    }
    for world in [false, true] {
        for link in doc.select(&selector("a[data-title][href]")) {
            let description = link.value().attr("data-title").unwrap_or_default();
            let url = link.value().attr("href").unwrap_or_default();
            let suffix = if world { "(World)" } else { "Image" };
            if description.contains(" - Cart - Front Image")
                && !description.contains("Fanart")
                && description.ends_with(suffix)
                && url.starts_with(IMAGE)
            {
                return Ok(url.to_owned());
            }
        }
    }
    Err(unavailable("no matching regional cartridge artwork"))
}

pub(crate) fn year_query(query: &str) -> Option<String> {
    let bytes = query.as_bytes();
    for i in 2..bytes.len().saturating_sub(2) {
        if bytes[i] == b'-'
            && bytes[i - 2..i].iter().all(u8::is_ascii_digit)
            && bytes[i + 1..i + 3].iter().all(u8::is_ascii_digit)
        {
            return Some(format!("{}20{}", &query[..i - 2], &query[i + 1..]));
        }
    }
    None
}

fn try_gallery(
    http: &mut impl Transport,
    cart: &Cart,
    id: u64,
    network_error: &mut Option<Error>,
) -> Option<String> {
    match http.get(&format!("{BASE}/games/images/{id}")) {
        Ok(page) => image_url(&String::from_utf8_lossy(&page), cart).ok(),
        Err(error) => {
            if matches!(error, Error::Network(_)) {
                *network_error = Some(error);
            }
            None
        }
    }
}

pub(crate) fn resolve(
    http: &mut impl Transport,
    cart: &Cart,
    canonical_title: Option<&str>,
) -> Result<String, Error> {
    let key = canonical_title
        .map(normalize)
        .unwrap_or_else(|| title(&cart.stem));
    if key.is_empty() {
        return Err(unavailable("empty game title"));
    }
    let known = Some(&key)
        .filter(|_| cart.platform == Platform::Gba)
        .and_then(|t| known(t));
    let mut network_error = None;
    let mut candidate_found = known.is_some();
    let mut gallery_pages = 0;
    let mut attempted_gallery_ids = Vec::new();

    if let Some(id) = known {
        attempted_gallery_ids.push(id);
        if let Some(url) = try_gallery(http, cart, id, &mut network_error) {
            return Ok(url);
        }
    } else {
        let full = canonical_title.unwrap_or(&cart.stem);
        let mut queries = vec![(query(full), key.clone())];
        let stem_query = query(&cart.stem);
        if !queries.iter().any(|(query, _)| query == &stem_query) {
            queries.push((stem_query, key.clone()));
        }
        if !queries.iter().any(|(query, _)| query == &key) {
            queries.push((key.clone(), key.clone()));
        }
        if let Some(year) = year_query(&queries[0].0) {
            if !queries.iter().any(|(query, _)| query == &year) {
                queries.push((year.clone(), normalize(&year)));
            }
        }
        for (query, wanted) in queries.into_iter().take(4) {
            match http.get(&format!("{BASE}/games/results?id={}", encode(&query))) {
                Ok(page) => {
                    let ids = game_id(&String::from_utf8_lossy(&page), &wanted, cart.platform)
                        .unwrap_or_default();
                    candidate_found |= !ids.is_empty();
                    for id in ids {
                        if attempted_gallery_ids.contains(&id) {
                            continue;
                        }
                        attempted_gallery_ids.push(id);
                        gallery_pages += 1;
                        if let Some(url) = try_gallery(http, cart, id, &mut network_error) {
                            return Ok(url);
                        }
                        if gallery_pages == 3 {
                            break;
                        }
                    }
                }
                Err(error) if matches!(error, Error::Network(_)) => network_error = Some(error),
                Err(_) => {}
            }
            if gallery_pages == 3 {
                break;
            }
        }
    }
    if let Some(error) = network_error {
        return Err(error);
    }
    if candidate_found {
        Err(unavailable("no matching regional cartridge artwork"))
    } else {
        Err(unavailable("no title match"))
    }
}
