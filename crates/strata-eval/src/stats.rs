//! Paired significance tests for comparing two retrieval systems on the
//! same queries (per-query metric values `a[i]`, `b[i]`).
//!
//! * **Paired randomization (permutation) test**: under the null hypothesis
//!   "the systems are interchangeable", each per-query difference is equally
//!   likely to have either sign. Flip signs at random many times and count
//!   how often the mean difference is at least as extreme as observed. No
//!   distributional assumption; recommended for IR by Smucker et al. (2007).
//! * **Paired t-test**: t = mean(d) / (sd(d) / sqrt(n)), two-sided p from
//!   Student's t with n-1 degrees of freedom. The t CDF uses the regularized
//!   incomplete beta function (continued fraction, as in *Numerical
//!   Recipes*); `scripts/crosscheck_stats.py` compares it with SciPy.

use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
pub struct PairedTest {
    pub n: usize,
    pub mean_a: f64,
    pub mean_b: f64,
    /// mean(b - a)
    pub mean_diff: f64,
    pub t_stat: f64,
    pub p_t_test: f64,
    pub p_randomization: f64,
    pub randomization_samples: usize,
}

/// Run both tests on paired samples (b - a).
pub fn paired(a: &[f64], b: &[f64], samples: usize, seed: u64) -> PairedTest {
    assert_eq!(a.len(), b.len());
    let n = a.len();
    let d: Vec<f64> = a.iter().zip(b).map(|(x, y)| y - x).collect();
    let mean_d = d.iter().sum::<f64>() / n as f64;
    let var = d.iter().map(|x| (x - mean_d).powi(2)).sum::<f64>() / (n as f64 - 1.0);
    let (t, p_t) = if var > 0.0 {
        let t = mean_d / (var / n as f64).sqrt();
        (t, 2.0 * student_t_sf(t.abs(), (n - 1) as f64))
    } else {
        (0.0, 1.0)
    };
    PairedTest {
        n,
        mean_a: a.iter().sum::<f64>() / n as f64,
        mean_b: b.iter().sum::<f64>() / n as f64,
        mean_diff: mean_d,
        t_stat: t,
        p_t_test: p_t.min(1.0),
        p_randomization: randomization(&d, samples, seed),
        randomization_samples: samples,
    }
}

/// Two-sided sign-flip randomization test on differences `d`.
/// p = (#{|mean(flipped)| >= |mean(d)|} + 1) / (samples + 1).
pub fn randomization(d: &[f64], samples: usize, seed: u64) -> f64 {
    let n = d.len() as f64;
    let observed = (d.iter().sum::<f64>() / n).abs();
    let mut rng = seed | 1;
    let mut extreme = 0usize;
    for _ in 0..samples {
        let mut sum = 0.0;
        for (i, x) in d.iter().enumerate() {
            // One random bit per element from a 64-bit xorshift state.
            if i % 64 == 0 {
                rng ^= rng << 13;
                rng ^= rng >> 7;
                rng ^= rng << 17;
            }
            sum += if (rng >> (i % 64)) & 1 == 1 { *x } else { -*x };
        }
        if (sum / n).abs() >= observed - 1e-12 {
            extreme += 1;
        }
    }
    (extreme + 1) as f64 / (samples + 1) as f64
}

/// Survival function P(T > t) of Student's t with `df` degrees of freedom.
pub fn student_t_sf(t: f64, df: f64) -> f64 {
    let x = df / (df + t * t);
    let tail = 0.5 * reg_inc_beta(df / 2.0, 0.5, x);
    if t >= 0.0 {
        tail
    } else {
        1.0 - tail
    }
}

fn ln_gamma(x: f64) -> f64 {
    // Lanczos approximation (g = 7, n = 9), accurate to ~1e-15.
    const C: [f64; 9] = [
        0.999_999_999_999_809_9,
        676.520_368_121_885_1,
        -1_259.139_216_722_402_8,
        771.323_428_777_653_1,
        -176.615_029_162_140_6,
        12.507_343_278_686_905,
        -0.138_571_095_265_720_12,
        9.984_369_578_019_572e-6,
        1.505_632_735_149_311_6e-7,
    ];
    if x < 0.5 {
        return (std::f64::consts::PI / (std::f64::consts::PI * x).sin()).ln() - ln_gamma(1.0 - x);
    }
    let x = x - 1.0;
    let mut a = C[0];
    let t = x + 7.5;
    for (i, c) in C.iter().enumerate().skip(1) {
        a += c / (x + i as f64);
    }
    0.5 * (2.0 * std::f64::consts::PI).ln() + (x + 0.5) * t.ln() - t + a.ln()
}

/// Regularized incomplete beta I_x(a, b).
pub fn reg_inc_beta(a: f64, b: f64, x: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    if x >= 1.0 {
        return 1.0;
    }
    let ln_front = ln_gamma(a + b) - ln_gamma(a) - ln_gamma(b) + a * x.ln() + b * (1.0 - x).ln();
    // Use the continued fraction where it converges fast; symmetry otherwise.
    if x < (a + 1.0) / (a + b + 2.0) {
        ln_front.exp() * beta_cf(a, b, x) / a
    } else {
        1.0 - ln_front.exp() * beta_cf(b, a, 1.0 - x) / b
    }
}

/// Lentz's continued fraction for the incomplete beta function.
fn beta_cf(a: f64, b: f64, x: f64) -> f64 {
    const TINY: f64 = 1e-300;
    let (qab, qap, qam) = (a + b, a + 1.0, a - 1.0);
    let mut c = 1.0;
    let mut d = 1.0 - qab * x / qap;
    if d.abs() < TINY {
        d = TINY;
    }
    d = 1.0 / d;
    let mut h = d;
    for m in 1..300 {
        let m = m as f64;
        let m2 = 2.0 * m;
        let aa = m * (b - m) * x / ((qam + m2) * (a + m2));
        d = 1.0 + aa * d;
        if d.abs() < TINY {
            d = TINY;
        }
        c = 1.0 + aa / c;
        if c.abs() < TINY {
            c = TINY;
        }
        d = 1.0 / d;
        h *= d * c;
        let aa = -(a + m) * (qab + m) * x / ((a + m2) * (qap + m2));
        d = 1.0 + aa * d;
        if d.abs() < TINY {
            d = TINY;
        }
        c = 1.0 + aa / c;
        if c.abs() < TINY {
            c = TINY;
        }
        d = 1.0 / d;
        let del = d * c;
        h *= del;
        if (del - 1.0).abs() < 1e-15 {
            break;
        }
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn t_distribution_reference_values() {
        // Two-sided critical values: P(|T| > 2.228) = 0.05 for df = 10,
        // P(|T| > 1.96) -> 0.05 as df -> infinity.
        assert!((2.0 * student_t_sf(2.228_138_851_986_522, 10.0) - 0.05).abs() < 1e-6);
        assert!((2.0 * student_t_sf(1.959_963_984_540_054, 1e7) - 0.05).abs() < 1e-5);
        assert!((student_t_sf(0.0, 5.0) - 0.5).abs() < 1e-12);
        // df = 1 is the Cauchy distribution: P(T > 1) = 0.25.
        assert!((student_t_sf(1.0, 1.0) - 0.25).abs() < 1e-10);
    }

    #[test]
    fn identical_systems_are_not_significant() {
        let a = vec![0.1, 0.5, 0.3, 0.9, 0.2];
        let t = paired(&a, &a, 2000, 1);
        assert_eq!(t.mean_diff, 0.0);
        assert!(t.p_randomization > 0.99);
        assert_eq!(t.p_t_test, 1.0);
    }

    #[test]
    fn consistent_improvement_is_significant() {
        let a: Vec<f64> = (0..50).map(|i| (i % 10) as f64 / 10.0).collect();
        let b: Vec<f64> = a
            .iter()
            .enumerate()
            .map(|(i, x)| x + 0.05 + 0.01 * (i % 3) as f64)
            .collect();
        let t = paired(&a, &b, 5000, 2);
        assert!(t.p_t_test < 1e-6);
        assert!(t.p_randomization < 0.001);
    }
}
