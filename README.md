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
- **The budget portal** (`www/app.html`) — the enterprise front end for
  budget contributors and administrators (dashboard, entry grid,
  workflow, audit, admin); pure HTTP client of `fml-server`, no wasm.
  Described under slice 15 below.
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
- **Typed rounding + exact allocation remainders** — the cent-level
  slice of the exact-decimal plan. `round 2 half_up` on any measure
  snaps stored values to the declared decimals at STORE time (four
  policies: half_up, half_even, floor, ceil) — downstream readers see
  posted amounts, grid edits snap, and `round` inside a solve block is
  a compile error (rounding breaks fixpoint convergence). `allocate …
  round 2` computes shares in minor units with the residual distributed
  by **largest remainder** (ties → member order): 100.00 by equal
  thirds gives 33.34 / 33.33 / 33.33, and the slices re-add to the
  rounded pot **exactly in integer cents** — tested in cents, through
  incremental edits, and against the round-trip theorem
  (`tests/rounding.rs`). The budget model's overhead allocation now
  carries `round 2`; nudging a headcount produces 51.32 / 177.63 /
  71.05 — summing to 300.00 to the cent, conservation assert green.
  What remains for full exact decimals is the representation change
  (integer minor units end-to-end instead of snapped f64) — an engine
  concern, not a language one; the syntax and semantics are now fixed.
- **Authentication + the tamper-evident log** — the server's v2 trust
  model. Zero-dependency SHA-256/HMAC-SHA256 (`src/crypto.rs`, pinned to
  the FIPS/RFC 4231 test vectors); identity is a bearer token
  `user.<hmac(secret)>` minted with `fml-server token alice` — a claimed
  "user" field is ignored, and MAC comparison is constant-time. The
  event log is now a **hash chain**: each event is signed over the
  previous signature, so a retroactive edit, deletion, or reorder of
  history makes replay fail with "signature mismatch — refusing to
  serve" naming the first bad event (tail truncation loses commits but
  cannot forge them). The gate remains ONE line: verify token → ACL →
  apply → sign → append. Client mode connects with `?token=…`
  (`tests/auth.rs`; demonstrated live: 401 without/with forged token,
  403 across ACL, signed commits, and a boot refusal on a
  retroactively edited log).
- **The lossless CST, slice 1** (`src/cst.rs`) — the foundation of the
  editor-tooling arc. A rowan-style **green tree** (position-independent
  nodes; tokens carry the exact source bytes — comments, whitespace,
  `1_000_000` spellings, `include` directives) plus a lazy **red
  cursor** computing absolute offsets on the way down. Assembly needs no
  second grammar: the trivia-preserving lexer (`lex_full`) and the
  existing parser's recorded declaration boundaries build the tree, so
  parser and CST cannot drift. Granularity is the top-level declaration;
  each declaration owns its leading trivia (a comment above a measure
  moves with it) and its same-line trailing comment. The defining
  theorem holds over every model in the repo:
  `reprint(parse_cst(text)) == text`, byte for byte. Structural edits
  (`with_child_removed/inserted/replaced`) rebuild only the root spine —
  removing the Squeeze scenario reprints as the source minus exactly
  those bytes, reinserting restores byte identity, and every untouched
  declaration is shared by reference (`Rc::ptr_eq`), tested in
  `tests/cst.rs`. (Bonus: compiling a file with unexpanded includes now
  fails with "resolve includes first" instead of a lexer error.)
- **Error-resilient parsing + the salvage compile (CST slice 2)** — a
  broken declaration no longer kills the file. `Parser::parse_resilient`
  catches the error, resyncs at the next declaration start (line-leading
  keyword or `ident :`/`ident =` at brace depth 0), records a ParseError
  with the declaration's span, and keeps going; the CST marks the region
  as an **ErrorDecl node** and still reprints byte-exactly — the tree
  exists even while you type garbage (include fragments without a
  `model` header now get CSTs too). On top sits `parse_salvage`: broken
  declarations AND their transitive dependents (measures, asserts,
  solves, scenarios, correlations, edit sites — cascaded via reference
  analysis to a fixed point) are dropped with reasons, and if the
  remainder checks, the workbench serves it live. Breaking `sales`
  mid-edit now shows an **amber warning** — "line 19: expected an
  expression, found + (also omitted: profit, fy_profit)" — over a live,
  undimmed grid of everything that survives, instead of freezing the
  whole model (`tests/resilient.rs`). Strict mode is untouched: the
  compiler still stops at the first error.
- **Edit sites from the CST (slice 3)** — the hand-rolled span-shifting
  arithmetic is gone. Each edit site is now a **token path** (owning
  declaration, token range) located once at session build; byte spans
  are DERIVED from the red tree whenever needed, never stored, never
  shifted. Replacements re-lex to the same token count by construction
  (`Num→Num`, `22%→Pct`, `12 USD→Num·Ws·Ident`), so every path stays
  valid across any edit sequence; a grid edit rebuilds one declaration
  node plus the root spine, and the session source IS the tree's
  reprint. All fourteen round-trip/byte-exactness theorems in the suite
  now run through this path unchanged, plus new ones: six
  length-changing patches hammering one map literal, Qty three-token
  replacement, and reprint stability under editing
  (`tests/cst_sites.rs`). (The include segment map remains positional —
  its CST-native replacement is per-file trees, a later slice.)
- **Structural edits (CST slice 4)** — the first user-visible payoff of
  the tree: whole-model operations as source transformations that
  recompile cleanly with formatting preserved. **Add a period**: one
  click bumps the calendar's end literal and extends every FULL-RANGE
  map input with a copy of its last entry — across files (each team's
  map grows in its own file; broadcast literals and sub-range maps like
  closed actuals are correctly untouched). **Rename a measure**:
  token-exact rewriting of the declaration and every reference across
  every file, guarded against collisions with every namespace (measures,
  members, dimensions, units, ranges, scenarios, keywords); comments are
  deliberately left alone. Both are Session methods returning new file
  texts routed through the flat CST + include source map
  (`tests/edits.rs`); the workbench grew a **+ period** header button,
  clickable row names (every row, inputs included, now opens the
  inspector), and a **rename…** form inside it. Verified live on the
  multi-file budget: 2030 appears in four files at once; renaming
  marketing_spend rewrites the team file's declaration and the master's
  formula in 400 µs.
- **Add-member (completing the structural-edit trio)** — "we opened a
  fourth cost center" is now one form: the member joins the dimension's
  list, and an arm `Member -> default` is inserted into **every**
  `match Dim { … }` block — token-scanned with brace-depth tracking,
  multi-line blocks get the arm on its own line, inline blocks stay
  inline, and blocks with an `else` are left alone (it already covers
  the newcomer). Tree rollups, allocations, and the conservation
  asserts pick the member up automatically, and the new member's cells
  are **immediately grid-editable** (write-back included) after the
  reload. Guards mirror rename's (all namespaces + keywords); the
  functional dimension is refused with guidance (a new entity needs a
  currency mapping). Verified live: adding Support to the budget grew
  four member rows, kept both asserts green, and an immediate edit of
  expenses[Support] re-rolled totals incrementally
  (`tests/edits.rs`).
- **Expression-level granularity + formula editing (slice 5)** — the
  tree deepens below declarations: **Body** (everything after `=`),
  **MapEntry** (`period: value`), and **MatchArm** nodes, recorded by
  the real parser as it parses (three instrumentation points — no
  second grammar) and nested by containment during CST assembly. Edit
  sites generalized from (decl, tokens) to full **tree paths**, and the
  structural-edit token scans moved to a flattened iterator, so all
  fourteen round-trip theorems pass unchanged over the deeper tree. On
  top, the first formula-level operation: the inspector now SHOWS a
  cell's formula and offers **edit formula…** — syntax pre-checked
  ("not a valid formula: expected an expression, found end of file"),
  replacing exactly the Body node's bytes (trailing trivia preserved),
  routed to the owning file in multi-file models
  (`tests/expr_nodes.rs`).
- **Per-file trees (slice 6)** — the last positional arithmetic is
  gone. Every file now carries its OWN lossless CST (fragments parse via
  the resilient path, verified lossless at session build), and every
  edit site carries a second tree path into its owning file's tree. A
  grid patch replaces the literal's tokens in BOTH trees and both texts
  become reprints — no byte splicing anywhere, no segment offsets to
  shift. The segment map stopped being stored state: it is **derived on
  demand by re-expanding the current file texts**, guarded by the
  lockstep invariant `expand(file texts) == flat source` (tested after
  every patch of a six-edit cross-file sequence). Structural edits and
  `locate_line` route through fresh segments, so they stay correct even
  after length-changing patches have moved everything
  (`tests/file_trees.rs`). Regions included more than once are detected
  at session build and marked non-editable, not discovered mid-patch.
- **The salsa-style incremental reload (slice 7)** — early cutoff for
  the compiler itself. Every declaration gets a **semantic fingerprint**
  (FNV over its non-trivia tokens), and reload compares sequences: if
  they match — flat AND per file, so a declaration moved between files
  is caught even when the flat text agrees — the entire analysis and
  runtime state are kept. Edit-site paths are relocated by **token
  ordinal** (immune to trivia shifts), declaration line numbers refresh
  so explain stays accurate, and the stats line reads "no semantic
  change, analysis reused: 0/0 steps". A semantic edit rebuilds and
  **names the culprit** ("reanalyzed (margin)"). Reordering
  declarations is conservatively a rebuild (order is semantics for
  solves). The governing theorem, tested across alternating edit
  sequences: reload ≡ fresh compile, always — and a grid patch
  immediately after a trivia shift lands on exactly the right tokens
  (`tests/incr.rs`). Comment edits on any file are now FREE.
- **Per-declaration queries, slice 1: references + the blast radius
  (slice 8)** — the first cached check query. Each declaration's
  `references` (every name its body and init mention, solve members
  included) is memoized by semantic fingerprint and survives reloads —
  the second consecutive edit shows one cache miss and a full row of
  hits. On top of it, a semantic edit to a plain measure/input/allocate
  now re-evaluates only the **blast radius** — the changed measures
  plus transitive dependents — copying every out-of-radius value from
  the old session: editing budget_cap runs 4 steps (cap + headroom),
  not 40, and the stats line reads "reanalyzed (budget_cap) → 2
  affected · 7 query hits". The conservatism is principled: if the
  radius touches a solve fixpoint, evaluation falls back to full —
  warm-started Gauss–Seidel converges to a slightly different point,
  and the governing theorem (incremental ≡ from-scratch, bit-exact,
  every cell) is non-negotiable; that theorem CAUGHT this exact case
  during development. Structure edits (calendar, dimensions, scenarios,
  asserts, solves) rebuild fully (`tests/queries.rs`). Analysis itself
  (unit inference, scheduling) is still whole-model — the remaining
  deepening, paired with the LSP that would consume it.
- **The LSP (slice 9)** — `fml-lsp`, a zero-dependency Language Server
  over stdio (hand-rolled JSON-RPC framing + a ~250-line JSON module),
  serving straight from the Session the whole arc built:
  **diagnostics** on open/change (resilient parse errors with exact
  lines, salvage-dropped dependents as warnings, check errors located);
  **hover** with the declaration facts, distribution/solve notes, the
  formula, and LIVE values; **go-to-definition** that is include-aware —
  jumping to `spend` from the master file lands in `team.fml` on disk;
  and **document symbols** (this file's declarations only). Reloads run
  through the salsa path, so trivia keystrokes cost nothing. Verified by
  a full end-to-end protocol test that spawns the real binary and
  speaks LSP to it: initialize → open (clean) → hover (values inline) →
  definition (into the include) → symbols → break (error surfaces) →
  fix (diagnostics clear) → shutdown (`tests/lsp.rs`). Wire it to any
  editor as a generic LSP for `.fml`: command `fml-lsp`, stdio
  transport, no arguments.
- **The IDE editor (slice 10)** — the workbench's source pane grew up:
  a zero-dependency code editor built as a highlight overlay (a
  mirrored `<pre>` behind a transparent-text textarea, scroll-synced,
  with a line-number gutter). Colors come from the REAL lexer via
  `fml_tokens`, with **semantic classes** the session provides:
  keywords purple, measures blue, dimension members teal, units green,
  numbers amber, comments italic — a name is colored by what it IS in
  this model, not by regex guesswork. **Error lines** highlight in red
  from the resilient parser's exact locations, routed through the
  source map to the active file, clearing the moment the model
  compiles. **Completion** pops as you type (or Ctrl+Space): measures
  with their units, members with their dimension, units, ranges,
  keywords — from the live session via `fml_complete` — with
  arrow/Tab/Enter/Escape keys and caret-anchored positioning. The same
  candidates now serve `textDocument/completion` in fml-lsp, covered by
  the protocol test.
- **UI focus pass (slice 11)** — the workbench reorganized around what a
  modeler actually checks. **Model health first**: the asserts moved
  from footer fine-print to header pills — green at a glance, a failing
  covenant throbs red the moment an edit breaks it. **A scannable
  grid**: negative numbers in red (finance eyes find the problem row
  instantly), input rows tinted, row hover highlighting across wide
  grids, a shadow edge on the sticky name column. **A calm header**:
  the model list became a dropdown, actions grouped behind a divider,
  and a **code / split / grid** view toggle plus a draggable pane
  divider (both persisted) — grid-only for review meetings, code-only
  for authoring. **Orientation**: an amber banner announces scenario
  view ("viewing scenario Squeeze — cells colored vs Base") with a
  one-click return; the window title names the model; a quiet footer
  hint teaches the click affordances; Escape closes any panel.
- **The budget process (slice 12)** — the server grew from a demo gate
  into a governed budget round. A config directory declares everything:
  `users.cfg` (user: **department role** — admin | editor | viewer),
  `access.cfg` (per model: which departments may READ it at all —
  department-restricted fmls — and which measures each department's
  editors may write), `models/` and per-model signed logs. Roles are
  structural, not advisory: **editors can only alter inputs** because
  /patch reaches only literal input sites by construction; formulas are
  a separate admin-only endpoint; viewers hold no write path at all.
  The round itself — **submit** (freezes the department's editors),
  **reopen**, **lock** (final) — flows through the SAME hash-chained
  log as the numbers, so *who submitted when* is tamper-evident audit
  history and a restart replays values, formula changes, and process
  state together ("replayed 7 events, chain verified" → locked, with
  the admin's formula edit persisted in the source). One gate: verify
  token → model read access → process state → role → grants → apply →
  sign → append (`tests/process.rs`). The workbench client mode shows a
  **process banner** (dept · role · round status), a model picker of
  what YOU may read, a submit button for editors, reopen/lock controls
  for admins — and cells freeze the moment your department submits.
- **LSP rename + references (slice 13)** — the last editor verbs.
  `textDocument/references` lists every occurrence of a measure across
  every file (declaration and uses, includes included);
  `prepareRename` returns the exact token range; `rename` rides
  `rename_measure` — all its namespace-collision guards intact — and
  returns a **WorkspaceEdit spanning the owning files**, so renaming
  `spend` from the master file rewrites `team.fml`'s declaration in the
  same editor transaction. Renaming to a keyword answers with a proper
  JSON-RPC error ("'round' is not a valid measure name"). Covered by
  the end-to-end protocol test (`tests/lsp.rs`).
- **The budget-management system + ACME Industrial (slice 14)** — the
  capstone. `models/acme/` is a complete industrial-company budget:
  product-line revenue (volumes × prices over a Line dimension), direct
  costs (materials, energy, labor), plant overhead ALLOCATED to lines
  with cent-exact conservation, personnel, a capex → depreciation →
  asset-base roll-forward, EBITDA/EBIT, four covenants, and a Downturn
  scenario — five files, each owned by a department (sales, production,
  HR, maintenance, finance). The server grew the management layer:
  **timestamped audit events** (wall clock in the signed chain),
  **GET /users** (the directory, admin-only), **POST /mint** (admins
  issue teammate tokens from the UI), and **POST /checkpoint** — the
  budget-cycle persistence act: write the approved numbers back to the
  model files on disk, archive the signed log, and open the next round
  on that baseline (lock → checkpoint → new round). The workbench
  gained a **token landing page** and an **admin console** (people &
  roles with one-click token minting, a readable timestamped audit
  timeline, checkpoint with confirmation). The whole round is locked in
  CI by an HTTP end-to-end test that spawns the real server: access
  restriction, the gate matrix, minting, lock, checkpoint-to-disk, and
  round two beginning on the new baseline (`tests/server_e2e.rs`,
  `tests/industrial.rs`).
- **The budget portal (slice 15)** — `www/app.html`, a second
  zero-dependency single-file front end aimed at the people who *don't*
  write fml: an enterprise EPM-style portal in the Hyperion / Anaplan
  mold. Dark-navy navigation sidebar with the signed-in identity chip;
  a **dashboard** of KPI cards with inline-SVG sparklines (top computed
  measures by FY magnitude), covenant status lights, and round status;
  a **budget-entry grid** in classic EPM grammar — amber cells you may
  type into, white computed cells, measures grouped with indented
  dimension members, finance-style `(1,234)` negatives, sticky
  header/measure column; a **workflow board** of department cards with
  submit/reopen/lock controls; the **audit timeline** and the **admin
  console** (people & roles, one-click token minting, checkpoint) as
  first-class pages. It speaks only HTTP to `fml-server` — no wasm —
  so a contributor's browser never even loads the compiler. The grid
  enforces nothing itself; every cell's editability is derived from the
  server's structural edit-sites, the department grants, and the round
  state, and the server re-checks every patch. Live-syncs on a 2-second
  `/seq` poll: one user's submit flips everyone else's cells read-only
  within a beat. Sign-in is a token landing page with localStorage
  persistence.
- **Workbench restyle (slice 16)** — `www/index.html` now shares the
  portal's design system (same tokens: navy chrome, blue accent, amber
  inputs, light surface, committed `color-scheme: light`). Navy app bar
  with brand block, segmented view control, and assert pills; UI chrome
  in the system sans, monospace confined to the code editor and grid
  numbers (tabular); enterprise grid styling (uppercase muted headers,
  amber input rows, red negatives, portal-blue change flash); inspector,
  tornado, admin console, and token landing as portal-style cards. Two
  functional fixes surfaced by verification: the client-mode source
  pane now renders as plain text through the same overlay (previously
  invisible — highlighting needs the local wasm lexer, which client
  mode never loads), and grid inputs carry `autocomplete="off"` plus a
  no-op guard in the change handler, after a browser form-state restore
  during reload resurrected a stale cell value and committed a phantom
  patch into the signed audit log.
- **The model view (slice 17)** — model management inside the workbench.
  `Session::model_info_json` is the model's structural self-description:
  the include hierarchy (directives read from the lossless lexer, decls
  grouped by owning file), every measure with unit/dims/kind flags
  (input, solve, stochastic, rounded, literal-editable) and the exact
  reference graph — refs from the checked bodies, dependents as its
  transpose — plus dims with roll-up groups, asserts with the measures
  they constrain, scenario chains, and correlations. Exposed as
  `fml_model_info` (wasm) and `GET /info` (server; `/grants` grew
  dept/role per user). Locked in CI by `tests/info.rs`: graph symmetry,
  include ownership (a fragment's measure locates in ITS file), flags.
  The workbench gains a fourth view — **model** — in both local and
  client mode (the view toggle now survives into connected sessions):
  an overview strip; an **architecture DAG** (measures layered by
  dependency depth, inputs amber → intermediates → outputs → assert
  nodes lit green/red by live results; hover traces edges, click opens
  a detail panel with built-from/feeds-into links, double-click jumps
  to the definition); a **files & includes tree** (decl mix per file,
  click opens the owning tab in the code view); **dimensions &
  roll-up** (group = Σ ← members, correlations); and **access & write
  privileges** — connected: the full user × grants matrix with the
  caller's row highlighted; local: the literal-editable input set and
  a pointer to server mode.
- **Menu bar (slice 18)** — the workbench header reorganized from a
  strip of buttons into a desktop-app menu bar: **File** (save to disk,
  draft state with revert), **Model** (simulate uncertainty, add
  period, add member with its inline form living inside the dropdown),
  **Scenario** (checkmarked list; the menu label carries the active
  scenario; hidden when the model declares none, and hidden in client
  mode where scenario preview would need the local wasm session — which
  also retires a latent crash), and **View** (code/split/grid/model,
  checkmarked). The assert pills collapsed into a single status
  indicator at the right — "✓ 4 checks" green or "✗ 1 check failing"
  red and throbbing — with the per-assert detail in its dropdown.
  One menu open at a time, outside-click and Escape close, items close
  on activation (`data-keep` for forms). Client mode keeps only what a
  contributor needs: brand, model, View, checks, and the process strip.
- Still ahead — per-declaration unit-inference/scheduling queries,
  integer minor-unit representation, read-side information-flow
  control, TLS / reverse-proxy deployment.

## Layout

| Path | Contents |
|---|---|
| `src/lexer.rs` | hand-rolled lexer (zero deps) |
| `src/parser.rs` | recursive descent → AST |
| `src/ast.rs` | Phase-1 AST |
| `src/units.rs` | abelian-group units |
| `src/check.rs` | resolution, series/unit inference, init/kind rules, cycle analysis, scheduling |
| `src/eval.rs` | reference evaluator (correct before fast) |
| `models/acme/` | THE example: the ACME Industrial corporate budget (5 files, one per department) |
| `tests/fixtures/` | golden models backing the test suite (finplan, solar PF, FX consolidation, …) |
| `tests/` | golden + negative tests |
