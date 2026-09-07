//! A small, strict `robots.txt` reader.
//!
//! Being a good citizen is not optional for a library people will point at
//! other people's servers, so this is on by default. The implementation follows
//! the parts of RFC 9309 that matter in practice: user-agent grouping, longest
//! matching rule wins, `Allow` beats `Disallow` on equal length, `*` and `$`
//! wildcards, and `Crawl-delay` as a courtesy extension.

/// Parsed rules for one user-agent.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Robots {
    rules: Vec<(bool, String)>,
    /// Seconds the host asked us to wait between requests, if stated.
    pub crawl_delay: Option<f64>,
    /// Sitemaps advertised in the file.
    pub sitemaps: Vec<String>,
}

impl Robots {
    /// An empty policy that allows everything.
    pub fn allow_all() -> Self {
        Robots::default()
    }

    /// Parse `robots.txt`, keeping the group that applies to `agent`.
    ///
    /// A group naming our agent explicitly always wins over the `*` group, as
    /// the RFC requires, even when `*` appears later in the file.
    pub fn parse(body: &str, agent: &str) -> Self {
        let agent_lc = agent.to_lowercase();
        let mut specific: Vec<(bool, String)> = Vec::new();
        let mut wildcard: Vec<(bool, String)> = Vec::new();
        let mut specific_delay = None;
        let mut wildcard_delay = None;
        let mut sitemaps = Vec::new();

        // Which groups the current run of `User-agent:` lines opened.
        let mut in_specific = false;
        let mut in_wildcard = false;
        let mut last_was_agent = false;

        for line in body.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let Some((key, value)) = line.split_once(':') else { continue };
            let key = key.trim().to_lowercase();
            let value = value.trim();

            match key.as_str() {
                "user-agent" => {
                    if !last_was_agent {
                        in_specific = false;
                        in_wildcard = false;
                    }
                    let v = value.to_lowercase();
                    if v == "*" {
                        in_wildcard = true;
                    } else if agent_lc.contains(&v) || v.contains(&agent_lc) {
                        in_specific = true;
                    }
                    last_was_agent = true;
                    continue;
                }
                "sitemap" => {
                    sitemaps.push(value.to_string());
                    last_was_agent = false;
                    continue;
                }
                _ => {}
            }
            last_was_agent = false;

            match key.as_str() {
                "disallow" | "allow" => {
                    let allow = key == "allow";
                    // `Disallow:` with an empty value means "allow everything".
                    if value.is_empty() && !allow {
                        continue;
                    }
                    if in_specific {
                        specific.push((allow, value.to_string()));
                    }
                    if in_wildcard {
                        wildcard.push((allow, value.to_string()));
                    }
                }
                "crawl-delay" => {
                    let d = value.parse::<f64>().ok().filter(|d| d.is_finite() && *d >= 0.0);
                    if in_specific {
                        specific_delay = d;
                    }
                    if in_wildcard {
                        wildcard_delay = d;
                    }
                }
                _ => {}
            }
        }

        if specific.is_empty() && specific_delay.is_none() {
            Robots { rules: wildcard, crawl_delay: wildcard_delay, sitemaps }
        } else {
            Robots { rules: specific, crawl_delay: specific_delay, sitemaps }
        }
    }

    /// May we fetch this path (including query string)?
    pub fn allows(&self, path: &str) -> bool {
        let mut best: Option<(usize, bool)> = None;
        for (allow, pattern) in &self.rules {
            if !matches_pattern(pattern, path) {
                continue;
            }
            let len = pattern.len();
            match best {
                // Equal specificity: `Allow` wins, per RFC 9309 §2.2.2.
                Some((blen, ballow)) if blen > len || (blen == len && ballow) => {}
                _ => best = Some((len, *allow)),
            }
        }
        best.map(|(_, allow)| allow).unwrap_or(true)
    }
}

/// Glob matching limited to the two wildcards robots.txt defines.
fn matches_pattern(pattern: &str, path: &str) -> bool {
    let anchored = pattern.ends_with('$');
    let pattern = pattern.strip_suffix('$').unwrap_or(pattern);

    let mut pos = 0usize;
    let mut parts = pattern.split('*');
    // The first segment must match at the very start.
    if let Some(first) = parts.next() {
        if !path[pos..].starts_with(first) {
            return false;
        }
        pos += first.len();
    }
    let segments: Vec<&str> = parts.collect();
    for (i, seg) in segments.iter().enumerate() {
        let last = i + 1 == segments.len();
        if seg.is_empty() {
            if last && anchored {
                return true; // trailing `*$`
            }
            continue;
        }
        if last && anchored {
            return path[pos..].ends_with(seg);
        }
        match path[pos..].find(seg) {
            Some(at) => pos += at + seg.len(),
            None => return false,
        }
    }
    if anchored && segments.is_empty() { pos == path.len() } else { true }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BODY: &str = r#"
        # comment
        User-agent: *
        Disallow: /private/
        Allow: /private/public/
        Crawl-delay: 2

        User-agent: rustai
        Disallow: /nope
        Disallow: /*.pdf$

        Sitemap: https://example.com/sitemap.xml
    "#;

    #[test]
    fn specific_group_wins_over_wildcard() {
        let r = Robots::parse(BODY, "rustai/0.1");
        assert!(r.allows("/private/secret"), "wildcard group should not apply");
        assert!(!r.allows("/nope"));
    }

    #[test]
    fn wildcard_group_applies_to_others() {
        let r = Robots::parse(BODY, "SomeOtherBot");
        assert!(!r.allows("/private/secret"));
        assert!(r.allows("/private/public/page"));
        assert_eq!(r.crawl_delay, Some(2.0));
    }

    #[test]
    fn longest_match_wins_and_allow_breaks_ties() {
        let r = Robots::parse("User-agent: *\nDisallow: /a/\nAllow: /a/", "x");
        assert!(r.allows("/a/b"));
    }

    #[test]
    fn dollar_anchors_the_end() {
        let r = Robots::parse(BODY, "rustai");
        assert!(!r.allows("/docs/manual.pdf"));
        assert!(r.allows("/docs/manual.pdf?x=1"));
    }

    #[test]
    fn empty_disallow_allows_everything() {
        let r = Robots::parse("User-agent: *\nDisallow:", "x");
        assert!(r.allows("/anything"));
    }

    #[test]
    fn sitemaps_are_collected() {
        let r = Robots::parse(BODY, "x");
        assert_eq!(r.sitemaps, ["https://example.com/sitemap.xml"]);
    }

    #[test]
    fn a_wildcard_crawl_delay_is_picked_up() {
        // arXiv publishes exactly this, and honouring it is why a second
        // request to the same host through one client waits fifteen seconds.
        let r = Robots::parse("User-agent: *\nCrawl-delay: 15\nAllow: /list\n", "rustai");
        assert_eq!(r.crawl_delay, Some(15.0));
    }

    #[test]
    fn a_crawl_delay_in_a_comment_is_not_a_directive() {
        // Wikipedia's robots.txt discusses crawl-delay in prose. Reading that
        // as a directive would stall every request to it.
        let r = Robots::parse(
            "# semrushbot respects crawl-delay directives\nUser-agent: *\nDisallow: /w/\n",
            "rustai",
        );
        assert_eq!(r.crawl_delay, None);
    }

    #[test]
    fn missing_file_allows_all() {
        assert!(Robots::allow_all().allows("/anything"));
    }
}
