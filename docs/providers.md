# Providers

Quota Bar ships with six collectors. Every one is optional, and every one runs
only when its provider is enabled. A missing CLI or a signed-out account turns
into one red line on that card, not a broken app.

| Id | Menu bar | Card |
| --- | --- | --- |
| `antigravity` | `ag` | Antigravity |
| `claude` | `cc` | Claude Code |
| `codex` | `cx` | Codex |
| `grok` | `gk` | Grok Build |
| `grokbot` | `gb` | Grok Bot |
| `opencode` | `oc` | OpenCode Go |

The ids are stable and are what `config.json` uses. Missing ids are enabled.

## Antigravity

Runs the Antigravity CLI:

```sh
agy -p "/quota" --output-format json
agy -p "/credits" --output-format json
```

The first command supplies the quota windows (weekly, five-hour and their
`3P` variants); the second adds the credit balance shown on the card. Sign in
with the Antigravity CLI as usual. `agy` is looked up on `PATH` and in the
usual install locations (`~/.local/bin`, `/opt/homebrew/bin`, `/usr/local/bin`
and others).

## Claude Code

Runs:

```sh
claude -p "/usage" --output-format json
```

and parses the CLI's own summary. Remaining is `100 − used`. The card also
shows when the login credential expires. Sign in with `claude` first.

## Codex

Reads ChatGPT's usage endpoint directly:

```
GET https://chatgpt.com/backend-api/wham/usage
```

with the ChatGPT login in `~/.codex/auth.json` (or
`$CODEX_HOME/auth.json`). If that login is stale, Quota Bar falls back to
`codex app-server`, which refreshes it without starting a model turn. The
app-server exchange has a 15-second deadline.

`CODEX_BIN` overrides the `codex` binary. The card notes the plan and credit
balance (`Plus · credits 0`).

## Grok Build

`grok usage --json` currently refuses outside the TUI, so Quota Bar reads the
same billing endpoint its `/usage` view uses:

```
GET https://cli-chat-proxy.grok.com/v1/billing?format=credits
```

with the OIDC login in `~/.grok/auth.json` (or `$GROK_HOME/auth.json`). An
expired access token is refreshed with the stored refresh token and written
back, so the CLI stays signed in. No model turn is made.

## Grok Bot

Grok Bot is a Cursor product, so its weekly allowance comes from Cursor's
dashboard, not the Grok billing endpoint. Quota Bar reads Cursor's
`state.vscdb` with `sqlite3`, read-only, then asks:

```
POST https://api2.cursor.sh/aiserver.v1.DashboardService/GetSandUsageStatus
```

Cursor must be installed and signed in. The database is never modified.

Default database locations:

```
~/Library/Application Support/Cursor/User/globalStorage/state.vscdb   # macOS
$XDG_CONFIG_HOME/Cursor/User/globalStorage/state.vscdb                # Linux
~/.config/Cursor/User/globalStorage/state.vscdb                       # Linux fallback
```

`CURSOR_STATE_VSCDB` overrides the path.

## OpenCode Go

Reads:

```
GET https://opencode.ai/zen/go/v1/usage
```

with the stored `oc_sk_…` key. `percent` is used, so the card shows
`100 − percent` as remaining. The key comes from `opencode auth export`, or
from `OPENCODE_GO_KEY` when set.

## Failures

A failing provider keeps its last numbers and adds the error in red, so a
temporary network blip does not erase what you were looking at. Hidden
providers are not fetched at all.

## Adding a provider

Add a collector in `src/quota.rs` and one entry to `PROVIDERS` there. The tray
menu, config file, footer and panel pick it up automatically. A collector
returns a `Source` with a short label, a title, a list of `Metric`s and an
optional note or error.

## A note on these endpoints

Codex, Grok Build, Grok Bot and OpenCode Go are read through the same internal
endpoints their own clients use, not public APIs, so they may change without
notice. Quota Bar never starts a model turn, never uploads your data, and only
writes a login back when Grok's token has expired. Cursor's database is opened
read-only.
