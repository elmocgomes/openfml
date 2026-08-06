//! Calendars and named period ranges. Grains: yearly, quarterly, monthly.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Grain {
    Yearly,
    Quarterly,
    Monthly,
}

/// A period literal as written: `2026`, `2026-Q3`, or `2026-07`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PeriodLit {
    pub year: i64,
    /// Sub-period number (quarter or month), if any.
    pub sub: Option<u8>,
    /// Whether the sub-period was written in `Qn` form.
    pub q_form: bool,
}

#[derive(Clone, Debug)]
pub struct Calendar {
    pub name: String,
    pub grain: Grain,
    start_year: i64,
    start_sub: u8, // 1-based quarter or month; 1 for yearly
    pub len: usize,
}

fn units_per_year(grain: Grain) -> i64 {
    match grain {
        Grain::Yearly => 1,
        Grain::Quarterly => 4,
        Grain::Monthly => 12,
    }
}

impl Calendar {
    pub fn new(name: String, grain: Grain, start: PeriodLit, end: PeriodLit) -> Result<Self, String> {
        let sub_of = |lit: &PeriodLit| -> Result<u8, String> {
            match grain {
                Grain::Yearly => {
                    if lit.sub.is_some() {
                        Err("yearly calendar bounds must be plain years".into())
                    } else {
                        Ok(1)
                    }
                }
                Grain::Quarterly => match (lit.sub, lit.q_form) {
                    (Some(q), true) if (1..=4).contains(&q) => Ok(q),
                    _ => Err("quarterly calendar bounds need Qn form (e.g. 2026-Q1)".into()),
                },
                Grain::Monthly => match (lit.sub, lit.q_form) {
                    (Some(m), false) if (1..=12).contains(&m) => Ok(m),
                    _ => Err("monthly calendar bounds need month form (e.g. 2026-01)".into()),
                },
            }
        };
        let (sq, eq) = (sub_of(&start)?, sub_of(&end)?);
        let upy = units_per_year(grain);
        let a = start.year * upy + (sq as i64 - 1);
        let b = end.year * upy + (eq as i64 - 1);
        if b < a {
            return Err("calendar end before start".into());
        }
        Ok(Calendar {
            name,
            grain,
            start_year: start.year,
            start_sub: sq,
            len: (b - a + 1) as usize,
        })
    }

    pub fn periods_per_year(&self) -> u32 {
        units_per_year(self.grain) as u32
    }

    fn ordinal(&self, lit: &PeriodLit) -> Result<i64, String> {
        match self.grain {
            Grain::Yearly => {
                if lit.sub.is_some() {
                    return Err(format!("calendar '{}' is yearly; period has a sub-period", self.name));
                }
                Ok(lit.year)
            }
            Grain::Quarterly => match (lit.sub, lit.q_form) {
                (Some(q), true) if (1..=4).contains(&q) => Ok(lit.year * 4 + (q as i64 - 1)),
                _ => Err(format!("calendar '{}' is quarterly; write periods as YYYY-Qn", self.name)),
            },
            Grain::Monthly => match (lit.sub, lit.q_form) {
                (Some(m), false) if (1..=12).contains(&m) => Ok(lit.year * 12 + (m as i64 - 1)),
                _ => Err(format!("calendar '{}' is monthly; write periods as YYYY-MM", self.name)),
            },
        }
    }

    fn start_ordinal(&self) -> i64 {
        self.start_year * units_per_year(self.grain) + (self.start_sub as i64 - 1)
    }

    /// Timeline index of a period literal; error when outside the calendar.
    pub fn index(&self, lit: &PeriodLit) -> Result<usize, String> {
        let o = self.ordinal(lit)? - self.start_ordinal();
        if o < 0 || o as usize >= self.len {
            return Err(format!("period {} is outside calendar '{}'", show(lit), self.name));
        }
        Ok(o as usize)
    }

    /// Like `index`, but also accepts the single period just before the
    /// calendar start (for init labels). Returns a signed index.
    pub fn index_or_prev(&self, lit: &PeriodLit) -> Result<isize, String> {
        let o = self.ordinal(lit)? - self.start_ordinal();
        if o == -1 || (o >= 0 && (o as usize) < self.len) {
            Ok(o as isize)
        } else {
            Err(format!("period {} is outside calendar '{}'", show(lit), self.name))
        }
    }

    pub fn year_of(&self, t: usize) -> i64 {
        let o = self.start_ordinal() + t as i64;
        o.div_euclid(units_per_year(self.grain))
    }

    pub fn label(&self, t: usize) -> String {
        let o = self.start_ordinal() + t as i64;
        match self.grain {
            Grain::Yearly => format!("{o}"),
            Grain::Quarterly => format!("{}-Q{}", o.div_euclid(4), o.rem_euclid(4) + 1),
            Grain::Monthly => format!("{}-{:02}", o.div_euclid(12), o.rem_euclid(12) + 1),
        }
    }
}

pub fn show(lit: &PeriodLit) -> String {
    match (lit.sub, lit.q_form) {
        (Some(q), true) => format!("{}-Q{}", lit.year, q),
        (Some(m), false) => format!("{}-{:02}", lit.year, m),
        (None, _) => format!("{}", lit.year),
    }
}

/// A named contiguous sub-range of the timeline, inclusive.
#[derive(Clone, Debug)]
pub struct PeriodRange {
    pub name: String,
    pub start: usize,
    pub end: usize,
}

impl PeriodRange {
    pub fn contains(&self, t: usize) -> bool {
        t >= self.start && t <= self.end
    }
}
