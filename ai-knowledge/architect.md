# Architecting whole systems

When the user asks for a "system", "workspace", "CRM", "tracker", "OS for X": build it as **hub + data + views + automation**.

## Blueprint
1. **Hub page** (doc, favourite): purpose, how to use, `[toc]`, links to everything (`[[Page]]`).
2. **Data layer**: 2‑5 databases with relations (e.g. Projects ⇄ Tasks ⇄ People). Seed 3‑8 realistic rows so the UI is not empty.
3. **Views**: board by status, calendar/timeline by date, table by owner/priority.
4. **Dashboards**: one overview dashboard page (KPIs, charts, upcoming, board) + one per area if the system is big.
5. **Automation**: status → date stamping, due-date reminders, recurring tasks, weekly digest script/flow.
6. **Docs**: a short "Playbook" page: conventions, definitions, process.
7. **Verify** and summarise: list pages created as a tree.

## Recipes (databases → properties)
* **Project management**: Projects(Status, Owner, Start/End, Priority) · Tasks(Status, Project→relation, Assignee, Due, Estimate h, Priority) · Milestones. Dashboard: open tasks by status (bar), due this week (table), workload by assignee (hbar), progress gauge.
* **CRM**: Companies · Contacts(Company→) · Deals(Stage select: Lead/Qualified/Proposal/Won/Lost, Value number, Close date). Dashboard: pipeline funnel, revenue by month (area), won vs lost (donut), follow-ups. Rule: Stage → Won ⇒ set Close date today + notify.
* **Content calendar**: Content(Channel multi, Status: Idea/Draft/Review/Scheduled/Published, Publish date, Owner). Calendar view, board by status, heat-map.
* **OKRs**: Objectives · Key results(Objective→, Current, Target). Dashboard: gauges, table of KRs.
* **Personal finance**: Transactions(Amount, Category select, Date, Account) · Budgets. Dashboard: spend by category (donut), monthly trend (line), budget progress bars.
* **Knowledge base / second brain**: Notes database(Type, Tags, Source URL) + Hub + Canvas map + Reading list.
* **Habit / health tracker**: Habits(Done checkbox, Date) + heat-map + countdown widgets.
* **Software architecture docs**: ADR database (Status, Date, Decision), system canvas diagram, runbook pages, flow for incident notification.

## Quality bar
* Names are specific ("Sales Pipeline", not "Database 1"). Every page has an icon.
* Never leave empty databases without at least one example row.
* Do not create more than ~12 pages without telling the user the plan first.
