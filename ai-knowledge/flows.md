# Flows (node-based pipelines)

A `flow` page is a graph: **nodes** with named input/output **ports**, joined by **wires**. Runs are a state machine polled once per frame;
a node runs when its inputs carry data, otherwise it is skipped. Inactive outputs (e.g. the unused branch of `if`) skip everything downstream.

## Tools
* `list_flow_nodes` — every node type with ports, parameters and docs (**call this before building**, the catalogue grows over time).
* `create_flow` {title, parent?, icon?, nodes:[{id,kind,label?,params?}], wires:[{from,to,from_port?,to_port?}], vars?, every_seconds?}
  `id` is a local name; ports are given by **name** (`"from_port":"true"`) or index; omitted ports = first output → first input.
  Positions are automatic (left→right layout).
* `flow_problems` {page} · `run_flow` {page, dry?: true} (network nodes need the Flow page open)
* `read_page` shows nodes, wires and the problems list.

## Patterns
* Trigger → transform → act: `manual`/`now`/`cloud_in` → `get`/`map`/`filter` → `db_add`/`db_update`/`notify`.
* Branching: `if` has outputs `true`/`false`; `switch` has one per case; `merge` rejoins.
* Code: the `code` node runs LumenScript (see `scripting`) with inputs as globals and `emit(value)` to output.
* Network: `http` (method, url, headers, body), `api` (ready-made), `rss`. On the web build requests go through the site proxy.
* Always end a flow with an `output` or `notify` node so the result is visible; add a `note` node explaining the flow.
* After `create_flow` call `flow_problems`; fix every problem before telling the user it is done.
