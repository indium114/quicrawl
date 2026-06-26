use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock, Mutex};
use std::fs;
use tokio::sync::Semaphore;

static REQUEST_SEM: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(20)));
static WRITE_SEM: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(1)));
static CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .user_agent("quicrawl (https://github.com/indium114/quicrawl)")
        .build()
        .unwrap()
});

static ROBOTS_CACHE: LazyLock<Mutex<HashMap<String, Arc<RobotsTxt>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

static VISITED: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

// MARK: robots.txt stuff
struct RobotsTxt {
    disallows: Vec<String>,
    allows: Vec<String>,
}

impl RobotsTxt {
    fn allow_all() -> Self {
        RobotsTxt {
            disallows: Vec::new(),
            allows: Vec::new(),
        }
    }

    fn parse(body: &str) -> Self {
        let mut disallows = Vec::new();
        let mut allows = Vec::new();
        let mut in_my_section = false;

        for line in body.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(agent) = line.strip_prefix("User-agent:").map(|s| s.trim()) {
                in_my_section = agent == "*" || agent.eq_ignore_ascii_case("quicrawl");
            } else if in_my_section {
                if let Some(path) = line.strip_prefix("Disallow:").map(|s| s.trim()) {
                    if path.is_empty() {
                        disallows.clear();
                        allows.clear();
                    } else {
                        disallows.push(path.to_string());
                    }
                } else if let Some(path) = line.strip_prefix("Allow:").map(|s| s.trim()) {
                    if !path.is_empty() {
                        allows.push(path.to_string());
                    }
                }
            }
        }

        RobotsTxt { disallows, allows }
    }

    fn is_allowed(&self, path: &str) -> bool {
        let mut matched: Option<(usize, bool)> = None;

        for d in &self.disallows {
            if path.starts_with(d) {
                let better = matched.map_or(true, |(len, _)| d.len() > len);
                if better {
                    matched = Some((d.len(), false));
                }
            }
        }
        for a in &self.allows {
            if path.starts_with(a) {
                let better = matched.map_or(true, |(len, allowed)| {
                    a.len() > len || (a.len() == len && !allowed)
                });
                if better {
                    matched = Some((a.len(), true));
                }
            }
        }

        matched.map_or(true, |(_, allowed)| allowed)
    }
}

fn extract_domain(url: &str) -> Option<&str> {
    let domain = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))?;
    domain.split('/').next()
}

fn extract_path(url: &str) -> &str {
    let after_scheme = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .unwrap_or(url);
    after_scheme.find('/').map_or("/", |i| &after_scheme[i..])
}

async fn ensure_robots(domain: &str, scheme: &str) -> Arc<RobotsTxt> {
    {
        let cache = ROBOTS_CACHE.lock().unwrap();
        if let Some(rules) = cache.get(domain) {
            return rules.clone();
        }
    }

    let robots_url = format!("{}://{}/robots.txt", scheme, domain);
    let robots = match CLIENT.get(&robots_url).send().await {
        Ok(resp) if resp.status().is_success() => match resp.text().await {
            Ok(body) => RobotsTxt::parse(&body),
            Err(_) => RobotsTxt::allow_all(),
        },
        _ => RobotsTxt::allow_all(),
    };
    let robots = Arc::new(robots);

    let mut cache = ROBOTS_CACHE.lock().unwrap();
    cache.insert(domain.to_string(), robots.clone());
    robots
}

// MARK: types
#[derive(Debug, Deserialize, Serialize, PartialEq)]
pub struct Site {
    pub title: String,
    pub url: String,
    pub text: String,
}

// MARK: save/load helpers
pub fn load_index() -> Vec<Site> {
    fs::read_to_string("index.json")
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_index(index: Vec<Site>) -> bool {
    match serde_json::to_string_pretty(&index) {
        Ok(json) => fs::write("index.json", json).is_ok(),
        Err(_) => false,
    }
}

// MARK: helper functions
pub async fn get(url: &str) -> std::result::Result<String, reqwest::Error> {
    let response = CLIENT.get(url).send().await?;

    response.text().await
}

pub fn parse_links(html: &str, original_link: &str) -> Vec<String> {
    let mut links: Vec<String> = Vec::new();
    let document = Html::parse_document(html);
    let selector = Selector::parse("a[href]").unwrap();

    for element in document.select(&selector) {
        if let Some(link) = element.value().attr("href") {
            let link = link.trim();
            if link.starts_with('/') {
                links.push(original_link.to_string() + link);
            } else if link.starts_with("http://") || link.starts_with("https://") {
                links.push(link.to_string());
            }
        }
    }

    links
}

pub fn parse_text(html: &str) -> String {
    let document = Html::parse_document(html);
    let selector = Selector::parse("body").unwrap();

    if let Some(body) = document.select(&selector).next() {
        let full_text = body.text().collect::<Vec<_>>().join(" ");
        return full_text
            .split_whitespace()
            .take(100)
            .collect::<Vec<_>>()
            .join(" ");
    }

    return "".to_string();
}

pub fn parse_title(html: &str) -> String {
    let document = Html::parse_document(html);
    let selector = Selector::parse("title").unwrap();

    if let Some(title) = document.select(&selector).next() {
        return title.text().collect::<Vec<_>>().join("");
    }

    return "Unknown Title".to_string();
}

pub fn spawn_crawl(url: String) {
    tokio::spawn(async move {
        crawl_url(url).await;
    });
}

pub async fn crawl_url(url: String) {
    let id = tokio::task::id();

    {
        let mut visited = VISITED.lock().unwrap();
        if !visited.insert(url.clone()) {
            usefulog::log(format!("task {id} skipped {url} (already visited)"));
            return;
        }
    }

    usefulog::log(format!("task {id} crawling {url}"));

    if let Some(domain) = extract_domain(&url) {
        let scheme = if url.starts_with("https://") {
            "https"
        } else {
            "http"
        };
        let robots = ensure_robots(domain, scheme).await;
        if !robots.is_allowed(extract_path(&url)) {
            usefulog::log(format!("task {id} skipped {url} (blocked by robots.txt)"));
            return;
        }
    }

    let request_permit = REQUEST_SEM.acquire().await.unwrap();

    let response = match get(&url).await {
        Ok(response) => response,
        Err(e) => {
            usefulog::err(format!("while crawling {url}: {:#?}", e));
            return;
        }
    };

    drop(request_permit);

    let links = parse_links(&response, &url);
    let text = parse_text(&response);
    let title = parse_title(&response);

    let index_entry = Site {
        title: title,
        url: url,
        text: text,
    };

    #[cfg(debug_assertions)]
    println!("=== index_entry");
    #[cfg(debug_assertions)]
    println!("{:#?}", index_entry);

    #[cfg(debug_assertions)]
    println!("=== links");
    #[cfg(debug_assertions)]
    println!("{:#?}", &links);

    for link in links {
        spawn_crawl(link);
    }


    let mut index = load_index();
    if index.contains(&index_entry) {
        return
    }
    let write_permit = WRITE_SEM.acquire().await.unwrap();
    index.push(index_entry);
    let _ = save_index(index);

    usefulog::hint(format!("wrote index entry to disk"));

    drop(write_permit);
}
