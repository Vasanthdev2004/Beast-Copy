use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use futures::{StreamExt, SinkExt};

#[tokio::main]
async fn main() {
    let url = "wss://ws-subscriptions-clob.polymarket.com/ws/market";
    println!("Connecting to {}...", url);
    let (ws_stream, _) = connect_async(url).await.unwrap();
    println!("Connected!");

    let (mut write, mut read) = ws_stream.split();

    let sub_msg = serde_json::json!({
        "assets_ids": [],
        "type": "market",
        "action": "subscribe"
    });
    
    write.send(Message::Text(sub_msg.to_string())).await.unwrap();
    println!("Subscribed to market channel with empty assets_ids");

    while let Some(msg) = read.next().await {
        if let Ok(Message::Text(text)) = msg {
            println!("RCV: {}", text);
            // Just print the first 5 messages to see the shape
            break;
        }
    }
}
