# Automation Recipes

A **recipe** is an ordered list of steps that Archon runs as a single automation
flow. Each step is either an **explicit deterministic browser action** or a
**natural-language goal** handed to the built-in agent. Recipes run through the
same `AutomationOrchestrator` guardrails as `--agent` (domain allow/block lists,
rate limiting, sensitive/password guards, and risk-gated confirmation), and they
produce the same transcript output (JSON + Markdown).

## Usage

```bash
# Dry-run preview (no mutations) of a recipe by path
archon --automate ./automation/recipes/example.json

# Resolve a bare name to automation/recipes/<name>.json
archon --automate example

# Execute the recipe (requires automation.enabled = true in config)
archon --automate example --agent-execute

# Skip per-action confirmation prompts for High/Critical actions
archon --automate example --agent-execute -yes

# Attach to a running hardened session instead of launching a new browser
archon --automate example --agent-execute --agent-attach

# Run headful and export the transcript to a chosen directory
archon --automate example --agent-execute --agent-headful --agent-export ./runs
```

Recipes reuse the agent flags: `--agent-execute`, `-yes`, `--agent-headful`,
`--agent-attach`, `--agent-provider`, `--agent-max-steps`, and `--agent-export`.

## Recipe format

```json
{
  "name": "Example hybrid recipe",
  "description": "Explicit actions plus a natural-language goal step",
  "start_url": "https://example.com",
  "steps": [
    { "action": "navigate", "url": "https://example.com" },
    { "action": "extract", "selector": "h1" },
    { "goal": "Find the 'More information' link and report where it points", "max_steps": 4 }
  ]
}
```

| Field | Type | Description |
| --- | --- | --- |
| `name` | string (required) | Human-readable recipe name; used as the run goal. |
| `description` | string | Optional longer description, appended to the goal label. |
| `start_url` | string | Optional URL navigated to before the first step. |
| `steps` | array (required, non-empty) | Ordered list of action or goal steps. |

### Action steps

An action step is an object with an `action` field:

| `action` | Required fields | Optional fields |
| --- | --- | --- |
| `navigate` | `url` | — |
| `click` | `selector` | — |
| `type` | `selector`, `text` | — |
| `extract` | `selector` | — |
| `scroll` | — | `selector` |
| `wait` | — | `ms` |
| `screenshot` | — | — |

### Goal steps

A goal step is an object with a `goal` field handed to the agent:

| Field | Type | Description |
| --- | --- | --- |
| `goal` | string (required) | Natural-language objective for the agent. |
| `start_url` | string | Optional URL the agent navigates to first. |
| `max_steps` | integer | Optional per-goal step cap (defaults to `--agent-max-steps`). |

Action and goal steps are distinguished by their required field (`action` vs
`goal`), so the two forms can be freely mixed in one `steps` array.

## Safety

- **Preview by default.** Without `--agent-execute`, Archon records what each step
  *would* do without performing any mutation.
- **Automation gate.** `--agent-execute` fails unless `automation.enabled = true`.
- **Risk confirmation.** High/Critical actions (click, type, navigate, submit)
  prompt for confirmation unless `-yes` is passed. A declined action is recorded
  as a preview and the run stops.
- **Stop on failure.** The first failed executed action ends the run.

## Transcript export

Every recipe run is persisted to `transcripts/agents/` as both `agent-{id}.json`
(machine-readable) and `agent-{id}.md` (human-readable). Pass `--agent-export
<DIR>` to additionally write both files to a directory of your choice. The same
export applies to `--agent` runs.
