# Lumen — where to take it, and how it makes money

Prices, percentages and market statements below are **planning assumptions**, not researched market data — validate each with customer interviews and small tests before committing.

## 1. What Lumen already is (the unfair advantages)
* **One Rust codebase on Windows, Android and the browser** (WebAssembly) — small binary (15–35 MB), no Electron, no web view: fast start, low RAM, battery-friendly.
* **Local-first and private by construction** — data lives on the device; password lock, atomic saves, rolling backups. Strong story for privacy-sensitive users and regions with poor connectivity.
* **Notion + Obsidian + Miro-lite + Airtable-lite in one app**: pages, databases (6 views, relations, formulas, rollups), dashboards, infinite canvas, flows, automations, reminders, scripting (LumenScript), PDF reader with annotations, AI agent + MCP server.
* **Real multi-script support** (HarfBuzz shaping, RTL, Bangla/Hindi/Tamil/Arabic…, now colour emoji) — most competitors are mediocre here. 2 billion+ people write in these scripts; it is a wedge market, not a footnote.
* Gap to fix before charging: **no cloud sync / collaboration, no iOS/macOS/Linux packaged builds, no templates marketplace, no public sharing** — they are the top reasons teams choose Notion.

## 2. Product directions (ranked by impact ÷ effort)
| Rank | Feature | Why it matters | Effort | Money angle |
|---|---|---|---|---|
| 1 | **Cloud sync + sharing + comments + permissions** (E2E-encrypted, offline-first; design in `DATA-ARCHITECTURE.md` §5 Phase 3) | Unlocks teams, multi-device, backups | L | Core subscription |
| 2 | **Huge-data engine** (row store, windowed tables, exact aggregates on 10⁷ rows) | "Airtable/Excel that never lags": import 1 M-row CSV in seconds | M–L | Pro tier gate (row limits) |
| 3 | **Charts 2.0**: combo + dual axis, treemap, sankey, waterfall, box plot, candlestick, bubble, map, histogram bins, trend/forecast lines, reference lines, annotations, drill-down, cross-filtering, PNG/SVG/PDF export, scheduled snapshot emails | Dashboards are a sellable product on their own (BI-lite) | M | Pro/Team |
| 4 | **Forms + public pages + publish-to-web** (static export, custom domain) | Lead capture, surveys, docs sites, portfolios | M | Add-on / Team |
| 5 | **Import/export**: Notion, Obsidian, Evernote, Excel/CSV/Parquet/SQLite, Google Docs; export Markdown/PDF/HTML/CSV | Lowers switching cost — the main growth lever | M | Free (acquisition) |
| 6 | **AI that works on your data**: local semantic search (embeddings stored in the row store), summarise/extract into properties, "chat with this database", meeting-notes → tasks; BYO key free, managed credits paid | Highest willingness-to-pay today | M | Credits / Pro |
| 7 | **Plugin & template marketplace** (LumenScript, widgets, chart packs, database blueprints; 70/30 revenue share) | Network effect, long-tail verticals | M–L | Take-rate |
| 8 | **Integrations**: Google/Outlook calendar & mail, GitHub/Jira, Slack/Teams, Stripe, webhooks, REST + MCP endpoints, Zapier/Make app | Embeds Lumen in workflows | M | Team/Business |
| 9 | **Mobile-first power**: home-screen widgets, share-sheet capture, voice notes + on-device transcription (whisper.cpp), OCR scan-to-page, quick-capture tile, offline PDF annotation sync | Phones are where notes are born | M | Pro |
| 10 | **iOS / macOS / Linux builds** (same Rust: egui + wgpu) + store listings | Doubles addressable market | M (iOS L) | Reach |
| 11 | **Vertical packs**: CRM, clinic/patients, legal matters, school (gradebook, timetable), church/NGO, small-shop inventory & invoicing, freelancer time-tracking, content calendar | Customers pay for outcomes, not blocks | S each | One-time packs or Business tier |
| 12 | **Enterprise/self-host**: SSO (SAML/OIDC), SCIM, audit log, data residency, on-prem server (single Rust binary + S3), admin console | High ACV, low volume | L | Annual contracts |
| 13 | Web clipper, email-in, calendar & timeline sync (ICS), time tracking, invoicing from database rows, e-signature, spaced-repetition flashcards, whiteboard templates, presentation mode | Stickiness | S–M | Pro |

## 3. Business model
**Freemium + open-core**
* **Free**: full local app on all platforms, unlimited pages, 10 000 rows/database (the huge-data engine is the upgrade), BYO AI key, 3 published pages, manual backups.
* **Pro (individual) — suggested US$6–9/month or ~US$70/year**: sync across devices (5 GB), unlimited rows, Charts 2.0 export, scheduled reports, managed AI credits, version history 1 year, custom domains for 1 site.
* **Team — suggested US$10–14/seat/month**: shared workspaces, permissions, comments, guest access, audit log, admin, integrations, priority support.
* **Business / Enterprise — custom**: SSO, SCIM, data residency or on-prem, SLA, security review.
* **Marketplace**: 30 % take on paid templates/plugins; featured placement as an upsell to creators.
* **Managed AI**: resell model usage with a margin (e.g. 30–50 %) over provider cost; BYO key stays free to keep trust.
* **Regional pricing** (purchasing-power adjusted) and **education discounts** — large user base in South Asia, Africa, LatAm where Notion-class prices are a barrier; one-time "lifetime Android" purchase is a proven option there.

Unit economics sketch: encrypted chunk sync costs cents per user per month (object storage ≈ US$0.015/GB-month; 1 M rows ≈ 20–50 MB). Real costs are support, AI inference and the sync server fleet — so price AI by credits, keep sync generous. Target gross margin > 80 % excluding AI.

## 4. Go-to-market
1. **Wedge**: privacy-first, fast, offline, runs on cheap Android phones and old laptops; **Bangla/Hindi/Arabic-first quality**; "import your Notion in one click".
2. **Channels**: Play Store + Microsoft Store + direct download (already built), Product Hunt/Hacker News/Reddit (r/selfhosted, r/rust, r/ObsidianMD, r/Notion), YouTube/TikTok template creators (give them the marketplace), university ambassadors, local-language tutorials, accountant/clinic/shop owner communities with vertical packs.
3. **Virality**: public pages and forms carry "Made with Lumen"; share links open the WebAssembly viewer instantly (no install).
4. **Trust**: open-source the store and file format; publish the encryption design; reproducible builds (the pipeline script already exists).

## 5. Metrics to steer by
Activation (first database or canvas created in 24 h), D7/D30 retention, WAU, notes created per WAU, import success rate, 5 M-row workspace start time & crash-free sessions (engineering KPI), free→Pro conversion (target 3–5 %), team seats per paying workspace, marketplace GMV, support tickets per 1 000 users, sync conflicts per 10 000 ops.

## 6. 12-month plan
* **Q1** — Phase 0/1 of the data architecture; Charts 2.0 (first 6 chart types + export); import (Notion/CSV/Obsidian); Play Store/Microsoft Store listings; telemetry opt-in (privacy-preserving).
* **Q2** — Sync v1 (single user, E2E, 2+ devices), version history in the cloud, Pro subscription launch, public pages.
* **Q3** — Shared workspaces, comments, permissions; forms; integrations (calendar, webhooks, REST); AI over your data; template marketplace beta.
* **Q4** — Team plan, SSO/audit (Business), self-host server, iOS/macOS/Linux, vertical packs, partner programme.

## 7. Risks and answers
* *Sync correctness* is the hardest engineering item — test with randomized multi-device simulations before shipping; keep local-only mode first-class.
* *Competition* (Notion, Obsidian, Anytype, AppFlowy, Coda, Airtable) — do not out-feature; win on speed, privacy, languages, data scale and price.
* *Single-codebase UI risks* (accessibility, native feel) — invest in screen-reader support (AccessKit), IME, and large-text modes early; they decide enterprise/education deals.
* *Support load* — in-app diagnostics export, self-serve backup restore, status page.
* *Platform rules* — iOS needs a Mac build pipeline and App Store review; budget for it before promising it.
