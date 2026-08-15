# OpenFML design system (v2 — "engraved ledger")

All tokens OKLCH, tinted toward hue 255 (ink blue). CSS custom property
NAMES are frozen (JS templates reference them): --navy --navy2 --navy3
--bg --card --line --ink --mut --pri --pri2 --ok --okbg --warn --warnbg
--bad --badbg --amber --amber2 --accent --dim --mono --sans.

- Shell (sidebar) : --navy  oklch(21% .022 262)  near-black ink
                    --navy2 oklch(27% .026 262)  raised ink
                    --navy3 oklch(34% .028 262)  ink hairline
- Surfaces        : --bg    oklch(97.3% .004 250) paper
                    --card  oklch(99.2% .002 250) panel
                    --line  oklch(91% .008 252)   hairline
                    --dim   oklch(94.5% .006 250) recessed
- Text            : --ink   oklch(26% .02 260)    primary
                    --mut   oklch(51% .015 258)   secondary
- Accent          : --pri   oklch(49% .16 262)    ink-blue (≤10% of surface)
                    --pri2  oklch(94% .035 262)   accent wash
- Semantics       : ok 55% .12 155 · warn 54% .11 75 · bad 53% .17 27
                    (+ 95-96% washes: --okbg --warnbg --badbg)
- Editable cells  : --amber oklch(97% .022 90) warm paper; hover --amber2
- Type: --sans system stack; --mono SF Mono. Scale 11/12.5/13/15/18,
  weights 400/500/650, small-caps labels ls .07em. Numbers always
  tabular-nums, right-aligned.
- Radii 6/8/10; shadows: sm 0 1px 2px oklch(25% .02 260 / .05);
  lg 0 16px 40px oklch(25% .02 260 / .16). Motion: 120ms ease-out only
  on color/opacity/transform.
- Bans honored: no border-left accents, no gradient text, no glass, no
  hero metrics, no identical card grids, no #fff/#000.
