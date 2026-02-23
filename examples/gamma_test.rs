use reqwest;

#[tokio::main]
async fn main() {
    let wallet = "0x2c7202074DA07249e447636A714D04e70E365e72";
    
    let urls = vec![
        format!("https://data-api.polymarket.com/activity?user={}&limit=5", wallet),
        format!("https://data-api.polymarket.com/activity?user={}&limit=5", wallet.to_lowercase()),
        format!("https://data-api.polymarket.com/trades?user={}&limit=5", wallet),
        format!("https://clob.polymarket.com/trades?maker_address={}&limit=5", wallet),
        format!("https://gamma-api.polymarket.com/activity?user={}&limit=5", wallet),
        format!("https://data-api.polymarket.com/positions?user={}&limit=5", wallet),
        format!("https://data-api.polymarket.com/positions?user={}&limit=5", wallet.to_lowercase()),
    ];
    
    for url in urls {
        println!("\n=== {} ===", url);
        match reqwest::get(&url).await {
            Ok(res) => {
                let status = res.status();
                let text = res.text().await.unwrap_or_default();
                let preview = if text.len() > 500 { &text[..500] } else { &text };
                println!("Status: {} | Size: {} bytes\n{}", status, text.len(), preview);
            },
            Err(e) => println!("Error: {}", e),
        }
    }
}
