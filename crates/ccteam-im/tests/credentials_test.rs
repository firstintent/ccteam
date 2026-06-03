//! Credentials reader integration tests.

use ccteam_im::credentials::{load, save, Credentials, LarkCreds, SlackCreds, TelegramCreds};
use tempfile::TempDir;

#[test]
fn missing_file_returns_default() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("none.json");
    let c = load(Some(&path)).unwrap();
    assert!(c.telegram.is_none());
    assert!(c.slack.is_none());
    assert!(c.discord.is_none());
    assert!(c.lark.is_none());
}

#[test]
fn round_trip_all_platforms() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("c.json");
    let original = Credentials {
        telegram: Some(TelegramCreds {
            bot_token: "TG:abc".into(),
            allowed_chat_ids: vec!["1".into(), "2".into()],
        }),
        slack: Some(SlackCreds {
            bot_token: "xoxb-x".into(),
            signing_secret: Some("sig".into()),
            poll_channels: vec!["C123".into()],
        }),
        discord: None,
        lark: Some(LarkCreds {
            app_id: "cli_x".into(),
            app_secret: "sek".into(),
            allowed_user_ids: vec!["ou_a".into()],
            use_feishu: true,
        }),
    };
    save(&path, &original).unwrap();
    let back = load(Some(&path)).unwrap();
    assert_eq!(back, original);
}

#[test]
fn empty_object_parses_to_default() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("c.json");
    std::fs::write(&path, "{}").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut p = std::fs::metadata(&path).unwrap().permissions();
        p.set_mode(0o600);
        std::fs::set_permissions(&path, p).unwrap();
    }
    let c = load(Some(&path)).unwrap();
    assert_eq!(c, Credentials::default());
}
