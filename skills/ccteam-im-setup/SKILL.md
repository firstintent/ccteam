---
name: ccteam-im-setup
description: "One-time IM token onboarding for ccteam chat bots. Walks the user through Telegram / Slack / Discord token registration + chat_id auto-detect, persists credentials to ~/.ccteam/im/credentials.json (0600), and optionally switches the backup transport between openhuman/channels and the official Anthropic plugin. Use when the user says '绑 TG token' / 'setup TG bot' / 'configure Slack' / '换 IM transport'."
---

# /ccteam-im-setup — one-time IM token onboarding

Standalone skill that any other skill (notably `ccteam-creator`
Phase 5.1) can invoke to make sure `~/.ccteam/im/credentials.json`
has the platform the rest of the flow needs. **Idempotent** —
re-running for a platform that's already configured re-verifies the
token and exits without prompting.

Backed by `ccteam_imd::onboarding::telegram_setup()`.

## When to invoke

- User says: "绑 TG token", "我要做 IM bot", "setup telegram",
  "我的 chat_id", "换 transport"
- `ccteam-creator` calls back into here from Phase 5.1 when chat
  credentials are missing
- Top-level `/ccteam` dispatcher's `configure-im` intent routes here

## When **not** to invoke

- User just wants to chat with an already-onboarded bot — that's
  the bot's own session, not this skill
- Token is already known to be valid and the user just changed
  bot persona — no re-onboarding needed

---

# Step 1 — Platform choice

```
> 要绑哪个 IM 平台?
  1. Telegram
```

目前只支持 Telegram。如果用户问起 Slack / Discord / Lark / DingTalk
/ WeChat,礼貌告诉用户当前只支持 Telegram + 询问"要先用 Telegram 吗?"
不要 promise 任何时间表。

---

# Step 2 — Telegram onboarding

Walk the user through, **one step at a time** — don't dump the
whole script:

### 2a. Get a token from @BotFather

```
> 我打不开浏览器,但你这边照做:
> 1. 在 Telegram 里搜 @BotFather,开聊
> 2. 发 `/newbot`,起个 bot 名字(你能看到的展示名,如 "我的助理")
> 3. 起个 bot username,要 `_bot` 结尾(如 `my_helper_bot`)
> 4. @BotFather 会回一段类似 `1234567890:ABCdefGHI...` 的 token
> 5. 把那段 token 粘给我,我帮你验证
```

Wait for the user to paste a token.

### 2b. Verify token (calls `telegram_setup`)

Call `ccteam_imd::onboarding::telegram_setup(token, poll_seconds=120)`.

The function:
1. Hits `https://api.telegram.org/bot<token>/getMe` to verify the
   token is valid and extract the bot's `@username`.
2. Long-polls `getUpdates` for up to `poll_seconds` waiting for the
   first incoming message — used to capture the user's `chat_id`.

While it's running, tell the user:

```
> ✓ Token 验证通过,bot 是 <@<bot_username>>
>
> 现在在手机 Telegram 里搜 <@<bot_username>>,私聊发一条 `hello`
> (任何文字都行)。我在等你的第一条消息,2 分钟内发就行。
```

### 2c. On `chat_id` capture

```
> ✓ 抓到了 chat_id = <id>
> 
> 把凭证写到 ~/.ccteam/im/credentials.json (0600)…
> ✓ 写完了
> 
> Telegram 已绑定。下一步你想做啥?
>   - 起个 chat workflow:`/ccteam-creator "做个 TG 助理"`
>   - 切换 transport:`/ccteam-im-setup --transport official-telegram`
```

Persist via either of these (same behaviour):

```rust
ccteam_imd::credentials::save(
    &ccteam_imd::credentials::default_path(),
    &Credentials { telegram: Some(result.creds), ..Default::default() },
)?;

// or the convenience wrapper:
ccteam_imd::credentials::write_credentials(
    &Credentials { telegram: Some(result.creds), ..Default::default() },
)?;
```

`result.bot_username` (carried separately on `TelegramSetupResult`,
not on the on-disk `TelegramCreds`) is used for the user-facing
"在 TG 找 @xxx" reply line — it's not persisted to credentials.json.

### 2d. Error handling

- **`OnboardingError::Http`** → "Telegram API 没通,你的网络能上
  api.telegram.org 吗?试试 `curl https://api.telegram.org`。"
- **`OnboardingError::ApiNotOk("getMe")`** → "Token 不对,@BotFather
  那边再 `/token` 一次,或重新 `/newbot`。"
- **`OnboardingError::NoIncomingMessage`** → "我等了 2 分钟没收到
  消息。手机 Telegram 找 `@<bot_username>`,发条 hello,再来一次:
  `/ccteam-im-setup`(token 我已经记着了不用再粘)。"

---

# Step 3 — Persistence

The credentials file lives at `~/.ccteam/im/credentials.json` with
`0600` (read/write owner only). Schema:

```jsonc
{
  "telegram": {
    "bot_token": "1234:abcd...",
    "allowed_chat_ids": ["987654321"]
  }
}
```

(`bot_username` is **not** persisted — it's a transient field on the
in-memory `TelegramSetupResult`. The daemon only needs the token +
chat_id ACL.)

**Do not**:
- write the token into env vars (users shouldn't have to learn `.bashrc`)
- write the token into project `workflow.yaml` (R8 red line + security)
- log the token to stdout (mask it past the first 6 chars when echoing back)

---

# Step 4 — Backup transport switch (optional)

The default chat transport is `openhuman/channels` (uniform code
path across all IM platforms). If the user wants the official
Anthropic Telegram plugin path instead, they can run:

```
/ccteam-im-setup --transport official-telegram
```

This writes `~/.ccteam/im/transport.toml`:

```toml
[transport]
preferred = "official-telegram"   # or "openhuman" (default)
```

When `preferred == "official-telegram"`, the daemon spawns chat
sessions with `--channels plugin:telegram@claude-plugins-official`
instead of the openhuman bridge.

Reasons to switch:
- User prefers an all-Anthropic stack
- openhuman is misbehaving and they want a fallback

Reasons not to switch:
- official path only covers Telegram
- bot-to-bot @ addressing (IM Squad) currently only works via
  openhuman

---

# What this skill does NOT do

- OAuth flow — token-only (Telegram bot tokens)
- Webhook URL auto-setup — user runs ngrok / cloudflared themselves;
  document in `docs/troubleshooting.md`
- Multi-account per platform — one bot per platform
- Token rotation / refresh — manual
