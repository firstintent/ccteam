---
name: ccgram-messaging
description: Inter-agent messaging — check inbox, send messages, discover peers, broadcast, and spawn agents. Use when idle, when you need help from another agent, or when you want to share status.
---

# Inter-Agent Messaging

You are part of a multi-agent swarm managed by ccgram. Other agents may send you messages. Use these commands to collaborate.

## On Start

Register yourself so other agents can find you:

```bash
ccgram msg register --task "brief description of your current task" --team "team-name"
```

## On Idle (after completing a task or waiting)

Check your inbox for messages from other agents:

```bash
ccgram msg inbox
```

IMPORTANT: When you have peer messages, summarize them to the user first and ask before processing:
"I have N messages from other agents. Here's a summary: [summary]. Should I handle these?"

Exception: if you were spawned with --auto (no user topic), process messages immediately without asking.

## Sending Messages

Find peers:

```bash
ccgram msg list-peers
ccgram msg find --team backend --provider claude
```

Send a message (returns immediately):

```bash
ccgram msg send <peer-id> "your message" --subject "topic"
```

Send and wait for a reply (blocks until reply or timeout):

```bash
ccgram msg send <peer-id> "question?" --wait
```

Reply to a received message:

```bash
ccgram msg reply <msg-id> "your answer"
```

## Broadcasting

Send a notification to all matching peers:

```bash
ccgram msg broadcast "status update" --team backend
ccgram msg broadcast "breaking change in API" --provider claude
```

## Spawning New Agents

Request a new agent for a specific task:

```bash
ccgram msg spawn --provider claude --cwd ~/project --prompt "implement feature X"
```

This requires human approval via Telegram unless --auto is set.
