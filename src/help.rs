use scraper::{Html, Selector};
use serde::{Serialize, Deserialize};
use std::sync::{Arc, LazyLock};
use tokio::sync::Semaphore;

static SEM: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(20)));
static CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .user_agent("quicrawl (https://github.com/indium114/quicrawl)")
        .build()
        .unwrap()
});

// MARK: types
#[derive(Debug, Deserialize, Serialize)]
pub struct Site {
    pub title: String,
    pub url: String,
    pub text: String,
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
            if link.starts_with("/") {
                links.push(original_link.to_string() + link)
            } else {
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
        return full_text.split_whitespace().take(100).collect::<Vec<_>>().join(" ");
    }

    return "".to_string()
}

pub fn parse_title(html: &str) -> String {
    let document = Html::parse_document(html);
    let selector = Selector::parse("title").unwrap();

    if let Some(title) = document.select(&selector).next() {
        return title.text().collect::<Vec<_>>().join("");
    }

    return "Unknown Title".to_string()
}

pub fn spawn_crawl(url: String) {
    tokio::spawn(async move {
       crawl_url(url).await;
    });
}

pub async fn crawl_url(url: String) {
    let id = tokio::task::id();
    println!("[log] task {id} crawling {url}");

    let permit = SEM.acquire().await.unwrap();

    let response = match get(&url).await {
        Ok(response) => response,
        Err(e) => {
            println!("[err] while crawling {url}: {:#?}", e);
            return;
        }
    };

    drop(permit);

    let links = parse_links(&response, &url);
    let text = parse_text(&response);
    let title = parse_title(&response);

    let index_entry = Site {
        title: title,
        url: url,
        text: text,
    };

    // println!("{:#?}", index_entry);

    for link in links {
        spawn_crawl(link);
    }
}
