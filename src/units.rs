//! Units of measure as a free abelian group over base-unit names, extended
//! with a scale factor so `unit kEUR = 1000 EUR` is a first-class scaled
//! unit: same dimension as EUR, different scale. Adding kEUR to EUR is a
//! type error; `x in kEUR` performs the (rate-less) scale conversion.

use std::collections::BTreeMap;
use std::fmt;

#[derive(Clone, Debug)]
pub struct Unit {
    pub dims: BTreeMap<String, i32>,
    pub scale: f64,
}

impl PartialEq for Unit {
    fn eq(&self, other: &Self) -> bool {
        self.dims == other.dims && self.scale == other.scale
    }
}

impl Unit {
    pub fn one() -> Self {
        Unit { dims: BTreeMap::new(), scale: 1.0 }
    }

    pub fn base(name: &str) -> Self {
        let mut m = BTreeMap::new();
        m.insert(name.to_string(), 1);
        Unit { dims: m, scale: 1.0 }
    }

    pub fn scaled(base: &Unit, factor: f64) -> Self {
        Unit { dims: base.dims.clone(), scale: base.scale * factor }
    }

    pub fn is_dimensionless(&self) -> bool {
        self.dims.is_empty() && self.scale == 1.0
    }

    pub fn same_dimension(&self, other: &Unit) -> bool {
        self.dims == other.dims
    }

    pub fn mul(&self, other: &Unit) -> Unit {
        let mut m = self.dims.clone();
        for (k, v) in &other.dims {
            let e = m.entry(k.clone()).or_insert(0);
            *e += v;
            if *e == 0 {
                m.remove(k);
            }
        }
        Unit { dims: m, scale: self.scale * other.scale }
    }

    pub fn inv(&self) -> Unit {
        Unit {
            dims: self.dims.iter().map(|(k, v)| (k.clone(), -v)).collect(),
            scale: 1.0 / self.scale,
        }
    }

    pub fn div(&self, other: &Unit) -> Unit {
        self.mul(&other.inv())
    }
}

impl fmt::Display for Unit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.dims.is_empty() && self.scale == 1.0 {
            return write!(f, "1");
        }
        let pos: Vec<String> = self
            .dims
            .iter()
            .filter(|(_, v)| **v > 0)
            .map(|(k, v)| if *v == 1 { k.clone() } else { format!("{k}^{v}") })
            .collect();
        let neg: Vec<String> = self
            .dims
            .iter()
            .filter(|(_, v)| **v < 0)
            .map(|(k, v)| if *v == -1 { k.clone() } else { format!("{k}^{}", -v) })
            .collect();
        let num = if pos.is_empty() { "1".to_string() } else { pos.join("·") };
        let body = if neg.is_empty() {
            num
        } else {
            format!("{num}/{}", neg.join("·"))
        };
        if self.scale == 1.0 {
            write!(f, "{body}")
        } else {
            write!(f, "{}×{body}", self.scale)
        }
    }
}
