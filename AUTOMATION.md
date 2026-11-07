# Automation in Lumen — analysis, design and roadmap

*What can be automated in a Notion-style workspace, what ships in this release (v6.0), and how the engine is built so the cloud / multi-user
features that come next can plug into it without a rewrite.*

## 1. What users automate (and where Lumen stands)

| Need | Typical Notion workaround | Lumen v6.0 |
|---|---|---|
| "When status changes → stamp a date" | Database automation (paid plans) | ✅ **Rules** — trigger *Property changes*, action *Set property* (`Today`, `Today + N`, shift N days, clear) |
| "Create a follow-up task when done" | Template buttons / automations | ✅ action *Create row* with placeholders (`{title}`, `{date}`, `{today}`, `{prop:Name}`) |
| "Move the card to another database" | Manual / automation | ✅ action *Move row* (properties matched by name + type, missing select options are created) |
| "One-click buttons in pages" | Button block | ✅ **Button block** (`/button`) — ordered action list, runs on the page or the database row that holds it, edit via ⚙ / right-click / long-press |
| "Repeat this task every Monday" | Recurring date property | ✅ **Repeat rules on date properties** — daily / weekly (weekday mask, every N) / monthly / yearly, end *never / on date / after N*, three modes: roll on completion, roll when the date passes (habits), keep the finished row and spawn the next copy; *count from completion* |
| "Remind me" | Reminder on a date | ✅ **Reminders** — bound to a row's date property (moves with the date) or standalone, time of day, lead time (at time … 2 days before), repeat, custom message |
| Desktop / push delivery | Windows app + mobile push | ✅ Windows toast (no dependency: PowerShell → `Windows.UI.Notifications`), browser push through the service worker, quiet hours, in-app inbox with unread badge |
| "Nag me before it is due / when overdue" | Formula + filters + manual check | ✅ trigger *A date arrives* (`−1 day`, `on the day`, `+1 day` at HH:MM), skips finished rows |
| "Every weekday at 9 create the stand-up page" | Recurring template | ✅ trigger *On a schedule* (same repeat engine, runs without a row) |
| Bulk "do this to all matching rows" | Select all → edit | ✅ trigger *Manual* + ▶ **Run now** on any rule (applies to every row matching its conditions) |
| Audit: "what did the automation do?" | — | ✅ **Activity log** (rule, row, what changed) |
| Starter content | Templates | ✅ **Recipes** — 11 one-click rules that adapt to a database's own Status / Date properties |

## 2. Architecture

```
 UI edits ──► Workspace (pages, props, blocks, auto: AutoState)
                │
   automation::tick(ws, rt, now_ms, now_local_min)       ← pure, no egui, no OS, no clock
                │
     detect()  snapshot diff → Ev::Added / Ev::Changed   ← the single seam for ALL change sources
                │
     process() queue (depth ≤ 3, budget 400)  ──► repeat handling  ──► rules (trigger + conditions) ──► run_actions()
                │                                                                                    │
     run_schedule()  date triggers · schedules · auto-rolling repeats · reminders  ◄────────────────────┘
                │
        Output { notices, changed }  ──► app: toasts, OS/web notifications, formula recompute
```

Key decisions

1. **Snapshot diff instead of hooks.** Rows are edited from ~15 places (table cells, board drags, timeline bars, bulk actions, imports).
   The engine diffs a snapshot 4× per second. Anything that changes a row — including a future sync layer applying a remote edit —
   produces the same `Ev`, with `old` and `new` values. Cost: one `BTreeMap` equality per row per tick.
2. **Pure engine, injected time.** `tick` receives `now_ms` and the local minute. Tests construct whole scenarios (31 s of virtual time) in
   microseconds; a server can run the identical code headless against a synced workspace.
3. **Local wall-clock minute as the time axis.** Reminders and date triggers are `day * 1440 + minute` in local time. The Windows build reads the
   time zone from `GetTimeZoneInformation`, the web build from `Date.getTimezoneOffset()` (`platform::tz_offset_minutes`, `dates::now_local_min`).
4. **Open vocabulary, forward compatible.** `Trigger`, `Action`, `Setter` are enums; every struct field is `#[serde(default)]`. Old workspace
   files load unchanged; adding a variant never breaks existing data.
5. **Safety rails** (all unit-tested):
   * cascades stop at depth 3 and 400 events; per-(rule,row) limit of 12 runs/minute — an A→B→A loop cannot run away;
   * the engine's own writes are re-snapshotted so they do not re-trigger the rule that made them;
   * text / number edits are *debounced* (1.2 s of rest) — no rule fires per keystroke;
   * scheduled occurrences older than 3 days are skipped silently — enabling a rule or opening the app after a holiday never replays the past;
   * date triggers remember `row → occurrence` so each fires exactly once, and re-fire if the date is moved;
   * trashed rows never trigger anything; deleted databases make actions skip (and log) instead of panicking.
6. **Delivery abstraction** (`platform::notify_os`, `schedule_push`): one call per channel. The browser receives the next 48 h of reminders as JSON and
   arms its own `setTimeout`s, so reminders fire in a background tab where the canvas is not repainting; a `tag` de-duplicates the Rust-side and
   the JS-side notification. The service worker already handles `push` events and `notificationclick`.

## 3. Data model additions

| Where | Field | Purpose |
|---|---|---|
| `Workspace.auto: AutoState` | `rules`, `reminders`, `inbox`, `log`, `settings` | everything automation-related, in the normal workspace JSON (backup / restore / future sync just work) |
| `Page.repeats: BTreeMap<prop id, Repeat>` | per-row recurrence of each date property | moves with the row on duplicate / export |
| `Block.acts: Vec<Action>` + `BlockKind::Button` | button actions | duplicate / history / copy-paste carry them automatically |
| `Reminder { page, prop, day, minute, lead_min, repeat, fired_for }` | `prop != 0` → follows the row's date | standalone when `prop == 0` |

## 4. Roadmap — designed-for, not yet built

*Cloud (next):* the engine is already a pure function of `(workspace, time)`.
1. **Server-side scheduler** — run `tick` in a worker (Rust, same crate compiled natively) for every synced workspace, so schedules and reminders run while
   all clients are closed; deliver through the existing service-worker `push` handler (`/api/push/subscribe` and the `push_subscriptions` table are in place).
2. **Per-device delivery preferences** — `Settings` becomes per-device, `AutoState` stays per-workspace (needs only a `device_id` on `Notice`).
3. **Conflict-free automation under sync** — add an `origin` to `Ev` (local / remote / engine) so a rule runs on exactly one device; the rate limiter key
   already supports it.
4. **Webhooks & API triggers** — new `Trigger::Webhook { token }` and `Action::Http { url, template }` (variants only; no schema migration).
5. **Integrations** — Slack / e-mail / calendar (ICS feed generated from reminders and date properties) as additional `Action`s and delivery channels.

*Editor & data:*
6. **Button property** in databases (the action list is the same `Vec<Action>`), **per-view buttons** on boards.
7. **Formula-driven triggers** (`Trigger::When(formula)`), rollup thresholds ("pipeline > 100 k").
8. **Relation-aware actions** — create a row *and* link it, update the related rows, roll status up to the parent.
9. **Wait / branch** — `Action::Wait(days)` (needs a persistent job queue, trivial once the server scheduler exists), `Action::If(cond, then, else)`.
10. **Templates as automations** — a database template carries its rules and repeat settings (instantiate = copy `Rule`s with remapped property ids).
11. **Undo for automation runs** — the log already records what changed; add before-values and a per-run *Undo*.
12. **Local AI actions** — summarise a page into a property, auto-tag, extract dates from text into a reminder (bring-your-own-key / local model).
13. **Voice & share-target capture on mobile** — `Action::Append` fed by the PWA `share_target`.
14. **Accessibility** — announce notifications through the screen-reader bridge eframe already has.

## 5. Test coverage (`cargo test`, module `automation::tests`)

weekly masks / intervals · month-end and leap-day clamping · overdue skipping, range preservation, end rules · quiet hours across midnight ·
status → set-date rule without re-trigger · cascade loop bound · recurring task roll + status reset · spawn mode · date trigger fires once and never
replays old dates · reminders (repeat / one-shot / push JSON) · schedule rules + button press · move-row property mapping · typing debounce · all recipes
build · `AutoState` JSON round-trip.
