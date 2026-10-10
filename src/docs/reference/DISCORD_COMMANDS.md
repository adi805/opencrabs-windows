# Discord application commands (slash commands)

Issue: [#1850](https://github.com/opencrabs/opencrabs/issues/1850): OpenCrabs has no Discord slash commands.

OpenCrabs projects `commands.toml` onto Discord's native slash-command list for
every guild the bot is in, so a command defined in that file appears in the
client's `/` menu with its description and an argument hint. It also registers
two right-click context menus, which are a separate command type and cost the
catalog nothing (see below). The scope of the slash-command projection is
`commands.toml` only: Telegram's menu also lists built-in commands and skills,
Discord's does not (#1850 named the file, not the whole catalog), and a skill
typed as plain text still works through the message path.
## Two different axes: intents and scopes

Both must be right or nothing works, and they fail in different ways.

**Gateway intents** are what events the bot is *told* about. OpenCrabs requests
`GUILD_MESSAGES | DIRECT_MESSAGES | MESSAGE_CONTENT | GUILD_MESSAGE_REACTIONS |
DIRECT_MESSAGE_REACTIONS | GUILD_MEMBERS` (`src/channels/discord/agent.rs`).
Intents are set in the Developer Portal under **Bot → Privileged Gateway
Intents**. Two of them are privileged and must be enabled there or Discord
refuses the IDENTIFY: `MESSAGE_CONTENT`, without which the bot receives empty
message content and looks deaf, and `GUILD_MEMBERS`, which carries member-join
events (FR-004, the welcome message). OpenCrabs names the missing toggle in the
log and stops instead of reconnecting forever when that refusal happens.
Intents have nothing to do with slash commands: leaving them alone does not
break `/`, and enabling `applications.commands` is not an intent.

**OAuth2 scopes** are what the bot is *allowed to do*, and they are granted at
invite time, not in the portal. Slash commands need:

```
bot applications.commands
```

If the bot was invited with `bot` only, the gateway connects, messages flow, and
every call to the command-registration endpoint returns **403**. This is the
single most common reason `/` is empty on a working OpenCrabs Discord install:
the bot looks healthy everywhere else.

## Re-inviting with the missing scope

The scope cannot be added from the portal. Re-run the invite URL:

```
https://discord.com/oauth2/authorize?client_id=<APPLICATION_ID>&scope=bot%20applications.commands&permissions=68608
```

Open it, pick the guild, Authorize. Then restart OpenCrabs, or wait: a sync where
every guild refused is **not** remembered as done, so the next reconnect tries
again and the newly granted scope is picked up without touching any config. A
reconnect alone is the reliable path, since Discord fires no new `READY` event for
an existing connection when you re-authorize it.

## How commands are chosen

`src/channels/discord/commands.rs` reads the catalog through the same
`CommandLoader` every channel uses, so entries added at runtime by the agent
appear on Discord too: the config watcher re-plans on every publish. The scope is
`commands.toml`; Telegram's menu additionally lists built-ins and skills, and this
projection does not. Names and descriptions that Discord would reject are
sanitized rather than dropped:

| Rule | Discord limit | What OpenCrabs does |
|------|---------------|---------------------|
| Name charset | `^[\w-]{1,32}$`, lowercase | lowercases, maps illegal characters to `-`, trims edge dashes |
| Name length | 32 characters | truncates |
| Description | 100 characters | truncates |
| Empty description | required for CHAT_INPUT | falls back to the command name |
| Commands per guild | 100 CHAT_INPUT | keeps the head of `commands.toml`, logs the dropped tail |
| Whole tree | 8000 characters | drops the entry that would cross the line, logs it |

Name collisions after sanitizing are **dropped**, not merged, and the log names
both the loser and the winner: silently merging `/foo bar` and `/foo-bar` would
run one command where the user believes they have two.

## Typing-time suggestions (autocomplete)

Discord only asks for suggestions (interaction type 4) when the option was
registered with `set_autocomplete(true)`, and it expects an answer inside the
same three-second window as any other interaction. Both halves read one table,
`autocomplete::catalog_for`, so what is registered and what is answered cannot
drift.

| Command | Suggestions come from |
|---------|-----------------------|
| `/provider`, `/providers` | the provider ids this config has configured |
| `/model`, `/models` | the models those providers offer |
| `/session`, `/sessions`, `/resume` | titles in the session store |

Every other command keeps the plain text option, which is what the client shows
today. Adding `/models` or `/sessions` to `commands.toml` lights up its
suggestions with no code change, because the command name is the only signal
available at interaction time: every command is registered with the same single
`args` string, so nothing in the registration distinguishes "pick a model" from
"paste an argument".

Three requests answer with an **empty list** rather than an error: a command
with no catalog, an option that is not the enumerable one, and a catalog that
cannot be read. Discord renders an empty list as "no suggestions", while a
failed interaction puts an error in front of the user for a keystroke.

Ranking puts a prefix match ahead of a coincidence and keeps catalog order
inside each bucket, so the same keystroke always produces the same list. The
response is capped at Discord's 25 choices and 100-character name budget, and
the name is clipped by characters so a multi-byte session title is never split
mid-codepoint.

## How a slash command is executed

The interaction handler rebuilds the invocation as the text the user would have
typed (`/name args`) and routes it through the tool-loop display path that a
tapped suggestion uses (#1852), not the bare interaction router that modal forms
and select menus ride. The difference is not cosmetic: the bare route is a
single completion with no tool loop, which is correct for a form fill (a
synthetic steering prompt) and useless for `/check`, whose whole purpose is to
make the agent run cargo. The handler does **not** call the `slash_command` tool
from the channel layer, so:

- whatever the agent would do with the typed text is what it does here, because
  it is handed the same string;
- arguments survive, because they are inside the rebuilt text;
- history records the command the same way it records a typed message, so
  nothing downstream can tell the difference, which is the point.

Every command is registered with a single optional string argument (`args`),
because the catalog's commands take free-form text rather than typed parameters.

## Right-click commands (context menus)

Two global context menus ride the same path as a picked slash command, so they
get the same deny-by-default gate, the same deferred acknowledgement and the
same turn router. Only the request text differs:

| Menu | Applies to | What it sends |
|------|-----------|---------------|
| `Ask agent` | a message | the author's name and id, the message link, and the content verbatim |
| `Ask agent about user` | a member | the member's id, with the display name for readability |

Both are registered in the **same global overwrite** as the command catalog.
`set_global_commands` replaces the whole global set, so registering the menus in
a call of their own would erase the catalog, and the next catalog sync would
erase them. Both components feed the sync key, so an edit to either one
re-syncs.

A context menu is a different kind of command, not a differently shaped
`CHAT_INPUT`: the name may be mixed case with spaces, the description must be
empty, and the budgets are separate (15 `USER`, 15 `MESSAGE`), so these two cost
the catalog nothing. The `^[\w-]{1,32}$` name rule applies to `CHAT_INPUT`
alone and must not be applied here.

A context menu is unavailable in a DM, so there is no DM branch to write. A
target the client named but did not resolve is how Discord reports a message
deleted between the right-click and the interaction landing: that answers an
ephemeral refusal in place rather than timing out, and no turn runs on a message
nobody can see.

## Where a long answer goes

Discord caps a message at 2000 characters, so a long answer has to be split.
Two routes exist and the order between them is the whole feature:

1. **Thread first.** An answer that clears `auto_thread_min_chars` (default
   `1800`, `0` disables) posts a short teaser in the channel and the full body
   in a thread anchored to the message. The teaser names the thread, so the
   answer stays reachable from the channel.
2. **Pager as fallback.** If thread creation is refused, the answer is chunked
   in place and page 0 carries the pager button (FR-009).

The decision runs **before** the pager and takes the answer length and the
threshold only, never the pager's page count. Gating the thread on "not paged"
is the bug this order exists to prevent: the pager claims every answer past 2000
characters, which is exactly the set of answers a thread is for, so a `!paged`
guard left threads reachable only in the narrow band between the threshold and
the page ceiling, and the answers that most need a thread could never get one.

Three cases are delivered in place, because a thread is not possible there: a
message that already arrived in a thread (Discord refuses to anchor a thread to
a thread), a `!bang` turn that opened its own thread, and a DM (no threads at
all). The channel-kind lookup runs only once the threshold is cleared, so a
short answer pays nothing for the feature.

## Who can run a command

Discord shows the command list to **every member of the guild**, so an invoked
command is a new way into the agent and gets the same deny-by-default gate
(OC-02) that `handle_message` applies to typed text:

| Situation | Result |
|-----------|--------|
| No `allowed_users`, no `allowed_roles`, no `bot_owner` | **unconfigured, denies everybody** (this is not "open to all") |
| The configured `bot_owner` | admitted |
| An id in `allowed_users` | admitted |
| A member holding a role in `allowed_roles` | admitted (guild only; a DM has no roles) |
| Anyone else | refused |

The refusal is an ephemeral message, so only the person who tapped it sees it,
plus a `warn` line naming which check failed. Channel scope travels with the
identity check: `allowed_channels` applies, including the parent fallback that
lets an allow-listed forum admit its posts. A command is solicited, so only a
channel's `dm_only` mode blocks it; that mode is read per channel, with the thread →
parent → global fallback (#2014). In a `mention` channel an unmentioned command is
dropped, except the owner's `/respond_to` and `/cowork`, which pass the gate (#2016).

## Per-channel settings and the owner commands

Each Discord channel, or a forum/thread parent, can have its own entry:

```toml
[channels.discord.channels.1473207147025137778]
name = "general"             # display only; access never reads it
respond_to = "all"           # this channel's mode; unset inherits the global respond_to
open = true                  # any member of this channel is admitted (ACL)
```

- **`open`** admits every member of the channel, and its threads and forum posts,
  past `allowed_users`. It never admits anyone while the bot has no
  `allowed_users`, `allowed_roles` or `bot_owner`. DMs and other channels stay locked.
- **`/respond_to`** (owner) typed in a channel or thread shows the mode that applies
  there. With an argument (`all`, `dm_only`, `mention`, `auto`) it writes that
  channel's own `respond_to`. Threads write their own entry, which wins over the parent's.
- **`/cowork`** (owner) in a server channel or thread writes `open = true` and the
  channel's `name`. It refuses in a DM. Members are not registered; `open` admits them.
- Both commands are refused for non-owners before anything is written, and a failed
  write is reported in the channel. Slack and WhatsApp answer `/respond_to` from their
  channel-level setting and do not write it.
- `auto` on Discord behaves as `mention`.

The verdict itself lives in `identity_admitted()` and `holds_allowed_role()` as
pure functions, which is what lets the deny-by-default case have a test: there
is no Discord application in CI, so an inline `if` in the gateway handler would
have shipped unproven.

## Rate limits that shaped the implementation

Discord's application-command limits are real and OpenCrabs stays inside them:

- **200 command creates per day per guild**
  ([docs](https://discord.com/developers/docs/interactions/application-commands#rate-limits)).
  This counts per-command `POST` creates. OpenCrabs uses the **bulk overwrite**
  route (`GuildId::set_commands` → `PUT /applications/<app>/guilds/<guild>/commands`),
  which replaces the whole set in one request and does not consume that budget.
- **5 requests per second per route.** OpenCrabs keeps a comparison key over the
  projected set plus the guild list, so an unchanged `commands.toml` re-read costs
  no API call at all and only a real change re-syncs. Guild membership is part of
  the key on purpose: a server the bot joined since the last sync moves it, so the
  new guild gets its menu without waiting for a config edit.
- Commands are synced on `ready` and on every config-publish. A reconnect
  re-plans, and the key comparison decides whether anything is sent, so a gateway
  that flaps on a short retry loop cannot turn into a registration storm. The
  trade-off is that a guild joined while the process is up is picked up on the
  next reconnect, not instantly.

## Adjacent surface: the `discord_send` tool

Everything above describes the slash-command projection. The other way an owner
drives Discord from OpenCrabs is the `discord_send` tool, which the agent calls
itself and a scheduled job can be pointed at. Its actions are a separate surface
with their own scope rules, so they are recorded here rather than in a second
file nobody would find from this one. Two groups are recent enough to spell out:
announcements (FR-012), and AutoMod rules with the guild audit log (FR-013).

### Announcements through a channel webhook (FR-012)

`announce` takes `message` and posts it to `channel_id`. With `channel_id`
omitted it goes to the ambient origin channel or the owner's last channel, the
same resolution every other action uses. What makes it an announcement rather than a
`send` is the two steps around the text:

- **The post leaves through a webhook, not as the bot's own message.** The
  webhook is named `OpenCrabs Announcements` and is reused when one already
  exists in that channel, so a repeated announcement does not litter the channel
  with a new webhook per call. Reuse is deliberately narrow: the webhook must be
  an **incoming** one, in **this** channel, carrying **our** name, and Discord
  must have handed us a **token** for it. That last condition is load-bearing
  rather than belt-and-braces: Discord only returns a token for a webhook the
  bot may execute, so a name match without one is unusable however well it fits.
  With no reusable webhook one is created, which needs **Manage Webhooks**.
- **It is then crossposted, which is what marks it published.** Only an
  announcement channel (`ChannelType::News`) supports that, so the check runs
  before the call and a refusal is reported as a **note** rather than an error:
  the post has already landed, and a failed crosspost must not turn a delivered
  announcement into a failed one. The text override carries the bot's own
  `username` and `avatar_url`, which is the half of AC-015 that is about the
  webhook rather than the channel.

The ceiling is **2000 characters, counted in characters rather than bytes**, and
a longer announcement is **refused with its measured length instead of
chunked**. Chunking would crosspost a fragment and silently drop the rest: only
one message can be crossposted, so there is no correct way to split one. The
post also passes through the Discord write budget like any other write, and when
the budget refuses it the result says nothing was posted rather than reporting a
failure that did not happen.

### AutoMod rules and the audit log (FR-013)

Five verbs join the tool: `automod_list`, `automod_create`, `automod_edit`,
`automod_delete` and `audit_log`.

**Reads are unguarded, mutations are not.** `automod_list` and `audit_log` only
read, so they run under the ordinary rules. The three mutating verbs go through
the scheduled-job scope guard on the same footing as the member actions (`kick`,
`ban`, `timeout`, `nickname`, `add_role`, `remove_role`): a job with no
`deliver_to` cannot change a guild's own settings any more than it can act on a
member. A rule is guild-level and has no member to name, so those three pass
`None` to the guard and its refusal names the guild's settings instead of
inventing a member to accuse.

| Verb | Parameters | Notes |
|---|---|---|
| `automod_list` | (none) | one line per rule: name, state, id, event, trigger, actions |
| `automod_create` | `keywords` (required), `name`, `block_message`, `alert_channel_id` | keyword trigger, blocks the message, enabled on creation |
| `automod_edit` | `rule_id` (required), plus at least one of `name`, `enabled`, `keywords` | an empty change set is refused rather than sent as a no-op |
| `automod_delete` | `rule_id` (required) | |
| `audit_log` | `limit` (default 10), `audit_action`, `user_id` | one line per entry, in the order Discord returns them |

None of the five takes a guild: they act on the guild the bot is connected to,
and say so plainly when no guild is known yet.

**The keyword grammar** decides whether a rule is created at all. Commas and
newlines both separate, so a pasted list works either way; blanks are dropped
and **duplicates collapse**, since Discord counts the entries and a repeated
phrase would spend the budget without widening the rule. A list past **1000
keywords** is refused with its count, a single keyword past **60 characters** is
refused with its length, and an empty list is refused outright rather than
creating a rule that matches nothing.

**Every change carries a reason.** `automod_create`, `automod_edit` and
`automod_delete` each send Discord an audit-log reason naming OpenCrabs, so the
guild's own log says where a change came from rather than only that it happened.
That reason is a constant with a test pinning its value, because a reword would
silently stop the guild's log from matching what the tool reports.

**`audit_action` accepts a name or a raw number.** The names are the ones this
feature is about (`automod_rule_create` 140, `automod_rule_update` 141,
`automod_rule_delete` 142, `automod_block_message` 143), plus the moderator
actions worth seeing beside them: `member_kick` (20), `member_ban` (22),
`member_update` (24), `member_role_update` (25), `message_delete` (72) and
`webhook_create` (50). Anything else is answered with that list rather than a
silent empty result, and a raw Discord action number passes straight through.

Rendering a rule or an entry is OpenCrabs' own work: serenity gives
`audit_log::Action` no `Display`, so the labels and the one-line shapes are ours
and are pinned by tests. A new trigger or action variant upstream renders as
"something else" instead of failing to compile here, because both enums are
`#[non_exhaustive]`.

Permissions: **Manage Guild** for the AutoMod verbs, **View Audit Log** for
`audit_log`, **Manage Webhooks** for an announcement that has to create its
webhook. A missing permission comes back as an error naming the permission
rather than as a silent no-op.
