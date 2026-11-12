# LumenScript — scripting for Lumen automations (v7.0)

LumenScript is a small, **sandboxed** scripting language written in pure Rust (`src/script.rs`, no dependencies). It runs the same in the
Windows build, in WebAssembly and in a future headless server. `src/scripthost.rs` is the only bridge to the workspace.

## Where scripts live
* **Automations → Scripts** — a library with editor (syntax highlighting, line numbers, live error marker), console, examples,
  *▶ Test* (runs on a copy of the workspace and lists what it **would** change), *Run* (once, or for every row of a database) and *Make a rule*.
* **Run script** — an action of any rule or button block, next to the point-and-click actions (`Action::Script { code }`).

## Language in 60 seconds
```js
// comments: // … and /* … */
let total = 0                       // statements end at a line break (or ;)
const limit = 10
for t in db("Tasks").rows() {       // for x in list | for i, x in list | for k, v in map | for i in 0..10
    if t.Status == "Done" && t["Due date"] < today() { total += 1 }
}
fn label(n) { return n > limit ? "many" : "few" }
let double = |x| x * 2              // lambdas / closures
notify("{total} finished tasks ({label(total)})")   // "…{expression}…" interpolates; '…' is literal
try { error("boom") } catch e { print("caught", e) }
```
Values: `null`, booleans, numbers, strings, lists, maps, functions, and host objects (rows, databases).
Operators: `+ - * / % **`, `== != < <= > >=`, `&& || !` (also `and or not`), `??`, `a ? b : c`, `?.`, ranges `a..b`.

## Workspace API (see the in-app reference for the full list)
`row`, `page`, `trigger`, `db("Name")`, `dbs()`, `database.rows() / .add({…}) / .find(prop, value) / .where(…) / .options(prop)`,
`row.Prop`, `row["Prop name"]`, `row.Prop = value`, `row.update({…})`, `row.append(text)`, `row.append_todo(text)`, `row.duplicate()`,
`row.move_to("Db")`, `row.trash()`, `create_page(title, body)`, `search(text)`, `notify(msg)`,
dates: `today() now() date_add(d, n) date_diff(a, b) days_until(d) weekday(d) date_format(d) is_date(s)`.

## Safety
| Limit | Value |
|---|---|
| Instruction budget per run | 400 000 steps (endless loops stop with an error) |
| Call depth / parser nesting | 40 / 80 |
| Strings / lists | 1 MB / 200 000 items |
| Rows created / changes / notifications per run | 200 / 3 000 / 25 |
| Output lines | 400 |
| Reachable from a script | only the host functions above — no files, network, clipboard, OS, `import` |

Failed `add()` calls never leave a half-built row; a failing script reports `line:column — message` and the Activity log marks it.

## For AI assistants
*Automations → Scripts → Reference → Copy API for an AI assistant* copies `scripthost::api_markdown()`: a prompt plus the complete API.
`script::check(src)` validates generated code without running it and returns a position + message, so an agent loop can
“generate → check → test (dry run) → ask the user to approve → run” without ever touching real data before approval.

## Not in v7.0 (also listed in Automations → Roadmap)
AI assistant itself (needs a provider key), webhook/HTTP triggers and server-side scheduling (need a server), autocomplete/debugger,
`import`/shared libraries, assigning relations from scripts.
