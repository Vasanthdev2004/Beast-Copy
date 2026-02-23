use reqwest;

#[tokio::main]
async fn main() {
    let wallet = "0x1d0034134e339a309700ff2d34e99fa2d48b0313";
    let url = format!("https://data-api.polymarket.com/activity?user={}&limit=5&offset=0", wallet);
    
    let client = reqwest::Client::builder()
        .user_agent("Mozilla/5.0")
        .build()
        .unwrap();

    println!("Fetching {}", url);
    match client.get(&url).send().await {
        Ok(res) => {
            let status = res.status();
            let text = res.text().await.unwrap_or_default();
            let preview = if text.len() > 2000 { &text[..2000] } else { &text };
            println!("Status: {} | Size: {} bytes\n{}", status, text.len(), preview);
        },
        Err(e) => println!("Error: {}", e),
    }
}
