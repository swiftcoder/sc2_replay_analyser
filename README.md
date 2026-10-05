# Replay analyser

This is intended to help LLMs analyse StarCraft 2 replay files. Since LLMs are pretty bad at interpreting the graphical data from the SC2 client itself, we use the bot API to dump relevant data into a set of json files for later analysis.

We play the StarCraft II replay back through the dockerised Linux SC2 client, and emit JSON diagnostics for **both players**: economy vs army, production scaling, compositions, upgrade research, engagements and base timing.

## Prerequisites

- Docker (the analyser launches a Linux SC2 client container).
- The map the replay was played on, available to the container. On macOS the
  ladder maps are pulled from `/Applications/StarCraft II/Maps`.

## Apple Silicon (M1/M2/M3/M4) Macs

The images are x86-64, so Docker runs them under emulation: slow QEMU by
default, or Apple's much faster Rosetta when it is enabled. Rosetta is
several times faster, but StarCraft II 4.10 crashes under it while loading
the map (both players `SC2Crash` at 0 game loops). The cause is SC2 itself:
it keeps some pthread mutexes at 2-byte-aligned addresses, and Rosetta's
translated ARM atomics SIGBUS on the `lock cmpxchg` when the operand crosses
a 16-byte boundary.

The small preload library in [`apple-silicon/`](apple-silicon/alignmutex.c)
(ported from
[aiarena/local-play-bootstrap#22](https://github.com/aiarena/local-play-bootstrap/pull/22))
routes each misaligned mutex to an aligned shadow mutex. Aligned mutexes
pass straight through, so it is a no-op elsewhere. On other platforms the
default run is unchanged.

To run replays with acceleration (the default on macOS/ARM):

1. Enable Rosetta for x86-64 containers:
   - **OrbStack:** Settings → Rosetta x86 emulation (or `orbctl config set rosetta true`).
   - **Docker Desktop:** Settings → General → "Use Rosetta for x86_64/amd64 emulation on Apple Silicon".
   - **colima:** `colima start --vm-type vz --vz-rosetta`.
2. Build the library once (needs only Docker): `./apple-silicon/build.sh`
3. Just run:
   `cargo run -- --replay <...> --maps-dir "<...>"`

   On macOS/ARM the Rosetta workarounds are on by default: the `--rosetta`
   argv padding is applied, and `apple-silicon/alignmutex.so` is preloaded
   automatically when present.

To run under QEMU instead (Rosetta disabled in the VM), opt back out with
`--no-rosetta` — the stock command line is correct there, and the padding
would break parsing instead. (An explicit `--alignmutex-so` is still
honoured under `--no-rosetta`, but it is unnecessary under QEMU.)

The argv padding duplicates the SC2 exe path as a sacrificial first argument.
It is needed on OrbStack, whose Rosetta drops argv[0] from
`/proc/self/cmdline` — and SC2 parses that file (not argv) as its command
line, skipping the first entry as the program name. Without it SC2 never
sees `-listen` and dies at startup (`Missing -listen command line`, then
signal 11). The colima setup from
[aiarena/local-play-bootstrap#22](https://github.com/aiarena/local-play-bootstrap/pull/22)
reaches map load without this workaround, so it appears OrbStack-specific;
Docker Desktop is untested — if startup fails there, try `--no-rosetta`
(and report back).

Upstream test results for the same library: Rosetta without it crashes at
map load; with it the test match completed (~2,100 game loops in ~25 s),
and games ran 3.7–9× faster than under QEMU.

## Running

```sh
cargo run -- \
  --replay /path/to/game.SC2Replay \
  --maps-dir "/Applications/StarCraft II/Maps" \
  --bot-name 12poolbot
```

`--bot-name 12poolbot` auto-selects the observed player (so "army", "our_score" and
the metrics' per-tick view are for 12poolbot). If the bot name isn't in the replay it
falls back to player 1. You can instead force it with `--observed-player-id 1|2`.

Useful flags:

| flag | default | meaning |
| --- | --- | --- |
| `--maps-dir` | `maps` | host dir mounted into the container's Maps folder |
| `--bot-name` | — | observed player by name (puck/titania/oberon/…) |
| `--observed-player-id` | auto | observed player id (1 or 2) |
| `--sample-loops` | `22` | game loops per observation (22 = 1 game second) |
| `--output-dir` | `output/analysis/<replay-stem>` | where the three files are written |
| `--no-docker` | off | connect to an already-running SC2 client instead of docker |
| `--alignmutex-so` | auto (`apple-silicon/alignmutex.so` when present) | explicit host path to the preload library (overrides the default) |
| `--rosetta` | on on macOS/ARM, off elsewhere | force-enable the Rosetta workarounds (argv padding + automatic alignmutex) |
| `--no-rosetta` | off | force-disable the Rosetta workarounds (use with QEMU) |
| `--cluster-radius` / `--confront-radius` / `--engagement-radius` / `--engagement-min-supply` | 12 / 20 / 14 / 6 | clustering + engagement tuning |

The analyser prints progress every 60 frames and, when done, the paths of the
output files.

## Output

Three files are written to the output directory:

```
output/analysis/<replay-stem>/
├── summary.json     # human-readable-ish: everything at a glance
├── metrics.jsonl    # one JSON object per game-loop sample (both players)
└── events.jsonl     # notable moments: composition/production/expansion changes + engagements
```

### summary.json

The place to start. Key sections:

- **`players` / `result`** — who played what race and who won.
- **`player_stats`** (per player): `final_expansions`, `peak_expansions`,
  `peak_workers`, `peak_army_supply`, `peak_army_value`, `total_killed_value`,
  `total_lost_value`, `worker_kills_value`, `worker_losses_value`.
  The classic tell: high `peak_workers`/`peak_expansions` with low
  `peak_army_supply` means the economy outran the army.
- **`engagements`** — each fight the clusterer found:
  - `kind`: `clash` (field fight) or `base_assault` (cluster threatening a
    townhall).
  - `sides`: per player the supply / value / unit breakdown.
  - `deaths`: per player `killed_value`, `lost_value`, `lost_units`.
  - `outcome`: `player_1` / `player_2`, or `mutual` when both sides lost more than
    they killed.
  - `base`: the townhall being assaulted, when applicable.
- **`transitions`** — when each player built production, teched, or changed
  composition (`kind`: `production`, `structure`, `tech`, `composition`,
  `upgrade`; `detail` like `WarpGate x3 (tier 2)`, `Forge x2 gained` or
  `Blink researched`).
- **`upgrades`** — every upgrade research, per player, with its completion
  `time_seconds` (e.g. `Blink` at 600s). Research is detected by watching the
  research ability appear on a structure's order and then vanish after the order
  is well underway, so it works without the API exposing upgrades directly.
  Note: SC2 only populates order queues for the *observed* player when watching
  a replay from a player's perspective, so non-observed (enemy) research is not
  recorded in a single run.

### metrics.jsonl

One object per sampled tick, keyed off `record_type` (`header`, `tick`, `footer`).
Each `tick` has `tick`, `time_seconds`, and per-player `players`:
`minerals`, `vespene`, `food_*`, `expansions`, `workers`, `income`
(`minerals_per_min`, `vespene_per_min`), `army` (`combat_supply`,
`combat_value`, `air_supply`, `stealth_supply`, `tier3_supply`), `production`
(`structures`, `producing`, `utilisation`, spend/produce rates), `killed`/`lost`
(value/count breakdowns), `supply_by_type` (live composition), and
`buildings_by_type` (per building type: total `count`, `completed`,
`under_construction`, `value`) — so you can see exactly when a double forge
started going up or how many gateways an opponent had at each moment.

There are also per-tick `clusters` and `confrontations` (spatial fighting data
used to build engagements).

A quick way to eyeball a game is to fold the ticks into a per-minute table:

```sh
python3 - <<'EOF'
import json
rows = {}
for line in open("output/analysis/<stem>/metrics.jsonl"):
    r = json.loads(line)
    if r["record_type"] != "tick":
        continue
    m = int(r["time_seconds"] // 60)
    p = r["players"]
    row = rows.setdefault(m, {})
    for pid in ("1", "2"):
        d = p[pid]
        comp = ", ".join(f"{u['count']}{u['unit_type']}" for u in
                         sorted(d["supply_by_type"], key=lambda x: -x["supply"])[:4] if u["count"])
        row[pid] = f"{d['income']['minerals_per_min']:.0f}i {d['workers']}w {d['expansions']}b " \
                   f"{d['army']['combat_supply']:.0f}s [{comp}]"
for m in sorted(rows):
    print(f"{m:>3}m | {rows[m].get('1','')} | {rows[m].get('2','')}")
EOF
```

### events.jsonl

Timeline of notable moments: `composition_transition`, `production_transition`,
`structure_transition` (non-production buildings completed or lost, e.g. a
second Forge or a destroyed Robotics Bay), `upgrade_started`/`upgrade_researched`,
`expansion_gained`/`expansion_lost`, and
`engagement_start`/`engagement_end` (which carry the same side/death/outcome data
as the summary's engagements).

## Worked example

Analysing a ladder loss:

```sh
cargo run -- \
  --replay ~/Downloads/4901254_GenesisLotus_puck_UltraloveAIE_v2.SC2Replay \
  --maps-dir "/Applications/StarCraft II/Maps" --bot-name puck
```

Then: read `summary.json` (`player_stats` for the economy-vs-army balance, the
`engagements` for the fights), and use the `metrics.jsonl` snippet above to see
*when* the balance shifted. Expected runtime for a ~10 minute replay is on the
order of a couple of minutes.
