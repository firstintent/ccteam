//! Pure-parser, codec, and URL-builder tests for the Lark/Feishu
//! channel. Adapted from
//! `references/openhuman/src/openhuman/channels/providers/lark_tests.rs`:
//! the `LarkConfig`/`LarkReceiveMode` serde tests are dropped (those
//! types don't exist in ccteam — region behavior is covered by the
//! URL-builder test instead), the 4-arg ctor replaces openhuman's
//! 5-arg one, and the WS frame literals are rewritten for tungstenite
//! 0.24 (`Vec<u8>`, no `.into()`).
//!
//! Zero network / fs / env. The inbound decode is covered against the
//! *production* parser, not a parallel copy: `decode_event` is exactly
//! what `listen_ws` runs on each reassembled DATA-frame payload, and
//! `decode_event_value` shares `decode_message_receive` +
//! `into_channel_message` with it — so feeding event JSON/bytes through
//! either here exercises the same decode→map (sender open_id, text/`post`,
//! @-placeholder strip, group @-mention gate) the live loop runs. The
//! wire framing is proven by the `pbbp2_*` round-trips. The only
//! uncovered seam is the socket plumbing of `listen_ws` itself (connect /
//! ACK / heartbeat / fragment reassembly / dedup); driving that would
//! need a `listen_ws_at(url)` seam + a scripted localhost peer, out of
//! Path-A parity scope.

use super::*;

fn make_channel() -> LarkChannel {
    LarkChannel::new(
        "cli_test_app_id".into(),
        "test_app_secret".into(),
        vec!["ou_testuser123".into()],
        true,
    )
}

// ── test-only helpers (inlined from openhuman's `test_support`) ─────────────

/// Deserialize a raw `POST /callback/ws/endpoint` reply and extract
/// `(url, ping_interval)`, mirroring the parsing inside `get_ws_endpoint`
/// without a socket.
fn endpoint_response(raw: &str) -> anyhow::Result<(String, Option<u64>)> {
    let resp = serde_json::from_str::<WsEndpointResp>(raw)?;
    if resp.code != 0 {
        anyhow::bail!(
            "Lark WS endpoint failed: code={} msg={}",
            resp.code,
            resp.msg.as_deref().unwrap_or("(none)")
        );
    }
    let ep = resp
        .data
        .ok_or_else(|| anyhow::anyhow!("Lark WS endpoint: empty data"))?;
    Ok((ep.url, ep.client_config.and_then(|cfg| cfg.ping_interval)))
}

/// Parse the `service_id` query param from a wss URL (default 0 on a
/// malformed / absent query), mirroring `listen_ws`.
fn parse_service_id(wss_url: &str) -> i32 {
    wss_url
        .split('?')
        .nth(1)
        .and_then(|qs| {
            qs.split('&')
                .find(|kv| kv.starts_with("service_id="))
                .and_then(|kv| kv.split('=').nth(1))
                .and_then(|v| v.parse::<i32>().ok())
        })
        .unwrap_or(0)
}

// ── channel basics ─────────────────────────────────────────────

#[test]
fn lark_channel_name() {
    let ch = make_channel();
    assert_eq!(ch.name(), "lark");
}

#[test]
fn lark_new_stores_fields_and_allowlist() {
    let ch = LarkChannel::new(
        "app_id".into(),
        "secret".into(),
        vec!["u1".into(), "u2".into()],
        true,
    );
    assert_eq!(ch.app_id, "app_id");
    assert_eq!(ch.allowed_users.len(), 2);
    assert!(ch.use_feishu);
}

// ── reaction (👀 ack) ──────────────────────────────────────────

#[test]
fn reaction_create_body_carries_emoji_type() {
    // Feishu wants {"reaction_type":{"emoji_type":"<TYPE>"}}.
    let body = reaction_create_body(LARK_ACK_EMOJI_TYPE);
    assert_eq!(body["reaction_type"]["emoji_type"], "OnIt");
}

#[test]
fn lark_ack_emoji_type_is_on_it() {
    // The 👀-equivalent: Feishu has no plain EYES, so OnIt ("on it / seen,
    // working") is the chosen ack emoji_type.
    assert_eq!(LARK_ACK_EMOJI_TYPE, "OnIt");
}

#[test]
fn reaction_urls_are_message_keyed() {
    let ch = make_channel();
    assert_eq!(
        ch.add_reaction_url("om_abc"),
        "https://open.feishu.cn/open-apis/im/v1/messages/om_abc/reactions"
    );
    assert_eq!(
        ch.delete_reaction_url("om_abc", "ZCaCIjUB"),
        "https://open.feishu.cn/open-apis/im/v1/messages/om_abc/reactions/ZCaCIjUB"
    );
}

#[tokio::test]
async fn remove_reaction_none_handle_is_noop() {
    // No reaction_id ⇒ nothing to delete; must NOT error (and never hits the
    // network, so it's safe in the sandbox).
    let ch = make_channel();
    assert!(ch.remove_reaction("oc_chat", "om_abc", None).await.is_ok());
}

// ── is_user_allowed ────────────────────────────────────────────

#[test]
fn lark_user_allowed_exact() {
    let ch = make_channel();
    assert!(ch.is_user_allowed("ou_testuser123"));
    assert!(!ch.is_user_allowed("ou_other"));
}

#[test]
fn lark_user_allowed_wildcard() {
    let ch = LarkChannel::new("id".into(), "secret".into(), vec!["*".into()], true);
    assert!(ch.is_user_allowed("ou_anyone"));
}

#[test]
fn lark_user_denied_empty() {
    let ch = LarkChannel::new("id".into(), "secret".into(), vec![], true);
    assert!(!ch.is_user_allowed("ou_anyone"));
}

#[test]
fn lark_is_user_allowed_wildcard_allows_everyone() {
    let ch = LarkChannel::new("a".into(), "s".into(), vec!["*".into()], true);
    assert!(ch.is_user_allowed("anyone"));
}

#[test]
fn lark_is_user_allowed_empty_allowlist_blocks_everyone() {
    // Empty allowlist matches nothing — explicit guard against the
    // "accidentally allowing all users" bug.
    let ch = LarkChannel::new("a".into(), "s".into(), vec![], true);
    assert!(!ch.is_user_allowed("anyone"));
}

#[test]
fn lark_is_user_allowed_respects_allowlist() {
    let ch = LarkChannel::new("a".into(), "s".into(), vec!["u1".into()], true);
    assert!(ch.is_user_allowed("u1"));
    assert!(!ch.is_user_allowed("u2"));
}

// ── heartbeat watchdog ─────────────────────────────────────────

#[test]
fn should_refresh_last_recv_true_for_binary_ping_pong() {
    assert!(should_refresh_last_recv(&WsMsg::Binary(vec![1, 2, 3])));
    assert!(should_refresh_last_recv(&WsMsg::Ping(vec![])));
    assert!(should_refresh_last_recv(&WsMsg::Pong(vec![])));
}

#[test]
fn should_refresh_last_recv_false_for_text_and_close() {
    assert!(!should_refresh_last_recv(&WsMsg::Text("x".to_string())));
    assert!(!should_refresh_last_recv(&WsMsg::Close(None)));
}

// ── pbbp2 codec round-trip (proves the hand-ported wire format) ─

#[test]
fn pbbp2_frame_roundtrip() {
    let frame = PbFrame {
        seq_id: 7,
        log_id: 0,
        service: 7,
        method: 1,
        headers: vec![PbHeader {
            key: "type".into(),
            value: "event".into(),
        }],
        payload: Some(br#"{"ok":true}"#.to_vec()),
    };
    let raw = frame.encode_to_vec();
    let decoded = PbFrame::decode(&raw[..]).expect("decode");
    assert_eq!(decoded.seq_id, 7);
    assert_eq!(decoded.method, 1);
    assert_eq!(decoded.header_value("type"), "event");
    assert_eq!(decoded.payload.as_deref(), Some(&br#"{"ok":true}"#[..]));
}

#[test]
fn pbbp2_control_frame_roundtrip() {
    // CONTROL discriminator (method=0, pong) — the other branch.
    let frame = PbFrame {
        seq_id: 1,
        log_id: 0,
        service: 0,
        method: 0,
        headers: vec![PbHeader {
            key: "type".into(),
            value: "pong".into(),
        }],
        payload: None,
    };
    let raw = frame.encode_to_vec();
    let decoded = PbFrame::decode(&raw[..]).expect("decode");
    assert_eq!(decoded.method, 0);
    assert_eq!(decoded.header_value("type"), "pong");
    assert!(decoded.payload.is_none());
}

// ── WS-endpoint reply parse + service_id query parse ───────────

#[test]
fn endpoint_response_extracts_url_and_ping() {
    let raw =
        r#"{"code":0,"data":{"URL":"wss://x?service_id=42","ClientConfig":{"PingInterval":11}}}"#;
    let (url, ping) = endpoint_response(raw).expect("parsed");
    assert_eq!(url, "wss://x?service_id=42");
    assert_eq!(ping, Some(11));
}

#[test]
fn endpoint_response_errors_on_nonzero_code() {
    let raw = r#"{"code":1901,"msg":"bad app"}"#;
    let err = endpoint_response(raw).expect_err("should error");
    assert!(
        err.to_string().contains("1901"),
        "error should carry the code, got: {err}"
    );
}

#[test]
fn service_id_defaults_zero_on_malformed_query() {
    assert_eq!(parse_service_id("wss://x?service_id=42"), 42);
    assert_eq!(parse_service_id("wss://x"), 0);
    assert_eq!(parse_service_id("wss://x?foo=bar"), 0);
    assert_eq!(parse_service_id("wss://x?service_id=notanint"), 0);
}

// ── region URL builders (Feishu <-> Lark switch) ───────────────

#[test]
fn region_urls_switch_on_use_feishu() {
    let lark = LarkChannel::new("a".into(), "s".into(), vec![], false);
    assert!(
        lark.tenant_access_token_url()
            .contains("open.larksuite.com/open-apis/auth"),
        "intl token url, got: {}",
        lark.tenant_access_token_url()
    );
    assert!(
        lark.send_message_url()
            .ends_with("/im/v1/messages?receive_id_type=chat_id"),
        "send url shape, got: {}",
        lark.send_message_url()
    );
    assert!(lark.send_message_url().contains("open.larksuite.com"));

    let feishu = LarkChannel::new("a".into(), "s".into(), vec![], true);
    assert!(feishu.tenant_access_token_url().contains("open.feishu.cn"));
    assert!(feishu.send_message_url().contains("open.feishu.cn"));
}

// ── decode_event_value (the tested seam — same parser the WS loop runs) ─
//
// These drive `decode_event_value`, which shares `decode_message_receive`
// + `into_channel_message` with the live `listen_ws` path, so they exercise
// the exact decode→map→ACL production runs. The byte-level WS entry
// (`decode_event` + the frame codec) is covered by `ws_*` below.

#[test]
fn lark_decode_non_event_payload() {
    let ch = make_channel();
    let payload = serde_json::json!({
        "challenge": "abc123",
        "token": "test_verification_token",
        "type": "url_verification"
    });
    assert!(ch.decode_event_value(&payload).is_none());
}

#[test]
fn lark_decode_valid_text_message() {
    let ch = make_channel();
    let payload = serde_json::json!({
        "header": { "event_type": "im.message.receive_v1" },
        "event": {
            "sender": { "sender_id": { "open_id": "ou_testuser123" } },
            "message": {
                "message_id": "om_abc",
                "message_type": "text",
                "content": "{\"text\":\"Hello OpenHuman!\"}",
                "chat_id": "oc_chat123",
                "create_time": "1699999999000"
            }
        }
    });

    let msg = ch.decode_event_value(&payload).expect("decoded");
    assert_eq!(msg.content, "Hello OpenHuman!");
    // sender = the user open_id (so the daemon ACL keys on a user, not a
    // chat); reply_target = the chat id (so replies still route).
    assert_eq!(msg.sender, "ou_testuser123");
    assert_eq!(msg.reply_target, "oc_chat123");
    assert_eq!(msg.id, "lark-om_abc");
    assert_eq!(msg.channel, "lark");
    assert_eq!(msg.timestamp, 1_699_999_999);
    assert!(msg.attachments.is_empty());
}

#[test]
fn lark_decode_sender_is_open_id_not_chat_id() {
    // The core ACL fix: a populated daemon-layer `lark_user_ids` (open_id
    // space, per acl.rs) can only ever match if the channel hands the
    // daemon the sender open_id — NOT the oc_ chat id. Guard it directly.
    let ch = LarkChannel::new("id".into(), "secret".into(), vec!["*".into()], true);
    let payload = serde_json::json!({
        "header": { "event_type": "im.message.receive_v1" },
        "event": {
            "sender": { "sender_id": { "open_id": "ou_alice" } },
            "message": {
                "message_id": "om_1",
                "message_type": "text",
                "content": "{\"text\":\"hi\"}",
                "chat_id": "oc_room",
                "create_time": "1000"
            }
        }
    });
    let msg = ch.decode_event_value(&payload).expect("decoded");
    assert_eq!(msg.sender, "ou_alice", "sender must be the user open_id");
    assert_ne!(
        msg.sender, "oc_room",
        "sender must NOT collapse into the chat id"
    );
    assert_eq!(msg.reply_target, "oc_room");
}

#[test]
fn lark_decode_unauthorized_user() {
    let ch = make_channel();
    let payload = serde_json::json!({
        "header": { "event_type": "im.message.receive_v1" },
        "event": {
            "sender": { "sender_id": { "open_id": "ou_unauthorized" } },
            "message": {
                "message_id": "om_1",
                "message_type": "text",
                "content": "{\"text\":\"spam\"}",
                "chat_id": "oc_chat",
                "create_time": "1000"
            }
        }
    });
    assert!(ch.decode_event_value(&payload).is_none());
}

#[test]
fn lark_decode_skips_app_and_bot_senders() {
    // The bot's own echoes (sender_type app/bot) must never loop back in,
    // even with a wildcard allowlist.
    let ch = LarkChannel::new("id".into(), "secret".into(), vec!["*".into()], true);
    for sender_type in ["app", "bot"] {
        let payload = serde_json::json!({
            "header": { "event_type": "im.message.receive_v1" },
            "event": {
                "sender": {
                    "sender_id": { "open_id": "ou_user" },
                    "sender_type": sender_type
                },
                "message": {
                    "message_id": "om_1",
                    "message_type": "text",
                    "content": "{\"text\":\"echo\"}",
                    "chat_id": "oc_chat",
                    "create_time": "1000"
                }
            }
        });
        assert!(
            ch.decode_event_value(&payload).is_none(),
            "sender_type={sender_type} must be dropped"
        );
    }
}

#[test]
fn lark_decode_group_requires_at_mention() {
    let ch = LarkChannel::new("id".into(), "secret".into(), vec!["*".into()], true);
    let base = |mentions: serde_json::Value| {
        serde_json::json!({
            "header": { "event_type": "im.message.receive_v1" },
            "event": {
                "sender": { "sender_id": { "open_id": "ou_user" } },
                "message": {
                    "message_id": "om_grp",
                    "message_type": "text",
                    "content": "{\"text\":\"hi team\"}",
                    "chat_id": "oc_group",
                    "chat_type": "group",
                    "mentions": mentions,
                    "create_time": "1000"
                }
            }
        })
    };
    // No @-mention in a group → dropped.
    assert!(ch
        .decode_event_value(&base(serde_json::json!([])))
        .is_none());
    // @-mentioned → delivered.
    let msg = ch
        .decode_event_value(&base(serde_json::json!([{"key": "@_user_1"}])))
        .expect("mentioned group message delivered");
    assert_eq!(msg.reply_target, "oc_group");
}

#[test]
fn lark_decode_unsupported_type_skipped() {
    // image/file/audio/media are now ingested (covered below); a genuinely
    // unsupported type (sticker / system) is still dropped.
    let ch = LarkChannel::new("id".into(), "secret".into(), vec!["*".into()], true);
    let payload = serde_json::json!({
        "header": { "event_type": "im.message.receive_v1" },
        "event": {
            "sender": { "sender_id": { "open_id": "ou_user" } },
            "message": {
                "message_id": "om_1",
                "message_type": "sticker",
                "content": "{}",
                "chat_id": "oc_chat"
            }
        }
    });
    assert!(ch.decode_event_value(&payload).is_none());
}

#[test]
fn lark_decode_image_without_key_skipped() {
    // A malformed image event missing its image_key is skipped, not surfaced
    // as an empty-content turn.
    let ch = LarkChannel::new("id".into(), "secret".into(), vec!["*".into()], true);
    let payload = serde_json::json!({
        "header": { "event_type": "im.message.receive_v1" },
        "event": {
            "sender": { "sender_id": { "open_id": "ou_user" } },
            "message": {
                "message_id": "om_1",
                "message_type": "image",
                "content": "{}",
                "chat_id": "oc_chat"
            }
        }
    });
    assert!(ch.decode_event_value(&payload).is_none());
}

#[test]
fn lark_decode_empty_text_skipped() {
    let ch = LarkChannel::new("id".into(), "secret".into(), vec!["*".into()], true);
    let payload = serde_json::json!({
        "header": { "event_type": "im.message.receive_v1" },
        "event": {
            "sender": { "sender_id": { "open_id": "ou_user" } },
            "message": {
                "message_id": "om_1",
                "message_type": "text",
                "content": "{\"text\":\"\"}",
                "chat_id": "oc_chat"
            }
        }
    });
    assert!(ch.decode_event_value(&payload).is_none());
}

#[test]
fn lark_decode_wrong_event_type() {
    let ch = make_channel();
    let payload = serde_json::json!({
        "header": { "event_type": "im.chat.disbanded_v1" },
        "event": {}
    });
    assert!(ch.decode_event_value(&payload).is_none());
}

#[test]
fn lark_decode_missing_sender() {
    let ch = LarkChannel::new("id".into(), "secret".into(), vec!["*".into()], true);
    let payload = serde_json::json!({
        "header": { "event_type": "im.message.receive_v1" },
        "event": {
            "message": {
                "message_id": "om_1",
                "message_type": "text",
                "content": "{\"text\":\"hello\"}",
                "chat_id": "oc_chat"
            }
        }
    });
    assert!(ch.decode_event_value(&payload).is_none());
}

#[test]
fn lark_decode_unicode_message() {
    let ch = LarkChannel::new("id".into(), "secret".into(), vec!["*".into()], true);
    let payload = serde_json::json!({
        "header": { "event_type": "im.message.receive_v1" },
        "event": {
            "sender": { "sender_id": { "open_id": "ou_user" } },
            "message": {
                "message_id": "om_1",
                "message_type": "text",
                "content": "{\"text\":\"Hello world 🌍\"}",
                "chat_id": "oc_chat",
                "create_time": "1000"
            }
        }
    });
    let msg = ch.decode_event_value(&payload).expect("decoded");
    assert_eq!(msg.content, "Hello world 🌍");
}

#[test]
fn lark_decode_missing_event() {
    let ch = make_channel();
    let payload = serde_json::json!({
        "header": { "event_type": "im.message.receive_v1" }
    });
    assert!(ch.decode_event_value(&payload).is_none());
}

#[test]
fn lark_decode_invalid_content_json() {
    let ch = LarkChannel::new("id".into(), "secret".into(), vec!["*".into()], true);
    let payload = serde_json::json!({
        "header": { "event_type": "im.message.receive_v1" },
        "event": {
            "sender": { "sender_id": { "open_id": "ou_user" } },
            "message": {
                "message_id": "om_1",
                "message_type": "text",
                "content": "not valid json",
                "chat_id": "oc_chat"
            }
        }
    });
    assert!(ch.decode_event_value(&payload).is_none());
}

#[test]
fn lark_decode_empty_object_returns_none() {
    let ch = make_channel();
    assert!(ch.decode_event_value(&serde_json::json!({})).is_none());
}

#[test]
fn lark_decode_empty_sender_returns_none() {
    let ch = make_channel();
    let payload = serde_json::json!({
        "header": { "event_type": "im.message.receive_v1" },
        "event": {
            "sender": { "sender_id": { "open_id": "" } },
            "message": {
                "message_id": "om_1",
                "message_type": "text",
                "content": r#"{"text":"hi"}"#,
                "chat_id": "oc_chat",
                "create_time": "1700000000000"
            }
        }
    });
    assert!(ch.decode_event_value(&payload).is_none());
}

#[test]
fn lark_decode_post_type_extracts_readable_text() {
    let ch = make_channel();
    let post_content = serde_json::json!({
        "zh_cn": {
            "title": "Title",
            "content": [[{"tag":"text","text":"Body"}]]
        }
    })
    .to_string();
    let payload = serde_json::json!({
        "header": { "event_type": "im.message.receive_v1" },
        "event": {
            "sender": { "sender_id": { "open_id": "ou_testuser123" } },
            "message": {
                "message_id": "om_post",
                "message_type": "post",
                "content": post_content,
                "create_time": "1700000000000",
                "chat_id": "oc_chat_xyz"
            }
        }
    });
    let msg = ch.decode_event_value(&payload).expect("decoded");
    assert!(msg.content.contains("Title"));
    assert_eq!(msg.sender, "ou_testuser123");
    assert_eq!(msg.reply_target, "oc_chat_xyz");
}

// ── decode_event (the live WS byte path: frame JSON → DecodedMessage) ───
//
// `decode_event` is exactly what `listen_ws` calls on each DATA frame's
// reassembled payload, so feeding it the bytes a real `im.message.receive_v1`
// frame carries proves the production decode end-to-end (the ACL + dedup
// that wrap it at the call site are covered by `decode_event_value` + the
// AclPolicy tests).

#[test]
fn lark_decode_event_bytes_maps_text_message() {
    let ch = make_channel();
    let event = serde_json::json!({
        "header": { "event_type": "im.message.receive_v1", "event_id": "evt_1" },
        "event": {
            "sender": { "sender_id": { "open_id": "ou_testuser123" } },
            "message": {
                "message_id": "om_bytes",
                "message_type": "text",
                "content": "{\"text\":\"from the wire @_user_1\"}",
                "chat_id": "oc_wire",
                "create_time": "1699999999000"
            }
        }
    });
    let bytes = serde_json::to_vec(&event).expect("serialize event");
    let decoded = ch.decode_event(&bytes).expect("decoded from bytes");
    assert_eq!(decoded.open_id, "ou_testuser123");
    assert_eq!(decoded.chat_id, "oc_wire");
    assert_eq!(decoded.message_id, "om_bytes");
    // @-placeholder stripped + trimmed, identical to the seam path.
    assert_eq!(decoded.text, "from the wire");
    let msg = decoded.into_channel_message();
    assert_eq!(msg.id, "lark-om_bytes");
    assert_eq!(msg.sender, "ou_testuser123");
    assert_eq!(msg.reply_target, "oc_wire");
}

#[test]
fn lark_decode_event_bytes_rejects_wrong_event_type() {
    let ch = make_channel();
    let event = serde_json::json!({
        "header": { "event_type": "im.chat.disbanded_v1", "event_id": "evt_2" },
        "event": {}
    });
    let bytes = serde_json::to_vec(&event).expect("serialize");
    assert!(ch.decode_event(&bytes).is_none());
}

#[test]
fn lark_decode_event_bytes_rejects_garbage() {
    let ch = make_channel();
    assert!(ch.decode_event(b"not json at all").is_none());
}

// ── parse_post_content ─────────────────────────────────────────

#[test]
fn parse_post_content_returns_zh_cn_locale_content() {
    let post = serde_json::json!({
        "zh_cn": {
            "title": "标题",
            "content": [[{"tag": "text", "text": "你好"}]]
        }
    })
    .to_string();
    let out = parse_post_content(&post).expect("parsed");
    assert!(out.contains("标题"));
    assert!(out.contains("你好"));
}

#[test]
fn parse_post_content_falls_back_to_en_us_when_zh_cn_missing() {
    let post = serde_json::json!({
        "en_us": {
            "title": "Hello",
            "content": [[{"tag": "text", "text": "world"}]]
        }
    })
    .to_string();
    let out = parse_post_content(&post).expect("parsed");
    assert!(out.contains("Hello"));
    assert!(out.contains("world"));
}

#[test]
fn parse_post_content_returns_none_for_invalid_json() {
    assert!(parse_post_content("not json").is_none());
}

#[test]
fn parse_post_content_handles_links_and_mentions() {
    let post = serde_json::json!({
        "zh_cn": {
            "title": "T",
            "content": [[
                {"tag": "text", "text": "pre "},
                {"tag": "a", "text": "link", "href": "https://x"},
                {"tag": "at", "user_name": "alice"}
            ]]
        }
    })
    .to_string();
    let out = parse_post_content(&post).expect("parsed");
    assert!(out.contains("link"));
    assert!(out.contains("@alice"));
}

#[test]
fn parse_post_content_falls_back_to_href_when_anchor_text_missing() {
    // Anchor without `text` must surface the `href` — otherwise the
    // link is invisible in the rendered message.
    let post = serde_json::json!({
        "zh_cn": {
            "title": "T",
            "content": [[
                {"tag": "text", "text": "see "},
                {"tag": "a", "href": "https://example.com/no-text"}
            ]]
        }
    })
    .to_string();
    let out = parse_post_content(&post).expect("parsed");
    assert!(
        out.contains("https://example.com/no-text"),
        "href fallback should surface when anchor has no text, got: {out}"
    );
}

#[test]
fn parse_post_content_returns_none_when_all_sections_empty() {
    let post = serde_json::json!({ "zh_cn": { "title": "" } }).to_string();
    assert!(parse_post_content(&post).is_none());
}

// ── strip_at_placeholders ──────────────────────────────────────

#[test]
fn strip_at_placeholders_removes_user_tokens() {
    assert_eq!(strip_at_placeholders("hello @_user_1 world"), "hello world");
    assert_eq!(
        strip_at_placeholders("@_user_42 message here"),
        "message here"
    );
}

#[test]
fn strip_at_placeholders_preserves_real_at_mentions() {
    assert_eq!(strip_at_placeholders("hello @alice"), "hello @alice");
}

#[test]
fn strip_at_placeholders_handles_multiple_placeholders() {
    assert_eq!(strip_at_placeholders("@_user_1 hi @_user_2 bye"), "hi bye");
}

// ── should_respond_in_group ────────────────────────────────────

#[test]
fn should_respond_in_group_requires_nonempty_mentions() {
    assert!(!should_respond_in_group(&[]));
    assert!(should_respond_in_group(&[
        serde_json::json!({"key": "val"})
    ]));
}

// ── attachments: pick_lark_attachment (inbound resource descriptor) ─────

#[test]
fn pick_lark_attachment_image_keys_on_image_key() {
    let p = pick_lark_attachment("image", r#"{"image_key":"img_v2_abc"}"#).expect("image pending");
    assert_eq!(p.key, "img_v2_abc");
    assert_eq!(p.kind, AttachmentKind::Image);
    // Images carry no usable wire name; the real extension is sniffed later.
    assert_eq!(p.file_name, "image");
}

#[test]
fn pick_lark_attachment_file_keeps_real_name() {
    let p = pick_lark_attachment(
        "file",
        r#"{"file_key":"file_v2_xyz","file_name":"report.pdf"}"#,
    )
    .expect("file pending");
    assert_eq!(p.key, "file_v2_xyz");
    assert_eq!(p.kind, AttachmentKind::File);
    assert_eq!(p.file_name, "report.pdf");
}

#[test]
fn pick_lark_attachment_audio_media_default_names() {
    // audio/media key on file_key and fall back to a typed default name when
    // the event omits file_name.
    let a = pick_lark_attachment("audio", r#"{"file_key":"file_a"}"#).expect("audio pending");
    assert_eq!(a.kind, AttachmentKind::File);
    assert_eq!(a.file_name, "audio.opus");
    let m = pick_lark_attachment("media", r#"{"file_key":"file_m"}"#).expect("media pending");
    assert_eq!(m.file_name, "video.mp4");
}

#[test]
fn pick_lark_attachment_none_for_text_or_missing_key() {
    assert!(pick_lark_attachment("text", r#"{"text":"hi"}"#).is_none());
    assert!(pick_lark_attachment("image", "{}").is_none());
    assert!(pick_lark_attachment("file", "{}").is_none());
    assert!(pick_lark_attachment("sticker", "{}").is_none());
}

// ── attachments: decode → DecodedMessage.pending (the live byte seam) ───

#[test]
fn lark_decode_image_yields_pending_no_text() {
    let ch = make_channel();
    let event = serde_json::json!({
        "header": { "event_type": "im.message.receive_v1" },
        "event": {
            "sender": { "sender_id": { "open_id": "ou_testuser123" } },
            "message": {
                "message_id": "om_img",
                "message_type": "image",
                "content": "{\"image_key\":\"img_v2_x\"}",
                "chat_id": "oc_c",
                "create_time": "1000"
            }
        }
    });
    let bytes = serde_json::to_vec(&event).unwrap();
    let decoded = ch.decode_event(&bytes).expect("image decoded");
    assert!(decoded.text.is_empty(), "a bare image carries no text");
    let p = decoded.pending.expect("image yields a pending download");
    assert_eq!(p.key, "img_v2_x");
    assert_eq!(p.kind, AttachmentKind::Image);
}

#[test]
fn lark_decode_file_yields_pending() {
    let ch = make_channel();
    let event = serde_json::json!({
        "header": { "event_type": "im.message.receive_v1" },
        "event": {
            "sender": { "sender_id": { "open_id": "ou_testuser123" } },
            "message": {
                "message_id": "om_file",
                "message_type": "file",
                "content": "{\"file_key\":\"file_v2_y\",\"file_name\":\"log.txt\"}",
                "chat_id": "oc_c",
                "create_time": "1000"
            }
        }
    });
    let bytes = serde_json::to_vec(&event).unwrap();
    let decoded = ch.decode_event(&bytes).expect("file decoded");
    let p = decoded.pending.expect("file yields a pending download");
    assert_eq!(p.key, "file_v2_y");
    assert_eq!(p.kind, AttachmentKind::File);
    assert_eq!(p.file_name, "log.txt");
}

#[test]
fn lark_decode_group_image_requires_at_mention() {
    // The group @-mention gate applies to attachments too.
    let ch = LarkChannel::new("id".into(), "secret".into(), vec!["*".into()], true);
    let event = |mentions: serde_json::Value| {
        serde_json::to_vec(&serde_json::json!({
            "header": { "event_type": "im.message.receive_v1" },
            "event": {
                "sender": { "sender_id": { "open_id": "ou_user" } },
                "message": {
                    "message_id": "om_gi",
                    "message_type": "image",
                    "content": "{\"image_key\":\"img_v2_g\"}",
                    "chat_id": "oc_group",
                    "chat_type": "group",
                    "mentions": mentions,
                    "create_time": "1000"
                }
            }
        }))
        .unwrap()
    };
    assert!(ch.decode_event(&event(serde_json::json!([]))).is_none());
    assert!(ch
        .decode_event(&event(serde_json::json!([{"key": "@_user_1"}])))
        .is_some());
}

// ── attachments: upload-response + magic-byte helpers (pure) ────────────

#[test]
fn parse_resource_key_extracts_data_key() {
    let v = serde_json::json!({ "code": 0, "data": { "image_key": "img_v2_z" } });
    assert_eq!(
        parse_resource_key(&v, "image_key").as_deref(),
        Some("img_v2_z")
    );
    let f = serde_json::json!({ "code": 0, "data": { "file_key": "file_v2_z" } });
    assert_eq!(
        parse_resource_key(&f, "file_key").as_deref(),
        Some("file_v2_z")
    );
}

#[test]
fn parse_resource_key_none_when_missing() {
    assert!(parse_resource_key(&serde_json::json!({"data": {}}), "image_key").is_none());
    assert!(parse_resource_key(&serde_json::json!({}), "file_key").is_none());
}

#[test]
fn json_content_str_double_parses_stringified_content() {
    // Feishu message `content` is itself a JSON string.
    assert_eq!(
        json_content_str(r#"{"file_name":"a.pdf"}"#, "file_name").as_deref(),
        Some("a.pdf")
    );
    assert!(json_content_str("not json", "x").is_none());
    assert!(json_content_str(r#"{"a":1}"#, "missing").is_none());
}

#[test]
fn image_ext_sniffs_magic_bytes() {
    assert_eq!(image_ext(&[0x89, b'P', b'N', b'G', 0x0d]), ".png");
    assert_eq!(image_ext(&[0xFF, 0xD8, 0xFF, 0xE0]), ".jpg");
    assert_eq!(image_ext(b"GIF89a"), ".gif");
    let mut webp = b"RIFF".to_vec();
    webp.extend_from_slice(&[0, 0, 0, 0]);
    webp.extend_from_slice(b"WEBP");
    assert_eq!(image_ext(&webp), ".webp");
    // Unknown/empty defaults to .jpg (Feishu images are usually JPEG).
    assert_eq!(image_ext(b"\x00\x01\x02"), ".jpg");
    assert_eq!(image_ext(&[]), ".jpg");
}

// ── interactive card (options → buttons) round-trip ─────────────────────────

#[test]
fn build_option_card_renders_text_and_one_button_per_option() {
    let opts = vec![
        MessageOption {
            data: "nav:cd:alpha".into(),
            label: "✓ alpha".into(),
            id: "alpha".into(),
        },
        MessageOption {
            data: "nav:cd:beta".into(),
            label: "▸ beta".into(),
            id: "beta".into(),
        },
    ];
    let card = build_option_card("Pick a project", &opts);
    let elements = card["elements"].as_array().expect("elements array");
    // One `div` for the text + one `action` per option.
    assert_eq!(elements.len(), 3);
    assert_eq!(elements[0]["tag"], "div");
    assert_eq!(elements[0]["text"]["content"], "Pick a project");
    // Each button carries the option's opaque `data` under `value.d`, and
    // its human label — the same split telegram rides on callback_data/text.
    assert_eq!(elements[1]["tag"], "action");
    let btn1 = &elements[1]["actions"][0];
    assert_eq!(btn1["tag"], "button");
    assert_eq!(btn1["text"]["content"], "✓ alpha");
    assert_eq!(btn1["value"]["d"], "nav:cd:alpha");
    let btn2 = &elements[2]["actions"][0];
    assert_eq!(btn2["value"]["d"], "nav:cd:beta");
    assert_eq!(btn2["text"]["content"], "▸ beta");
}

#[test]
fn build_option_card_omits_empty_text_div() {
    let opts = vec![MessageOption {
        data: "t:0".into(),
        label: "Yes".into(),
        id: "yes".into(),
    }];
    let card = build_option_card("", &opts);
    let elements = card["elements"].as_array().expect("elements array");
    // No leading text div when content is empty — just the one button.
    assert_eq!(elements.len(), 1);
    assert_eq!(elements[0]["tag"], "action");
}

#[test]
fn decode_card_action_legacy_flat_shape() {
    // Legacy card callback: fields flat at the top level, button value we set.
    let payload = serde_json::json!({
        "open_id": "ou_testuser123",
        "open_chat_id": "oc_conv1",
        "open_message_id": "om_msg1",
        "token": "c-abc",
        "action": { "tag": "button", "value": { "d": "nav:use:s7" } },
    });
    let action = decode_card_action(payload.to_string().as_bytes()).expect("decoded");
    assert_eq!(action.open_id, "ou_testuser123");
    assert_eq!(action.chat_id, "oc_conv1");
    assert_eq!(action.message_id, "om_msg1");
    assert_eq!(action.data, "nav:use:s7");
}

#[test]
fn decode_card_action_card_2_0_event_shape() {
    // card.action.trigger: fields nested under `event` with operator/context.
    let payload = serde_json::json!({
        "schema": "2.0",
        "header": { "event_type": "card.action.trigger" },
        "event": {
            "operator": { "open_id": "ou_testuser123" },
            "context": { "open_chat_id": "oc_conv2", "open_message_id": "om_msg2" },
            "action": { "tag": "button", "value": { "d": "nav:cd:gamma" } },
        },
    });
    assert!(payload_is_card_action(payload.to_string().as_bytes()));
    let action = decode_card_action(payload.to_string().as_bytes()).expect("decoded");
    assert_eq!(action.open_id, "ou_testuser123");
    assert_eq!(action.chat_id, "oc_conv2");
    assert_eq!(action.message_id, "om_msg2");
    assert_eq!(action.data, "nav:cd:gamma");
}

#[test]
fn decode_card_action_rejects_non_card_payload() {
    // A plain message event carries no button value → None (falls through to
    // the message decoder in the live loop).
    let msg = serde_json::json!({
        "schema": "2.0",
        "header": { "event_type": "im.message.receive_v1" },
        "event": {
            "sender": { "sender_id": { "open_id": "ou_x" }, "sender_type": "user" },
            "message": { "message_type": "text", "content": "{\"text\":\"hi\"}" },
        },
    });
    assert!(!payload_is_card_action(msg.to_string().as_bytes()));
    assert!(decode_card_action(msg.to_string().as_bytes()).is_none());
}

#[test]
fn decode_card_action_requires_open_id_and_data() {
    // Missing button value → None even with a valid operator.
    let no_data = serde_json::json!({
        "open_id": "ou_x",
        "action": { "tag": "button", "value": {} },
    });
    assert!(decode_card_action(no_data.to_string().as_bytes()).is_none());
    // Missing operator → None even with a value.
    let no_op = serde_json::json!({
        "action": { "tag": "button", "value": { "d": "t:0" } },
    });
    assert!(decode_card_action(no_op.to_string().as_bytes()).is_none());
}
