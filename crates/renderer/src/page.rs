//! View data and the deterministic no-cover gradient.
//!
//! A [`Site`] is the whole set of Timelines a render run has loaded. Pages are
//! plain owned data: the renderer reads DynamoDB first, then renders from
//! memory, so no reactive resource and no async component is needed during the
//! static render.

use data::model::{Event, Timeline};

/// Everything a render run loaded from DynamoDB.
#[derive(Clone, Debug, Default)]
pub struct Site {
    /// One page per Timeline. Ordered by [`Timeline::id`] for deterministic output.
    pub pages: Vec<TimelinePage>,
}

impl Site {
    /// Ids in stable order; the static-route prerender list comes from here.
    pub fn timeline_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .pages
            .iter()
            .map(|page| page.timeline.id.clone())
            .collect();
        ids.sort();
        ids
    }
}

/// A Timeline plus its ordered Events and the denormalized byline/eyebrow.
#[derive(Clone, Debug)]
pub struct TimelinePage {
    pub timeline: Timeline,
    /// Chronological: `SK` is `EVENT#<timestamp>#<id>` and DynamoDB queries
    /// return it in ascending order.
    pub events: Vec<Event>,
    /// `ACCOUNT#<ownerId>.displayName`, when the owner row exists.
    pub owner_name: Option<String>,
    /// `COLLECTION#<collectionId>.title`, when the Timeline is collected.
    pub collection_title: Option<String>,
}

/// Two deterministic hues derived from the Timeline id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gradient {
    pub hue: u16,
    pub accent: u16,
}

impl Gradient {
    /// Inline custom properties consumed by `.hero--gradient` in the stylesheet.
    pub fn style(self) -> String {
        format!(
            "--gradient-from:hsl({} 42% 21%);--gradient-to:hsl({} 52% 10%);",
            self.hue, self.accent
        )
    }
}

/// FNV-1a over the Timeline id. Stable across runs, platforms, and hosts, so
/// the same Timeline always gets the same hero even with no stored cover.
pub fn gradient_for(timeline_id: &str) -> Gradient {
    const FNV_OFFSET: u32 = 2_166_136_261;
    const FNV_PRIME: u32 = 16_777_619;

    let hash = timeline_id.bytes().fold(FNV_OFFSET, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(FNV_PRIME)
    });
    let hue = (hash % 360) as u16;
    let accent = ((u32::from(hue) + 35 + (hash >> 9) % 70) % 360) as u16;
    Gradient { hue, accent }
}

/// Reader-facing label for a Source chip: the host, without the scheme or path.
pub fn source_label(url: &str) -> String {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let host = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(rest)
        .trim_start_matches("www.");
    host.to_owned()
}

/// Only `http(s)` Sources become links. The publish checklist guarantees this,
/// but rendering refuses to turn any other scheme into an `href`.
pub fn is_http_source(url: &str) -> bool {
    url.starts_with("https://") || url.starts_with("http://")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_label_is_the_host() {
        assert_eq!(
            source_label("https://www.ohiosos.gov/elections/candidates/"),
            "ohiosos.gov"
        );
        assert_eq!(source_label("http://example.com"), "example.com");
        assert_eq!(source_label("not a url"), "not a url");
    }

    #[test]
    fn only_http_sources_are_links() {
        assert!(is_http_source("https://example.com"));
        assert!(is_http_source("http://example.com"));
        assert!(!is_http_source("javascript:alert(1)"));
        assert!(!is_http_source("data:text/html,hi"));
    }
}
