# UX Task-Series Parity Report

Generated 2026-08-17T09:47:46.031Z by e2e/parity-tasks.mjs.
Both servers serve the SAME live DB — every divergence is a server gap.

| Step | Python (oracle) | Rust | Verdict |
|---|---|---|---|
| A.session-list | `{"archivedCount":0,"previewLinesShapes":["array-of-strings"],"probes":{"amux-helper":{"archived":false,"flags":false,"hasPreview":true,"previewLinesShape":"arra` | `{"archivedCount":0,"previewLinesShapes":["array-of-strings"],"probes":{"amux-helper":{"archived":false,"flags":false,"hasPreview":true,"previewLinesShape":"arra` | DIVERGES — facts differ |
| B.board-data | `{"byStatus":{"review":1,"todo":6},"fields":["archived","created","creator","depends_on","desc","due","due_time","gate","id","last_verified_at","log","owner_type` | `{"byStatus":{"review":1},"fields":["archived","created","creator","depends_on","desc_head","desc_len","due","epic","folded_n","id","log_n","needsyou_note","owne` | DIVERGES — facts differ |
| C.groups | `{}` | `{}` | PARITY |
| D.board-write-flow | `{"create":201,"createdId":"yes","edit":200,"move":409}` | `{"create":201,"createdId":"yes","edit":200,"move":409}` | PARITY |
| E.schedules | `{"count":1,"enabled":1,"fields":["command","created","deleted","done_action","done_pattern","enabled","exit_actions","fires_day","fleet_share","gcal_event_id","` | `{"count":1,"enabled":1,"fields":["command","computed_next_run","created","deleted","done_action","done_pattern","enabled","exit_actions","fires_day","fleet_shar` | PARITY — rust-only extras (parity): computed_next_run,version |
| F.calendar | `{"count":0,"status":200}` | `{"count":0,"status":200}` | PARITY |
| G.settings-backends | `{"prefsCount":10,"usage":200,"usageHasWindows":false}` | `{"prefsCount":10,"usage":200,"usageHasWindows":false}` | PARITY |
| H.session-verb-peek | `{"hasOutput":false,"status":404}` | `{"hasOutput":false,"status":404}` | PARITY |
| H2.tab-endpoints | `{"map":200,"mapHasSettings":true,"mapPins":0,"skillFields":["description","hint","name"],"skills":200,"skillsCount":4,"slash":200,"slashCount":69,"statusesIds":` | `{"map":200,"mapHasSettings":true,"mapPins":0,"skillFields":["description","hint","name"],"skills":200,"skillsCount":4,"slash":200,"slashCount":69,"statusesIds":` | PARITY |
| J.crm-tab | `{"visibleCrmTab":false}` | `{"visibleCrmTab":false}` | PARITY — python shows it: false (intended difference) |
| I.worker-tab-numbers | `{"boardChips":["Working now1","Needs you1","Rotting1","Unowned7","Mine2","Archived6","⚡ Focus1"],"sessionTabs":["🔔0","22","0"]}` | `{"boardChips":["Needs you1","Rotting1","Unowned1","Mine1","⚡ Focus1"],"sessionTabs":["🔔0","22","0"]}` | DIVERGES |

3 step(s) diverge — see rows above.
