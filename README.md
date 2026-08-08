# fml — a corporate finance modelling language

Phase 1 implementation: lexer → parser → unit/type checker → reference
evaluator, with the golden-model suite as CI. Design documents live in
[`../finmodel-lang-research/`](../finmodel-lang-research/) — start with
`00-SYNTHESIS.md` (literature synthesis), `06-architecture.md` (stack) and
`07-syntax-and-ir.md` (surface syntax + core IR).

```bash
cargo test                          # golden suite + negative tests
cargo run -- check models/finplan.fml
cargo run -- eval  models/finplan.fml
```

## What works (Phase-1 subset)

- **Declarations**: `model`, yearly `calendar`, `currency`, `unit`, typed
  `input`s (scalar, broadcast, per-period map literals), measures with
  optional unit/kind annotations, `over`, `init`.
- **Units** (Kennedy-style free abelian group): currencies and declared units
  as base dimensions, `USD/share` compound annotations, percent literals,
  `rate`/`ratio`/`1` as dimensionless spellings, bare numeric literals
  unit-polymorphic in additive/init positions. `USD + share` is a check-time
  error.
- **Series semantics**: sequential evaluation over the calendar; `prev()`
  with mandatory `init` when it reaches the calendar start (checked);
  series-ness and units inferred for unannotated measures.
- **`solve` blocks** (block form): per-period Gauss–Seidel fixpoint in
  declaration order, seeded from the previous period, with `tolerance` /
  `max_iterations`, divergence and non-convergence errors. Same-period
  cycles anywhere else are compile errors naming the cycle.
- **`assert`** with `± tol`, evaluated every period, reported with max
  deviation.
- **CLI**: `fml check` (symbol table with inferred units) and `fml eval`
  (value table, solve convergence stats, assert results; non-zero exit on
  assert failure — CI-ready).

Verified against the independent Python reference implementation of
Warren & Shelton (1971) FINPLAN: all measures match within fixpoint
tolerance, leverage lands on the 45% target each year, balance sheet ties
(`tests/golden_finplan.rs`).

Also verified: the FAST-style project-finance sculpting model
(`models/solar_pf.fml`, golden #2) — quarterly calendar, period sub-ranges
(`period tenor = 2027-Q1 .. 2036-Q4`), `match t { in constr -> … }`
dispatch, forward windows (`sum(debt_service[t+1 .. t+2])`), `npv`/`irr`/
`annualize`, `when first(range)`, and the **tearing-form solve**
(`solve sizing { relax uses …  relax debt_facility … }`). The scheduler is
topological over (measure × period) pairs — forward references make time
bidirectional — with Tarjan SCC condensation; nontrivial SCCs are executed
as per-period Gauss–Seidel (block solves) or tearing iteration (relax
solves). The sizing fixpoint converges in 5 iterations; DSCR is exactly
1.30 across the tenor and the balance amortizes to zero by the PV identity
(`tests/golden_solar.rs`).

And golden #3, the two-entity FX consolidation (`models/fx_consol.fml`):
monthly calendar, an **Entity dimension** (`dimension Entity = tree { Group
-> { PT_Co, US_Co } }`), **member-dependent `local` currencies** resolved
through a `functional` map with per-member unit checking, explicit currency
conversion (`net_income in kEUR at usd_eur_avg` — multiply-vs-divide
derived from the units), `match Entity { … }` dispatch, member and group
indexing (`assets_tr[Group]` = leaf sum, rejected for `local` units with a
"translate first" error), and per-member assert iteration. All four
invariant families (entity books, elimination identity, consolidated
balance, CTA double-derivation) hold at 0.000000 (`tests/golden_fx.rs`).
The scheduler generalizes to the (measure × member × period) micro-graph.

## Phase-1 closers

- **Scaled units**: `unit kEUR = 1000 EUR` — same dimension, different
  scale; mixing without conversion is ill-typed; `x in kEUR` converts
  without a rate (`tests/units_scaled.rs`).
- **`eliminate` pairs**: `eliminate loan over m : a against b ± tol` —
  a declared elimination that desugars to a conservation tie-assert
  (used in `models/fx_consol.fml`).

## Phase 2 (started): the incremental engine + WASM

- **`live::Session`** — incremental recalculation over the stored
  (measure × member × period) micro-graph: dirty propagation in plan order
  with **early cutoff** (a step re-runs only when a dependency's *value*
  changed; unchanged results stop propagation). In the Build-Systems-à-la-
  Carte taxonomy: topological scheduler + dirty-bit rebuilder + early
  cutoff — the option Excel never took. Tests prove exact agreement with
  full recompute, zero work on no-op edits, and later-period edits skipping
  earlier periods entirely (`tests/incremental.rs`).
- **WASM build** (`src/wasm.rs`, zero-dependency C-ABI):
  `cargo build --release --target wasm32-unknown-unknown`, then serve
  `www/`.
- **The workbench** (`www/index.html`) — the bi-representational surface
  from design doc 07: an editable **source pane** and an editable **grid
  projection** over one live session. Input rows render as in-place
  editable cells (grid edit → `set_input` → incremental recalc, changed
  cells flash, ~1 ms); source edits recompile the whole model debounced
  (~2 ms for the 467-step sculpting model) with checker errors shown
  inline by line; sub-range cells are dimmed; units annotate every row;
  asserts and scalars update live in the footer. Model switcher covers
  all three goldens.

## Not yet (by design — see the phase plan in 06-architecture.md)

- Floats, not exact decimal minor-units — the exact-decimal tower with
  typed rounding policies is an engine-phase representation change,
  deliberately not retrofitted onto the reference evaluator.
- `consolidate`/`observable` sugar from design doc 10 (translation written
  explicitly; semantics proven); one dimension; no multi-dimension
  broadcasting yet.
- **Grid → text write-back** (`Session::patch_input`, `fml_patch`): the
  first slice of the lossless-CST plan. The lexer carries byte spans; the
  parser records an *edit site* for every literal input value (map entries
  and broadcast literals); a grid edit rewrites exactly that span in the
  source — byte-identical everywhere else — and applies the same change
  incrementally. Tests prove the round-trip theorem (patch + incremental
  recalc ≡ fresh compile of the patched source), single-contiguous-span
  minimality, broadcast semantics (editing any cell of a uniform literal
  changes the one literal, hence all periods — what the text says), and
  that formula-defined inputs are refused (`tests/patch.rs`). The
  workbench grid now only offers edits it can express in the text, and
  the source pane updates live when the grid is edited.
- **Multi-dimension broadcasting** — the research's consensus core
  abstraction, generalized from one dimension to N. Any number of
  `dimension` declarations (`tree { … }` with a rollup group or flat
  `list { … }`); measures range over any subset (`over Product, Region,
  plan`); evaluation contexts are member *assignments* over all dims, and
  reads project the assignment onto the target's dimension set — so
  `price * volume` broadcasts a Product-only price over Region and time
  automatically. `sum[Dim](…)` aggregates one dimension (and rejects
  summing `local` units across currencies); `x[Member]` pins a coordinate,
  chained `x[Alpha][EU]` pins several; `x[Group]` rolls up a tree dim.
  Reading a measure with an unbound dimension is a compile error naming
  the fix. The micro-graph, incremental engine, and workbench all operate
  on (measure × tuple × period); all three golden models pass unchanged
  (`tests/multidim.rs`).
- **Scenarios** — the third pillar of the research trio. `scenario
  Downside from Base { g = { 2026: 2% } … }` declares a named, deliberately
  unweighted overlay of input overrides; `from` chains overlays (a child
  inherits its parent's overrides). Overrides are validated like inputs:
  computed targets rejected, units checked per member context, maps only
  on series. Evaluation is an **incremental delta from Base**: clone,
  apply the override chain, dirty-propagate — Base values stay untouched,
  and asserts/covenants re-run under the scenario. The round-trip theorem
  is tested: scenario evaluation ≡ fresh compile with the overrides baked
  into the source (`tests/scenarios.rs`). The workbench grows scenario
  chips with a diff-vs-Base view: changed cells tint green/red with the
  Base value as tooltip; editing is Base-only (scenario numbers come from
  the scenario text).
- **The budget-template workflow** (multi-owner models,
  `models/budget.fml`): input bodies may be `match Dim { Member -> <map or
  expr> … }` — each cost center/entity owns one arm, and every map entry
  (or broadcast literal) in an arm is a **member-aware edit site**. Grid
  cells carry (measure, member, period); a grid edit patches exactly that
  member's arm in the source, byte-untouched elsewhere, and re-rolls the
  totals incrementally (`tests/budget.rs`: member isolation, broadcast-arm
  semantics, envelope covenant + Squeeze scenario breach).
- **Multi-file models**: `include "cc_marketing.fml"` — expansion with
  cycle/depth guards, resolved relative to the model file (CLI) or by the
  host. Each owner owns a file; git merges become structurally
  conflict-free.
- **Per-file span provenance** — expansion keeps a **source map**
  (`expand_includes_with_map`): every byte of the expanded document traces
  back to (file, local offset). `Session::patch_input` routes each grid
  edit into the file that owns the span — the master file is
  byte-untouched when a team edits its numbers. The multi-file round-trip
  theorem is tested: patch + incremental recalc ≡ re-expanding the
  *patched files* and compiling fresh (`tests/include_patch.rs`). The
  workbench grew **file tabs**: `models/team_budget.fml` includes three
  team-owned files; editing a Marketing cell rewrites
  `team_marketing.fml` only (draft-dot on exactly that tab), included
  files are fetched on demand, per-file drafts survive reload, and source
  edits in any tab recompile the whole model.
- **The collaboration server** (`fml-server`, zero-dependency HTTP) —
  design doc 07 §3 v1: every mutation passes ONE gate — authorize →
  apply → log. **Dimension-subspace write ACLs** from a plain owners file
  (`alice: expenses[Marketing]`, `cfo: *`); an **append-only, attributed
  event log** that is the authority (boot = model file + replay; the
  replay theorem is tested: cold boot ≡ live state, sources byte-equal);
  and source write-back, so `GET /model` returns the budget with every
  owner's committed numbers in their own arms. Demonstrated end-to-end:
  alice writes Marketing (ok), alice writes Engineering (403), mallory
  writes anything (403), cfo raises the cap (measure-wide grant), server
  restarts and replays to identical state (`tests/collab.rs`). v1 trust
  model: claimed identity — real authentication is a deployment layer.
- **Workbench client mode** — open the workbench with
  `?server=http://host:5199&user=alice` and it becomes a connected client:
  the server evaluates, the browser projects. The header shows the
  connection; the source pane is read-only (server-owned truth, kept in
  sync by write-back); **cells are editable only where the user's grants
  intersect the edit sites** (alice sees exactly her Marketing cells);
  edits POST through the ACL gate and render as `commit #N`; peers'
  commits arrive via a sequence poll and flash in the grid within ~1.5 s.
  Verified live with two users on the budget model.
- **The actuals switchover** (`models/rolling.fml`) — the research's most
  load-bearing FP&A time construct, as one line:
  `sales = actuals sales_act until closed else prev(sales) * (1 + growth)`.
  Desugars onto period dispatch (`match t`), so the boundary handoff is
  exact: the first forecast month compounds off the last actual. A rolling
  forecast is `period closed = … .. 2026-06` advanced one month as actuals
  land — everything re-blends (`tests/rolling.rs`). Single-period `period
  close = 2026-06` form supported.
- **Tornado sensitivity, click-a-cell** — `Session::tornado` perturbs every
  literal-editable input site ±10% (runtime-only, fully restored, solve
  failures skipped), recalcs incrementally, and ranks impact on any chosen
  output cell. In the workbench: click any computed cell → ranked
  red/green bars. The demo teaches real economics: for December profit in
  the rolling model, the top driver is the JUNE ACTUAL (it re-bases the
  whole H2 forecast), ahead of margin and growth.
- **Distributions — the `simulate` leg**, completing the research trio.
  `input growth : rate ~ metalog { p10: 1%, p50: 3%, p90: 6% }` (3-term
  Keelin metalog, closed-form fit) plus `~ uniform(a,b)` and
  `~ normal(mean,sd)`. **Deterministic by default at the median** (the
  Naylor adoption lesson: what-if first, stochastic as an upgrade in the
  same artifact). `Session::simulate(n)` runs trial-aligned Monte Carlo
  with deterministic seeds (SIPmath posture — bit-reproducible
  everywhere), incremental recalc per trial, full restore, and per-cell
  [p10, p50, p90]. Workbench: a **simulate** chip switches the grid to
  band view (p50 with p10…p90 under each cell). 500 trials of the rolling
  model in 17 ms; actual months show zero-width bands (booked numbers are
  certain), forecast months fan out (`tests/simulate.rs`). Distribution
  inputs are excluded from the tornado — sensitivity, scenario, and
  simulation stay distinct constructs, as the literature demands.
- **Correlation + per-period draws** — `correlate growth, margin = 0.7`
  turns independent samples into coherent trial vectors via a Gaussian
  copula (Cholesky over the declared pair matrix, validated
  positive-definite at compile time), while every input's marginal stays
  exactly as assessed — the SLURP posture. `~ normal(0, 1) per period`
  draws a fresh shock each period (iid) instead of one draw per trial
  (parameter uncertainty); mixing frequencies inside one correlated group
  is a compile error. Statistical structure is tested through the model
  (`tests/correlate.rs`): N(0,1) sum/difference percentile widths land on
  their closed-form values, marginals survive the copula,
  anti-correlation narrows sums, and all seven misuse forms fail at
  compile time. In the rolling demo, margin is now `~ normal(22%, 2%)`
  correlated 0.7 with growth: January profit is banded (margin
  uncertainty applies to booked months too) while January sales stays
  certain.
- **Provenance — "explain this number"** (`Session::explain`,
  `fml_explain`): the research's one-engine thesis made tangible. For any
  cell: where it is defined — **routed to the owning file** through the
  include source map ("team_marketing.fml:2") — which `match`/`actuals`
  arm actually fired for that period ("match t → in m \ closed"), its
  nature (distribution + correlations, solve membership, literal
  editability), and every direct dependency cell with its value: `prev`
  references point at the previous period (or surface the `init` value at
  the range start), aggregates (`sum`, windows, `npv`, `irr`) list their
  constituent cells, tree rollups expand to leaves. Clicking a computed
  cell in the workbench now opens the **inspector**: dependency rows
  drill down (breadcrumb trail), dep cells highlight in the grid, the
  definition link jumps the source pane to the owning file and selects
  the defining line, and the tornado runs from a button inside the panel
  (`tests/explain.rs`).
- **Goal-seek** (`Session::goal_seek`, `fml_goalseek`) — the IFPS
  classic: which input value makes an output hit a target? Safeguarded
  secant iteration over runtime values (clamped steps so it survives
  passing through solve fixpoints), fully restored afterwards —
  committing the answer is a separate, explicit act. Linear goals land
  exactly in ~3 evaluations; the nonlinear compounding-growth goal and a
  goal *through the FINPLAN financing fixpoint* (share price via EBIT
  margin, re-solving Gauss–Seidel every evaluation) both converge;
  unresponsive levers are rejected with a clear error
  (`tests/goalseek.rs`). In the workbench the inspector grew a **goal
  seek…** form: pick a target and a lever (any literal-editable cell, or
  a scalar input), solve, then **apply** — routed through the grid→text
  write-back into the owning file, or falling back to a runtime-only set
  for distribution inputs. Footer scalars are now clickable, so
  "fy_profit = 350?" is: click, type 350, pick growth, solve (7 evals,
  ~600 µs), apply.
- **The `allocate` primitive** — the workhorse of every budgeting
  system: `allocate overhead_share : kEUR flow over CostCenter, plan =
  overhead by headcount` spreads a total across a dimension's members in
  proportion to a driver. Desugars (like `eliminate`) to the
  proportional split `total * driver / sum[Dim](driver)` **plus an
  auto-generated conservation tie-assert** (`allocate_overhead_share`)
  proving the pieces re-add to the pot every period. A dimensionless
  driver (`by 1`) gives an equal split; a zero driver-sum is a runtime
  error naming the allocation, not a silent NaN; time-varying drivers
  reshape the split period by period; and `explain` shows the full
  allocation basis for free — the pot, the member's own driver, and
  every member's driver via the sum (`tests/allocate.rs`). The budget
  model now carries overhead-by-headcount into per-member
  `loaded_cost`. The float-honest tie-assert tolerance (1e-6) is a
  placeholder for the exact-decimal phase, where conservation becomes
  exact with a remainder-goes-to-largest rounding discipline.
- **Quantified provenance** — explain now carries an **exact additive
  decomposition**: the taken branch's +/− structure, aggregate
  constituents, rollup leaves, and `npv`'s per-period discounted terms
  (the PV bridge) become signed terms that sum to the cell's value —
  never a sensitivity approximation. What can't be split additively
  stays one honestly-labeled term (`sales × margin`). The inspector
  renders them as **contribution bars** with share percentages (signed
  bridges read like a waterfall: +1,700 / −1,635 → 65), drillable when a
  term is a single cell, capped with a Σ-rest row beyond 14 terms
  (`tests/contrib.rs`). Terms compose with everything: the allocation's
  loaded_cost splits into own-spend vs allocated overhead, fy_profit
  into twelve monthly bars.
- Still ahead: full lossless CST, salsa memoization, LSP, exact decimals
  (+ exact allocation remainders), read-side information-flow control,
  real authentication.

## Layout

| Path | Contents |
|---|---|
| `src/lexer.rs` | hand-rolled lexer (zero deps) |
| `src/parser.rs` | recursive descent → AST |
| `src/ast.rs` | Phase-1 AST |
| `src/units.rs` | abelian-group units |
| `src/check.rs` | resolution, series/unit inference, init/kind rules, cycle analysis, scheduling |
| `src/eval.rs` | reference evaluator (correct before fast) |
| `models/` | golden models in `.fml` |
| `tests/` | golden + negative tests |
