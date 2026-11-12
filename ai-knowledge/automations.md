# Automations, reminders, scripts

## Rules (when → if → then)
`create_rule` {name, database, trigger, conditions?, actions}
* **trigger**: `"row_added"` · `"manual"` · `{"type":"prop_changed","prop":"Status","to":"Done"}` (omit `to` = any change) ·
  `{"type":"date_reached","prop":"Due","offset_days":-1,"time":"09:00"}` · `{"type":"schedule","every":"daily|weekly|monthly|yearly","interval":1,"time":"09:00","weekdays":["mon","fri"]}`
* **conditions**: `[{prop,op,value}]` same filter ops as database views.
* **actions** (run in order): `{"type":"set","prop":"Status","value":"Done"}` (value may be text/number/bool, or `"today"`, `"+3d"`),
  `{"type":"create_row","database":"Tasks","title":"Follow up: {title}"}`, `{"type":"move_row","database":"Archive"}`,
  `{"type":"rename","title":"…"}`, `{"type":"append","text":"…","todo":true}`, `{"type":"notify","message":"…"}`,
  `{"type":"duplicate"}`, `{"type":"trash"}`, `{"type":"script","code":"…"}`.
* `list_automations`, `set_rule_enabled` {rule, on}, `delete_rule` {rule}.
* Scheduled / date triggers run while the app is open; reminders also fire as OS notifications.

## Reminders
`create_reminder` {message, date:"2025-06-01", time:"09:00", page?, lead_minutes?}

## Scripts (LumenScript)
Use scripts when point-and-click actions are not enough (loops, several databases, calculations). See `scripting`.
`save_script` {name, code} stores it in the Automations → Scripts library.
