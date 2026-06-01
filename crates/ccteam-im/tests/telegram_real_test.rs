use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ccteam_im::transport::providers::telegram::TelegramChannel;
use ccteam_im::transport::{Channel, SendMessage};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_telegram_channel_roundtrip_smoke() {
    if std::env::var("CCTEAM_REAL_IM_TELEGRAM").ok().as_deref() != Some("1") {
        eprintln!("skip: set CCTEAM_REAL_IM_TELEGRAM=1 for real Telegram smoke");
        return;
    }

    let token = std::env::var("CCTEAM_TELEGRAM_BOT_TOKEN")
        .or_else(|_| std::env::var("CCTEAM_TELEGRAM_TOKEN"))
        .expect("CCTEAM_TELEGRAM_BOT_TOKEN is required");
    let chat_id =
        std::env::var("CCTEAM_TELEGRAM_CHAT_ID").expect("CCTEAM_TELEGRAM_CHAT_ID is required");
    let wait_secs = std::env::var("CCTEAM_TELEGRAM_WAIT_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(120);

    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let expected = std::env::var("CCTEAM_TELEGRAM_EXPECT_TEXT")
        .unwrap_or_else(|_| format!("CCTEAM-TELEGRAM-OK-{nonce}"));
    let channel = Arc::new(TelegramChannel::new(token, vec![chat_id.clone()]));

    assert!(
        channel.health_check().await,
        "Telegram getMe health check failed"
    );
    channel
        .send(&SendMessage::new(
            format!("ccteam real Telegram smoke: reply exactly `{expected}`"),
            chat_id.clone(),
        ))
        .await
        .expect("Telegram sendMessage prompt should succeed");

    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    let listener = {
        let channel = Arc::clone(&channel);
        tokio::spawn(async move { channel.listen(tx).await })
    };

    let deadline = tokio::time::Instant::now() + Duration::from_secs(wait_secs);
    let mut matched = None;
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining.min(Duration::from_secs(10)), rx.recv()).await {
            Ok(Some(msg)) => {
                if msg.reply_target == chat_id && msg.content.trim() == expected {
                    matched = Some(msg);
                    break;
                }
            }
            Ok(None) => break,
            Err(_) => continue,
        }
    }

    listener.abort();
    let msg = matched.unwrap_or_else(|| {
        panic!(
            "did not receive expected Telegram reply `{expected}` in chat {chat_id} within {wait_secs}s"
        )
    });
    channel
        .send(&SendMessage::new(
            format!("ccteam real Telegram smoke PASS: {}", msg.id),
            chat_id,
        ))
        .await
        .expect("Telegram sendMessage ACK should succeed");
}
